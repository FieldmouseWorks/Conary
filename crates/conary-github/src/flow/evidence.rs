//! Permission is based on independently attested producer records, never
//! controller-posted assertions. Plugin v1 remains a work protocol.
use super::persist::Result;
use super::*;
use crate::attest::{Signer, Subject, attested};
use conary_core::op::{Attest, Outcome};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub signer: String,
    pub fresh: Option<String>,
    pub require: Vec<String>,
    pub negative: String,
}
impl Evidence {
    pub(crate) fn negatives(&self) -> Result<crate::train::Negatives> {
        if self.negative.trim().is_empty() {
            return Err("negative check is not configured".into());
        }
        Ok(crate::train::Negatives {
            check: self.negative.clone(),
            signer: self.signer.parse::<Signer>()?,
            fresh: self.fresh.clone(),
        })
    }
}
/// Canonical producer log. Its digest must be in the signed commit predicate's
/// existing log.sha256 field. This does not extend the plugin wire contract.
pub fn review_receipt(head: &str, body: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"kind":"review","head":head,"body":body}))
        .expect("receipt")
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
pub fn major_receipt(revision: &Revision, generation: u64, verdict: Option<Major>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"kind":"major","revision":revision,
        "generation":generation,"verdict":verdict}))
    .expect("receipt")
}
pub fn receipt_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn authorize<G: Write>(pass: &Pass<'_>, github: &G, f: &Facts) -> Result<()> {
    let evidence = pass
        .config
        .evidence
        .as_ref()
        .ok_or("producer attestation verifier is not configured")?;
    if evidence.require.is_empty() {
        return Err("no required attested gate is configured".into());
    }
    let repo = f.repo.to_string();
    let mut receipts: std::collections::BTreeSet<String> = f
        .settings
        .reviewers
        .iter()
        .map(|role| format!("conary/review/{role}"))
        .collect();
    receipts.extend(["conary/judgment".into(), "conary/major".into()]);
    if f.settings
        .proofs
        .keys()
        .any(|name| receipts.contains(name) || name == &evidence.negative)
    {
        return Err("proof check name collides with producer receipt or negative evidence".into());
    }
    let mut required: std::collections::BTreeSet<_> = evidence.require.iter().cloned().collect();
    required.extend(
        f.checks
            .iter()
            .filter(|c| c.state == Ci::Green)
            .filter(|c| c.name != evidence.negative && !receipts.contains(&c.name))
            .map(|c| c.name.clone()),
    );
    required.extend(
        github
            .check_runs(&repo, &f.revision.head)?
            .into_iter()
            .filter(|r| r.name != evidence.negative && !receipts.contains(&r.name))
            .map(|r| r.name),
    );
    for name in required {
        check(github, f, evidence, &f.revision.head, &name, None)?;
    }
    let negatives = evidence.negatives()?;
    let base = github.merge_base(
        &repo,
        f.base_tip.as_deref().ok_or("negative base unknown")?,
        &f.revision.head,
    )?;
    crate::train::verify_negative(github, github, &f.repo, &base, &f.revision.head, &negatives)?
        .map_err(|why| format!("negative evidence: {why}"))?;

    let records = github.reviews(&repo, f.number)?;
    for (role, reviewer) in &f.reviewers {
        let Some(review) = &reviewer.review else {
            continue;
        };
        if review.revision != f.revision {
            continue;
        }
        let record = records
            .iter()
            .find(|r| r.id.is_some() && r.id == reviewer.record_id)
            .ok_or("review producer record disappeared")?;
        let head = record.commit_id.as_deref().ok_or("review head missing")?;
        let pull = github.pull(&repo, f.number)?;
        let mut carried = std::collections::BTreeSet::new();
        if head != f.revision.head {
            if !github.carries(
                &repo,
                f.base_tip.as_deref().ok_or("review base missing")?,
                &f.revision.head,
                head,
            )? {
                return Err("review no longer covers this tree".into());
            }
            carried.insert(head.to_owned());
        }
        let parsed = super::reviews::read(
            &pull,
            std::slice::from_ref(record),
            &Default::default(),
            &f.settings,
            &carried,
        )?;
        if parsed.get(role).and_then(|r| r.review.as_ref()) != Some(review) {
            return Err("review changed after the decision read".into());
        }

        let body = record.body.as_deref().ok_or("review body missing")?;
        check(
            github,
            f,
            evidence,
            head,
            &format!("conary/review/{role}"),
            Some(&review_receipt(head, body)),
        )?;
    }
    if let Some(generation) = f.judgment_generation {
        // A conflict or unsupported tombstone cannot be made positive by a local result.
        if f.judgment.is_none() {
            return Err("judgment issuance is a tombstone".into());
        }
        check(
            github,
            f,
            evidence,
            &f.revision.head,
            "conary/judgment",
            Some(&judgment_receipt(
                &f.revision,
                generation,
                f.judgment.as_ref().map(|j| j.verdict),
            )),
        )?;
    }
    if let Some(generation) = f.major_generation {
        if f.major.is_none() {
            return Err("dependency issuance is a tombstone".into());
        }
        check(
            github,
            f,
            evidence,
            &f.revision.head,
            "conary/major",
            Some(&major_receipt(&f.revision, generation, f.major)),
        )?;
    }
    Ok(())
}
fn check<G: Write>(
    github: &G,
    f: &Facts,
    evidence: &Evidence,
    head: &str,
    name: &str,
    receipt: Option<&[u8]>,
) -> Result<()> {
    let repo = f.repo.to_string();
    let runs = github.check_runs(&repo, head)?;
    let mut latest = std::collections::BTreeMap::new();
    for run in runs.into_iter().filter(|r| r.name == name) {
        let suite = run.check_suite.as_ref().map(|s| s.id);
        if latest
            .get(&suite)
            .is_none_or(|old: &crate::api::CheckRun| old.id < run.id)
        {
            latest.insert(suite, run);
        }
    }
    if latest.is_empty() {
        return Err(format!("{name}: no attested check run (statuses cannot attest)").into());
    }
    let tree = github.tree(&repo, head)?;
    for run in latest.into_values() {
        if run.status != "completed" || run.conclusion.as_deref() != Some("success") {
            return Err(format!("{name}: producer check is not successful").into());
        }
        let verified = attested(
            github,
            github,
            &f.repo,
            Subject::Commit {
                commit: head,
                tree: &tree,
            },
            &run,
            &evidence.signer.parse::<Signer>()?,
            evidence.fresh.as_deref(),
        )?
        .map_err(|why| format!("{name}: {why}"))?;
        if verified.result != Outcome::Pass {
            return Err(format!("{name}: producer did not pass").into());
        }
        if let Some(receipt) = receipt {
            let Attest::GithubToken { statement, .. } = verified.attest;
            let predicate: serde_json::Value =
                serde_json::from_str(statement.lines().nth(2).ok_or("missing signed predicate")?)?;
            if predicate["log"]["sha256"].as_str() != Some(receipt_digest(receipt).as_str()) {
                return Err(
                    format!("{name}: signed producer log does not bind this exact result").into(),
                );
            }
        }
    }
    Ok(())
}
