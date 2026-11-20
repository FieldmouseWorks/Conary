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

pub use conary_core::op::{Degraded, Reading};

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

/// What a decision's reads met: each read that failed or stopped short,
/// and each one that was taken and answered, which clears a degradation the
/// ledger holds for it ([`record`]'s `ran`). A read that was never taken
/// clears nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reads {
    pub met: Vec<Degradation>,
    pub answered: std::collections::BTreeSet<Degraded>,
}
impl Reads {
    /// `read` failed or stopped short, for the reason `why`.
    pub(crate) fn met(&mut self, read: Reading, why: String) {
        let what = Degraded::Read { read };
        if !self.met.iter().any(|d| d.what == what) {
            self.met.push(Degradation { what, why });
        }
    }
    /// `read` was taken and answered in full.
    pub(crate) fn answered(&mut self, read: Reading) {
        self.answered.insert(Degraded::Read { read });
    }
    /// `read`'s answer, if it gave one: a read whose failure the decision
    /// goes on past, holding what it was for failed.
    pub(crate) fn take<T, E: std::error::Error + 'static>(
        &mut self,
        read: Reading,
        answer: std::result::Result<T, E>,
    ) -> Option<T> {
        match answer {
            Ok(answer) => {
                self.answered(read);
                Some(answer)
            }
            Err(e) => {
                self.met(read, why(&e));
                None
            }
        }
    }
    /// `read`'s answer, or the decision's end: a read the decision can't go
    /// on without.
    pub(crate) fn ask<T>(
        &mut self,
        read: Reading,
        answer: std::result::Result<T, crate::ApiError>,
    ) -> std::result::Result<T, ReadError> {
        match answer {
            Ok(answer) => {
                self.answered(read);
                Ok(answer)
            }
            Err(error) => Err(ReadError::new(read, error)),
        }
    }
    /// Everything `other` met and answered, too.
    pub(crate) fn extend(&mut self, other: Self) {
        for d in other.met {
            if !self.met.iter().any(|m| m.what == d.what) {
                self.met.push(d);
            }
        }
        self.answered.extend(other.answered);
    }
}

/// A read the decision can't go on without that failed: what degraded, and
/// GitHub's error, which it shows as.
#[derive(Debug)]
pub struct ReadError {
    pub degradation: Degradation,
    pub error: crate::ApiError,
}
impl ReadError {
    pub(crate) fn new(read: Reading, error: crate::ApiError) -> Self {
        let why = why(&error);
        Self::with(read, why, error)
    }
    /// A read GitHub answered whose answer can't be used, for the fixed
    /// reason `why`.
    pub(crate) fn with(read: Reading, why: String, error: crate::ApiError) -> Self {
        Self {
            degradation: Degradation {
                what: Degraded::Read { read },
                why,
            },
            error,
        }
    }
}
impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
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
    again: impl Fn(&Degraded) -> bool,
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
    wait(&mut next, found);
    name(&mut next, found, Vec::new(), again, ledger, repo, pr);
    next
}

/// A degraded record or answer (`read`) counts as its check's failure: the
/// PR waits on it, at no stage, so nothing is granted (a wait at the train
/// stage with a seat would be), whatever else it would do but stop work.
pub(crate) fn wait(next: &mut super::Next, read: &[Degradation]) {
    let Some(first) = read.first() else {
        return;
    };
    if matches!(next.step, super::Step::StopJob(_)) {
        return;
    }
    next.stage = super::Stage::None;
    next.step = super::Step::Wait { until: None };
    next.wait_until = None;
    next.proof = None;
    next.wait_on = None;
    next.description = match read.len() {
        1 => format!("Degraded: {first}"),
        n => format!("Degraded: {first}, and {} more", n - 1),
    }
    .chars()
    .take(140)
    .collect();
}

/// Name every degraded path of the PR in its decision: what this pass met
/// reading its record and answers (`read`) and its evidence (`lookups`),
/// and what the ledger still holds for its revision that this pass didn't
/// take again (`again`, the same test [`record`] clears by, so a dry run
/// names what a writing pass would leave). A degraded record or answer has
/// already made the PR [`wait`], and a lookup that couldn't tell has already
/// kept its landing from being granted.
pub(crate) fn name(
    next: &mut super::Next,
    read: &[Degradation],
    lookups: Vec<Degradation>,
    again: impl Fn(&Degraded) -> bool,
    ledger: Option<&conary_core::Ledger>,
    repo: &Repo,
    pr: u64,
) {
    let mut named: Vec<Degradation> = read.iter().cloned().chain(lookups).collect();
    for d in held(ledger, repo, pr) {
        if !again(&d.what) && !named.iter().any(|n| n.what == d.what) {
            named.push(d);
        }
    }
    next.degraded = named;
}
