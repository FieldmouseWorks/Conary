//! Permission is based on independently attested producer records, never
//! controller-posted assertions. Plugins remain a work protocol. The
//! reviewer's word is a gate, not evidence (DESIGN.md 4.3), so nothing here
//! asks for it: the flow's decision waits for it, and `scope::current`
//! refuses permission when it changed since.
use super::persist::Result;
use super::*;
use crate::attest::service::Producers;
use crate::attest::{Signer, Subject, Trust, attested, skipped};
use conary_core::op::Outcome;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Whose attestations count as evidence, from the machine's configuration.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// The CI workflow whose attestations count, as `OWNER/REPO/PATH`.
    #[schemars(extend("x-spender" = false))]
    pub signer: String,
    /// The prefix of runners started for one job, whose results count beside
    /// GitHub-hosted ones. `null` accepts GitHub-hosted jobs only.
    #[schemars(extend("x-spender" = false))]
    pub fresh: Option<String>,
    /// The checks whose attested results a landing needs, on the tree that
    /// lands: the head's, for a PR that lands itself, and the candidate's,
    /// as the train's gate asks them, for one that lands through the train.
    #[schemars(extend("x-spender" = false))]
    pub require: Vec<String>,
    /// The check whose attested negative run a new test needs.
    #[schemars(extend("x-spender" = false))]
    pub negative: String,
    /// The machine's producer keys file (`docs/service-attestation.md`):
    /// the services whose signed results count. None trusts no service.
    #[serde(default)]
    #[schemars(extend("x-spender" = false))]
    pub producers: Option<std::path::PathBuf>,
}
impl Evidence {
    /// Whose attestations count, read from the machine's configuration.
    /// A producer keys file that's named and doesn't read fails closed.
    pub(crate) fn trust(&self) -> Result<Trust> {
        Ok(Trust {
            signer: self.signer.parse::<Signer>()?,
            fresh: self.fresh.clone(),
            producers: match &self.producers {
                Some(path) => Producers::load(path)?,
                None => Producers::default(),
            },
        })
    }
    pub(crate) fn negatives(&self) -> Result<crate::train::Negatives> {
        self.negatives_under(self.trust()?)
    }
    fn negatives_under(&self, trust: Trust) -> Result<crate::train::Negatives> {
        if self.negative.trim().is_empty() {
            return Err("negative check is not configured".into());
        }
        Ok(crate::train::Negatives {
            check: self.negative.clone(),
            trust,
        })
    }
}
pub fn judgment_receipt(
    revision: &Revision,
    generation: u64,
    verdict: Option<Judgment>,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"kind":"judgment","revision":revision,
        "generation":generation,"verdict":verdict}))
    .expect("receipt")
}
/// A judgment's receipt: the legacy one, plus the issuance's provenance when
/// it has one and, for a v3 issuance, the `why` and `context` it was judged
/// under, so its signed log binds exactly what the sealed issuance says.
pub fn judgment_receipt_by(
    revision: &Revision,
    generation: u64,
    verdict: Option<Judgment>,
    provenance: Option<&conary_core::plugin::Provenance>,
    basis: Option<&Basis>,
) -> Vec<u8> {
    let mut receipt = serde_json::json!({"kind":"judgment","revision":revision,
        "generation":generation,"verdict":verdict});
    let fields = receipt.as_object_mut().expect("a receipt is an object");
    if let Some(provenance) = provenance {
        fields.insert("provenance".into(), serde_json::json!(provenance));
    }
    if let Some(basis) = basis {
        fields.insert("why".into(), serde_json::json!(basis.why));
        fields.insert("context".into(), serde_json::json!(basis.context));
    }
    serde_json::to_vec(&receipt).expect("receipt")
}
pub fn major_receipt(revision: &Revision, generation: u64, verdict: Option<Major>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"kind":"major","revision":revision,
        "generation":generation,"verdict":verdict}))
    .expect("receipt")
}
/// A proof's definition, as a service's statement names it
/// (`docs/service-attestation.md`): the SHA-256 of its recipe's JSON.
pub fn recipe_digest(recipe: &Recipe) -> String {
    receipt_digest(&serde_json::to_vec(recipe).expect("a recipe is JSON"))
}
pub fn receipt_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A lookup of a check's attested result that couldn't tell whether it
/// holds: GitHub didn't answer a read it needed. It's a degraded path
/// ([`super::degraded`]), never a pass.
#[derive(Debug)]
pub(crate) struct CantTell(pub super::Degradation);
impl std::fmt::Display for CantTell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Degraded: {}", self.0)
    }
}
impl std::error::Error for CantTell {}
/// A read `check` needed that GitHub didn't answer is a lookup that
/// couldn't tell; any other error is the lookup's own answer.
fn told<T>(check: &str, read: Result<T>) -> Result<T> {
    read.map_err(|e| {
        if e.is::<crate::ApiError>() {
            Box::new(CantTell(super::Degradation {
                what: super::Degraded::Lookup {
                    check: check.to_owned(),
                },
                why: super::degraded::why(&*e),
            })) as super::persist::Error
        } else {
            e
        }
    })
}
/// Whether every attested result a landing needs holds. Each check whose
/// lookup told yes or no, rather than couldn't tell, goes in `looked`.
/// Whether `grant` puts the head in the merge train, rather than landing
/// it: a train PR's join, or its seated wait. The train lands only a
/// candidate whose own gate passed `evidence.require` attested, on the tree
/// that lands (`train::step`), so its head isn't asked them: a repo's gate
/// may run only on the candidate. A merge or settlement lands the head
/// itself and is always asked them, on the head.
fn joins_train(grant: &Step, f: &Facts) -> bool {
    super::decide::by_train(f) && matches!(grant, Step::JoinTrain | Step::Wait { .. })
}

/// Whether the facts permit `grant`, one of the steps `pass::grants` names.
pub(crate) fn authorize<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    f: &Facts,
    grant: &Step,
    looked: &mut std::collections::BTreeSet<String>,
) -> Result<()> {
    if super::approval::required(f) && !super::approval::approved(pass, github, f)? {
        return Err("An authorized person's scoped approval is required".into());
    }
    let evidence = pass
        .config
        .evidence
        .as_ref()
        .ok_or("producer attestation verifier is not configured")?;
    if evidence.require.is_empty() {
        return Err("no required attested gate is configured".into());
    }
    let repo = f.repo.to_string();
    let receipts: std::collections::BTreeSet<String> =
        ["conary/judgment".into(), "conary/major".into()].into();
    if evidence
        .require
        .iter()
        .any(|name| receipts.contains(name) || name == &evidence.negative)
    {
        return Err("required gate collides with producer receipt or negative evidence".into());
    }
    if f.settings
        .proofs
        .keys()
        .any(|name| receipts.contains(name) || name == &evidence.negative)
    {
        return Err("proof check name collides with producer receipt or negative evidence".into());
    }
    let trust = evidence.trust()?;
    // Every check on the head must be green (`observations` holds that), but
    // producer attestation is asked only of what the machine names as
    // evidence: its required checks (unless the train asks them of its
    // candidate instead) and, unless the PR is docs-only, its proofs. Any
    // other green check is a PR job's, whose token names GitHub's merge
    // commit rather than the head, and a status can't attest.
    let effective = observations(github, f, &f.revision.head)?;
    let mut required = std::collections::BTreeSet::new();
    if !joins_train(grant, f) {
        required.extend(evidence.require.iter().cloned());
    }
    if !effective.docs {
        // Every effective spelling of a proof, since `check` reads runs by
        // exact name and GitHub's check names ignore case.
        for name in f.settings.proofs.keys() {
            let spellings: Vec<_> = effective
                .runs
                .iter()
                .filter(|r| r.name.eq_ignore_ascii_case(name))
                .map(|r| r.name.clone())
                .collect();
            if spellings.is_empty() {
                required.insert(name.clone());
            }
            required.extend(spellings);
        }
    }
    for name in required {
        check(github, f, &trust, &f.revision.head, &name, None, looked)?;
    }
    let negatives = evidence.negatives_under(trust.clone())?;
    let negative = &evidence.negative;
    let base = told(
        negative,
        github
            .merge_base(
                &repo,
                f.base_tip.as_deref().ok_or("negative base unknown")?,
                &f.revision.head,
            )
            .map_err(Into::into),
    )?;
    let verdict = told(
        negative,
        crate::train::verify_negative(github, github, &f.repo, &base, &f.revision.head, &negatives)
            .map_err(Into::into),
    )?;
    looked.insert(negative.clone());
    verdict.map_err(|why| format!("negative evidence: {why}"))?;

    let records = github.reviews(&repo, f.number)?;
    if super::reviews::changes_requested(&records) {
        return Err("Review changes were requested before permission was used".into());
    }
    if let Some(generation) = f.judgment_generation {
        // A conflict or unsupported tombstone cannot be made positive by a local result.
        if f.judgment.is_none() {
            return Err("judgment issuance is a tombstone".into());
        }
        check(
            github,
            f,
            &trust,
            &f.revision.head,
            "conary/judgment",
            Some(&judgment_receipt_by(
                &f.revision,
                generation,
                f.judgment.as_ref().map(|j| j.verdict),
                f.judgment.as_ref().and_then(|j| j.provenance.as_ref()),
                f.judgment.as_ref().and_then(|j| j.basis.as_ref()),
            )),
            looked,
        )?;
    }
    if let Some(generation) = f.major_generation {
        if f.major.is_none() {
            return Err("dependency issuance is a tombstone".into());
        }
        check(
            github,
            f,
            &trust,
            &f.revision.head,
            "conary/major",
            Some(&major_receipt(&f.revision, generation, f.major)),
            looked,
        )?;
    }
    Ok(())
}
fn check<G: Write>(
    github: &G,
    f: &Facts,
    trust: &Trust,
    head: &str,
    name: &str,
    receipt: Option<&[u8]>,
    looked: &mut std::collections::BTreeSet<String>,
) -> Result<()> {
    let answer = lookup(github, f, trust, head, name, receipt);
    if !answer.as_ref().is_err_and(|e| e.is::<CantTell>()) {
        looked.insert(name.to_owned());
    }
    answer
}
fn lookup<G: Write>(
    github: &G,
    f: &Facts,
    trust: &Trust,
    head: &str,
    name: &str,
    receipt: Option<&[u8]>,
) -> Result<()> {
    let repo = f.repo.to_string();
    let mut latest: Vec<_> = told(name, observations(github, f, head))?
        .runs
        .into_iter()
        .filter(|r| r.name == name)
        .collect();
    if receipt.is_some() {
        // A receipt is one producer issuance, not a conjunction of every
        // workflow that published this role in the past.
        latest = latest
            .into_iter()
            .max_by_key(|r| r.id)
            .into_iter()
            .collect();
    }
    if latest.is_empty() {
        return Err(format!("{name}: no attested check run (statuses cannot attest)").into());
    }
    let tree = told(name, github.tree(&repo, head).map_err(Into::into))?;
    // A proof's pass counts only for the recipe the machine selects now.
    let definition = f.settings.recipe(name).map(recipe_digest);
    for run in latest {
        if run.status != "completed" || run.conclusion.as_deref() != Some("success") {
            return Err(format!("{name}: producer check is not successful").into());
        }
        let verified = told(
            name,
            attested(
                github,
                github,
                &f.repo,
                Subject::Commit {
                    commit: head,
                    tree: &tree,
                    definition: definition.as_deref(),
                },
                &run,
                trust,
            )
            .map_err(Into::into),
        )?
        .map_err(|why| format!("{name}: {why}"))?;
        // A result counts here only whole: nothing covers what it skipped.
        let skipped = skipped(&verified.attest).map_err(|why| format!("{name}: {why}"))?;
        if !skipped.is_empty() {
            return Err(format!(
                "{name}: it skipped {}, so it says nothing about them",
                skipped.join(", ")
            )
            .into());
        }
        if verified.result != Outcome::Pass {
            return Err(format!("{name}: producer did not pass").into());
        }
        if let Some(receipt) = receipt
            && verified.log.as_deref() != Some(receipt_digest(receipt).as_str())
        {
            return Err(
                format!("{name}: signed producer log does not bind this exact result").into(),
            );
        }
    }
    Ok(())
}

fn observations<G: Write>(github: &G, f: &Facts, head: &str) -> Result<super::ci::Result> {
    let repo = f.repo.to_string();
    let snapshot = github.snapshot(&repo, f.number)?;
    if snapshot.revision != f.revision || !snapshot.checks_complete {
        return Err("producer check scope changed or is incomplete".into());
    }
    let paths = github.files(
        &repo,
        f.base_tip.as_deref().ok_or("check base unknown")?,
        head,
    )?;
    let observed = super::ci::checks(
        github,
        &repo,
        head,
        &f.revision.base,
        &paths,
        // The policy's word on these same paths: `read` classified the diff
        // from this base tip to this head.
        f.classified.docs,
        &snapshot.suites,
        &f.settings,
    )?;
    if head == f.revision.head {
        let checks: Vec<_> = observed
            .checks
            .iter()
            .cloned()
            .map(|mut c| {
                if let Some((name, _)) = f.settings.proof_recipe(&c.name) {
                    c.name.clone_from(name);
                }
                c
            })
            .collect();
        if checks != f.checks || checks.iter().any(|c| c.state != Ci::Green) {
            return Err("Blocking or changed CI observations before permission".into());
        }
    }
    Ok(observed)
}
