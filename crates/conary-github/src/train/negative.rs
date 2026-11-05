//! A change's negative (`docs/DESIGN.md` section 4.3, rule 2): its new
//! tests run on its base's code, which they have to fail. burrow's shared CI
//! runs it on every PR and attests what it found
//! (`docs/ci-attestation.md`, "Negative runs"); a train that asks for it
//! lets a change join only once that attestation holds for the revision
//! joining, and says the tests failed there, or that it adds no test to
//! code. The attestation is recorded as the revision's evidence, checked
//! once, and counted from the ledger after that.

use conary_core::Store;
use conary_core::ledger::Change;
use conary_core::op::{Body, CheckKind, Outcome, Repo};
use conary_core::{ChangeId, RevisionId};

use super::{Forge, TrainError, record};
use crate::api::Source;
use crate::attest::{Subject, attested};

/// Which check is a PR's negative run, and which runners its result counts
/// from.
#[derive(Debug, Clone, Default)]
pub struct Negatives {
    /// The check run of burrow's shared CI that runs a PR's new tests on
    /// its base's code: its caller's job, then `negative`.
    pub check: String,
    /// What the name of each self-hosted runner started fresh for one job
    /// starts with, as for the gate's checks ([`super::Settings::fresh`]).
    pub fresh: Option<String>,
}

/// Whether a result counts as a change's negative: its new tests failed on
/// its base's code, or it adds none to code.
fn counts(result: Outcome) -> bool {
    matches!(result, Outcome::Fail | Outcome::None)
}

/// Whether a revision's negative counts, from the ledger or, failing that,
/// from the latest run of the check on its head, checked and recorded: why
/// not, if it doesn't. Only GitHub not answering, or the ledger refusing
/// the record, is an error.
pub(super) fn negative(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    repo: &Repo,
    change: ChangeId,
    revision: RevisionId,
    n: &Negatives,
) -> Result<Result<(), String>, TrainError> {
    let c = store.ledger().change(change).expect("ingest recorded it");
    if counted(c, revision) {
        return Ok(Ok(()));
    }
    let r = c.revision(revision).expect("ingest recorded it");
    let (base, head) = (r.base.to_string(), r.head.to_string());
    let full = repo.to_string();
    // A rerun is a new run, with a higher id.
    let Some(run) = source
        .check_runs(&full, &head)?
        .into_iter()
        .filter(|run| run.name == n.check)
        .max_by_key(|run| run.id)
    else {
        return Ok(Err(format!("{} hasn't run on {head}", n.check)));
    };
    if run.status != "completed" {
        return Ok(Err(format!("{} hasn't finished on {head}", n.check)));
    }
    let subject = Subject::Negative {
        base: &base,
        head: &head,
    };
    let a = match attested(forge, source, repo, subject, &run, n.fresh.as_deref())? {
        Ok(a) => a,
        Err(why) => return Ok(Err(format!("{}: {why}", n.check))),
    };
    let result = a.result;
    record(
        store,
        Body::Evidence {
            change,
            revision,
            kind: CheckKind::Negative,
            check: run.name.clone(),
            result,
            source: run
                .html_url
                .clone()
                .unwrap_or_else(|| format!("check run {}", run.id)),
            attest: Some(a.attest),
        },
    )?;
    Ok(if counts(result) {
        Ok(())
    } else {
        Err(format!(
            "{}: its new tests pass on {base}, the code they'd test the change against",
            n.check
        ))
    })
}

/// Whether the ledger holds an attested negative for the revision that
/// counts.
fn counted(c: &Change, revision: RevisionId) -> bool {
    c.evidence.iter().any(|e| {
        e.revision == revision
            && e.kind == CheckKind::Negative
            && e.attest.is_some()
            && counts(e.result)
    })
}
