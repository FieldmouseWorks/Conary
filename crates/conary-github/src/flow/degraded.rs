//! Degraded outcomes (`docs/DESIGN.md` section 4.6, `docs/flow.md`
//! "Degraded paths"): a path that half-worked and went on without its
//! answer. A pass collects each one it meets, names it in the PR's
//! decision, counts it as its check's failure, and appends it to the ledger
//! once while it lasts; when the same path works again for the same
//! revision, it appends the op that clears it.
use super::persist::Result;
use conary_core::Store;
use conary_core::op::{Body, DEGRADED_WHY, Repo};
use serde::{Deserialize, Serialize};

pub use conary_core::op::Degraded;

/// One degraded path, as a decision names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Degradation {
    #[serde(flatten)]
    pub what: Degraded,
    /// What failed, by its kind ([`why`]).
    pub why: String,
}

impl std::fmt::Display for Degradation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.what, self.why)
    }
}

/// What failed, from the error's kind alone: never the text a file, a
/// plugin or GitHub put in it, since the ledger keeps it for good.
pub(crate) fn why(error: &(dyn std::error::Error + 'static)) -> String {
    let error = match error.downcast_ref::<super::jobs::Unreadable>() {
        Some(e) => &*e.0,
        None => error,
    };
    let why = if let Some(e) = error.downcast_ref::<serde_json::Error>() {
        format!(
            "it isn't what it should hold: {:?} error at line {}, column {}",
            e.classify(),
            e.line(),
            e.column()
        )
    } else if let Some(e) = error.downcast_ref::<std::io::Error>() {
        format!("it can't be read: {:?}", e.kind())
    } else if let Some(e) = error.downcast_ref::<crate::ApiError>() {
        if e.unavailable {
            "GitHub couldn't be asked just now".into()
        } else {
            "GitHub refused the read".into()
        }
    } else {
        "the read failed".into()
    };
    let mut end = why.len().min(DEGRADED_WHY);
    while !why.is_char_boundary(end) {
        end -= 1;
    }
    why[..end].to_owned()
}

/// Bring the ledger in line with what this pass met of one PR's degraded
/// paths: each path `seen` that the ledger doesn't hold degraded for the
/// change's current revision is recorded, and each one it holds that `ran`
/// says this pass took again, and that wasn't seen, is cleared. A PR the
/// ledger hasn't seen yet has nothing to record against: its decision still
/// names what degraded.
pub(crate) fn record(
    store: &mut Store,
    repo: &Repo,
    pr: u64,
    seen: &[Degradation],
    ran: impl Fn(&Degraded) -> bool,
) -> Result<()> {
    let Some(change) = store.ledger().by_pr(repo, pr) else {
        return Ok(());
    };
    let Some(revision) = change.current else {
        return Ok(());
    };
    let id = change.id;
    let active: Vec<Degraded> = change.degraded(revision).map(|d| d.what.clone()).collect();
    for d in seen {
        if !active.contains(&d.what) {
            store.record(Body::Degraded {
                change: id,
                revision,
                what: d.what.clone(),
                why: d.why.clone(),
            })?;
        }
    }
    for what in active {
        if ran(&what) && !seen.iter().any(|d| d.what == what) {
            store.record(Body::Restored {
                change: id,
                revision,
                what,
            })?;
        }
    }
    Ok(())
}

/// The degradations the ledger holds for the PR's current revision that
/// this pass didn't take again, so a decision still names them.
pub(crate) fn held(ledger: Option<&conary_core::Ledger>, repo: &Repo, pr: u64) -> Vec<Degradation> {
    let Some(change) = ledger.and_then(|l| l.by_pr(repo, pr)) else {
        return Vec::new();
    };
    let Some(revision) = change.current else {
        return Vec::new();
    };
    change
        .degraded(revision)
        .map(|d| Degradation {
            what: d.what.clone(),
            why: d.why.clone(),
        })
        .collect()
}

/// The decision for a PR whose record didn't read: it waits, naming what
/// degraded. Nothing in the record counts, and nothing acts for it.
pub(crate) fn waiting(
    found: &[Degradation],
    priority: super::Level,
    ledger: Option<&conary_core::Ledger>,
    repo: &Repo,
    pr: u64,
) -> super::Next {
    let mut next = super::Next {
        stage: super::Stage::None,
        step: super::Step::Wait { until: None },
        description: String::new(),
        wait_until: None,
        overdue_at: None,
        proof: None,
        wait_on: None,
        on_usage: false,
        owner_notice: None,
        switches: Vec::new(),
        priority,
        degraded: Vec::new(),
    };
    name(
        &mut next,
        found,
        Vec::new(),
        &Default::default(),
        ledger,
        repo,
        pr,
    );
    next
}

/// Name every degraded path of the PR in its decision: what this pass met
/// reading its record and answers (`read`) and its evidence (`lookups`),
/// and what the ledger still holds for its revision that this pass didn't
/// look up again. What came through one counts as its check's failure: a
/// degraded record or answer keeps the PR waiting on it, whatever else it
/// would do but stop work, and a lookup that couldn't tell has already
/// kept its landing from being granted.
pub(crate) fn name(
    next: &mut super::Next,
    read: &[Degradation],
    lookups: Vec<Degradation>,
    looked: &std::collections::BTreeSet<String>,
    ledger: Option<&conary_core::Ledger>,
    repo: &Repo,
    pr: u64,
) {
    let mut named: Vec<Degradation> = read.iter().cloned().chain(lookups).collect();
    for d in held(ledger, repo, pr) {
        let again = matches!(&d.what, Degraded::Lookup { check } if looked.contains(check));
        if !again && !named.iter().any(|n| n.what == d.what) {
            named.push(d);
        }
    }
    if let Some(first) = read.first()
        && !matches!(next.step, super::Step::StopJob(_))
    {
        next.step = super::Step::Wait { until: None };
        next.proof = None;
        next.description = match read.len() {
            1 => format!("Degraded: {first}"),
            n => format!("Degraded: {first}, and {} more", n - 1),
        }
        .chars()
        .take(140)
        .collect();
    }
    next.degraded = named;
}
