//! The check decision from prs.py. A suite which judged another diff is
//! neither green nor red for this PR. Cancelled work is pending until an
//! equivalent, completed workflow run replaces it, or annotations prove a
//! timeout. What nothing replaced is listed by the way GitHub runs it again,
//! for the stall's recovery ([`super::recover`]).

use std::collections::{BTreeMap, BTreeSet};

use crate::api::CheckRun;
use conary_core::policy::Paths;

use super::degraded::{ReadError, Reading, Reads};
use super::{Check, Ci, Place, Read, Recovery, Settings};

/// Workflow metadata from GitHub's suite/workflow-run reads. Missing metadata
/// is not evidence that a cancelled run was replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub path: Option<String>,
    /// The workflow run's own id, which its rerun is asked by.
    pub run: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suite {
    pub status: String,
    pub conclusion: Option<String>,
    pub app: String,
    pub workflow: Option<Workflow>,
}

pub(super) struct Result {
    pub checks: Vec<Check>,
    pub docs: bool,
    pub has_ci: bool,
    /// Effective producer runs, selected by the same scope/replacement rules.
    pub runs: Vec<CheckRun>,
    /// Cancelled runs nothing replaced, by the way GitHub runs each again.
    pub stopped: BTreeMap<Recovery, Vec<String>>,
    /// Runs that timed out, by the way GitHub runs each again.
    pub timed_out: BTreeMap<Recovery, Vec<String>>,
}

const LANE: &str = "lane into ";
const DOCS: &str = "docs only into ";
const WHOLE: &str = ".github/workflows/flake.yml";
const RETARGET: &str = ".github/workflows/flake-retarget.yml";

fn suite_id(run: &CheckRun) -> Option<u64> {
    run.check_suite.as_ref().map(|s| s.id)
}
fn suite<'a>(run: &CheckRun, suites: &'a BTreeMap<u64, Suite>) -> Option<&'a Suite> {
    suites.get(&suite_id(run)?)
}
fn flow(run: &CheckRun, suites: &BTreeMap<u64, Suite>) -> Option<String> {
    let s = suite(run, suites)?;
    let w = s.workflow.as_ref()?;
    let identity = if matches!(w.path.as_deref(), Some(WHOLE | RETARGET)) {
        WHOLE
    } else {
        &w.id
    };
    Some(format!("{}:{identity}", s.app))
}

fn split(name: &str) -> (&str, &str) {
    name.rsplit_once(" / ").unwrap_or(("", name))
}
fn judged<'a>(
    run: &'a CheckRun,
    suites: &BTreeMap<u64, Suite>,
) -> Option<(&'a str, Option<&'a str>)> {
    if suite(run, suites)?.app != "github-actions" {
        return None;
    }
    let job = split(&run.name).1;
    if let Some(base) = job
        .strip_prefix(LANE)
        .filter(|b| !b.is_empty() && !b.contains(' '))
    {
        return Some((base, None));
    }
    let (base, fork) = job.strip_prefix(DOCS)?.split_once(" from ")?;
    if base.is_empty()
        || base.contains(' ')
        || fork.len() != 40
        || !fork
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    Some((base, Some(fork)))
}
/// How GitHub runs a check run again, by its kind: a GitHub Actions run by
/// its workflow run's rerun, another App's by its check suite's re-request,
/// once the suite has finished, as a rerun needs. A run in no suite the
/// snapshot read, or an Actions run whose workflow run it didn't name, has
/// no way the flow knows.
fn rerun(run: &CheckRun, suites: &BTreeMap<u64, Suite>) -> Option<Recovery> {
    let s = suite(run, suites).filter(|s| s.status == "completed")?;
    match s.app.as_str() {
        "" => None,
        "github-actions" => s.workflow.as_ref()?.run.map(Recovery::Rerun),
        _ => suite_id(run).map(Recovery::Rerequest),
    }
}
fn cancelled(run: &CheckRun) -> bool {
    run.status == "completed" && run.conclusion.as_deref() == Some("cancelled")
}
fn ran(run: &CheckRun) -> bool {
    run.status == "completed" && !matches!(run.conclusion.as_deref(), Some("skipped" | "cancelled"))
}
fn gate(job: &str) -> bool {
    job == "check"
        || job.starts_with(DOCS)
        || {
            let fields: Vec<_> = job.split(' ').collect();
            matches!(fields.as_slice(), ["leg", k, "of", n] if k.len() == 1 && n.len() == 1 && matches!(*k, "1"|"2"|"3"|"4"|"5"|"6"|"7"|"8") && matches!(*n, "2"|"3"|"4"|"5"|"6"|"7"|"8"))
        }
}
fn same_work(a: &CheckRun, b: &CheckRun) -> bool {
    if a.name == b.name {
        return true;
    }
    let (ac, aj) = split(&a.name);
    let (bc, bj) = split(&b.name);
    ac == bc && (aj.starts_with(LANE) && bj.starts_with(LANE) || gate(aj) && gate(bj))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Replacement {
    Finished,
    Going,
    None,
}
fn replacement(
    c: &CheckRun,
    runs: &[CheckRun],
    suites: &BTreeMap<u64, Suite>,
    stale: &BTreeSet<u64>,
) -> Replacement {
    let (Some(mine), Some(workflow)) = (suite_id(c), flow(c, suites)) else {
        return Replacement::None;
    };
    let same: Vec<_> = runs
        .iter()
        .filter(|r| flow(r, suites).as_ref() == Some(&workflow))
        .collect();
    let others: Vec<_> = same
        .iter()
        .copied()
        .filter(|r| suite_id(r) != Some(mine))
        .collect();
    let mut gone = stale.clone();
    gone.extend(
        others
            .iter()
            .filter(|r| cancelled(r))
            .filter_map(|r| suite_id(r)),
    );
    let done: Vec<_> = others
        .iter()
        .copied()
        .filter(|r| {
            suite(r, suites).is_some_and(|s| s.status == "completed")
                && !suite_id(r).is_some_and(|id| gone.contains(&id))
                && ran(r)
        })
        .collect();
    let shared = same.iter().any(|r| {
        let (caller, job) = split(&r.name);
        caller == split(&c.name).0 && job.starts_with(LANE)
    });
    if done.iter().any(|r| {
        same_work(c, r)
            && (!shared
                || done.iter().any(|lane| {
                    suite_id(lane) == suite_id(r)
                        && split(&lane.name).0 == split(&r.name).0
                        && judged(lane, suites).is_some()
                }))
    }) {
        return Replacement::Finished;
    }
    if others
        .iter()
        .any(|r| suite(r, suites).is_some_and(|s| s.status != "completed"))
    {
        Replacement::Going
    } else {
        Replacement::None
    }
}

/// Where a run that hasn't finished stands: queued for a runner, or started
/// at the time GitHub gives. A start GitHub didn't give, or gave in a form
/// that doesn't read, leaves it unplaced, so the wait's own clock runs.
fn placed(run: &CheckRun) -> Place {
    let at = || {
        run.started_at
            .as_deref()
            .and_then(|t| super::reviews::timestamp(t).ok())
    };
    match run.status.as_str() {
        "queued" => Place::Queued(at()),
        "in_progress" => at().map_or(Place::Unplaced, Place::Started),
        _ => Place::Unplaced,
    }
}

/// When the stall clock of a wait on CI starts, from the checks it waits
/// on (`waiting-costs-nothing`). A check that's pending only behind the
/// others has no clock of its own, unless nothing else is pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Clock {
    /// The wait's own clock: some pending check isn't a run known to be
    /// queued or started.
    Wait,
    /// Every pending check is queued for a runner: no clock runs, and the
    /// queue time since the earliest time GitHub gave, when it gave one, is
    /// capacity.
    Queued(Option<u64>),
    /// Every pending check is queued or started, and the earliest started
    /// then: no earlier than that, however long it queued.
    Started(u64),
}

pub(super) fn clock(checks: &[Check]) -> Clock {
    let pending: Vec<_> = checks
        .iter()
        .filter(|c| c.state == Ci::Pending && c.run != Place::Behind)
        .map(|c| c.run)
        .collect();
    if pending.is_empty() || pending.contains(&Place::Unplaced) {
        return Clock::Wait;
    }
    let started = pending
        .iter()
        .filter_map(|r| match r {
            Place::Started(at) => Some(*at),
            _ => None,
        })
        .min();
    started.map_or_else(
        || {
            Clock::Queued(
                pending
                    .iter()
                    .filter_map(|r| match r {
                        Place::Queued(at) => *at,
                        _ => None,
                    })
                    .min(),
            )
        },
        Clock::Started,
    )
}

/// Evaluate one consistent set of check and suite reads. Annotation reads are
/// lazy: a replacement run clears an old cancellation without asking why the
/// old run stopped. API failure propagates, never guesses a timeout verdict.
/// A read it goes on past (the docs lane's fork point, a run's suite) is a
/// degraded read in `reads`, and what it was for is pending. Each read's
/// outcome is kept in `reads`, even when a later one ends it.
/// `documentation` is whether the machine's policy names every path of
/// `paths` documentation: the docs lane's word alone, a job anyone's PR
/// can name, never waives a proof.
#[allow(clippy::too_many_arguments)]
pub(super) fn checks(
    source: &dyn Read,
    repo: &str,
    head: &str,
    base: &str,
    paths: &Paths,
    documentation: bool,
    suites: &BTreeMap<u64, Suite>,
    settings: &Settings,
    reads: &mut Reads,
) -> std::result::Result<Result, ReadError> {
    let proofs = &settings.machine();
    let mut grouped: BTreeMap<(u64, String), CheckRun> = BTreeMap::new();
    let mut runs = Vec::new();
    let raw_runs = reads.ask(Reading::Checks, source.check_runs(repo, head))?;
    let mut has_ci = !raw_runs.is_empty()
        || suites
            .values()
            .any(|s| s.conclusion.as_deref() == Some("startup_failure"));
    for run in raw_runs {
        let Some(id) = suite_id(&run) else {
            runs.push(run);
            continue;
        };
        let key = (id, run.name.clone());
        if grouped.get(&key).is_none_or(|old| old.id < run.id) {
            grouped.insert(key, run);
        }
    }
    runs.extend(grouped.into_values());
    let said: Vec<_> = runs
        .iter()
        .filter_map(|r| judged(r, suites).map(|j| (r, j)))
        .collect();
    // A base branch that's gone has no fork point: that's GitHub's answer.
    let fork = if said.iter().any(|(_, (b, f))| *b == base && f.is_some()) {
        match source.branch(repo, base) {
            Ok(Some(tip)) => reads.take(Reading::Fork, source.merge_base(repo, &tip, head)),
            Ok(None) => {
                reads.answered(Reading::Fork);
                None
            }
            Err(e) => {
                reads.met(Reading::Fork, super::degraded::why(&e));
                None
            }
        }
    } else {
        None
    };
    let stale: BTreeSet<_> = said
        .iter()
        .filter(|(_, (b, f))| *b != base || f.is_some_and(|f| fork.as_deref() != Some(f)))
        .filter_map(|(r, _)| suite_id(r))
        .collect();
    let docs = documentation
        && said.iter().any(|(r, (_, f))| {
            f.is_some()
                && !suite_id(r).is_some_and(|id| stale.contains(&id))
                && r.status == "completed"
                && r.conclusion.as_deref() == Some("success")
        })
        && !paths.cut
        && !paths.files.iter().any(|p| {
            std::iter::once(p.path.as_str())
                .chain(p.from.as_deref())
                .any(|p| p == ".github" || p.starts_with(".github/"))
        });
    let ran: BTreeSet<_> = said
        .iter()
        .filter(|(r, (_, f))| {
            f.is_none()
                && !suite_id(r).is_some_and(|id| stale.contains(&id))
                && suite(r, suites).is_some_and(|s| s.status == "completed")
        })
        .filter_map(|(r, _)| flow(r, suites))
        .collect();
    let unrun: BTreeSet<_> = said
        .iter()
        .filter(|(r, _)| {
            suite_id(r).is_some_and(|id| stale.contains(&id))
                && !flow(r, suites).is_some_and(|w| ran.contains(&w))
        })
        .map(|(r, _)| {
            suite(r, suites)
                .and_then(|s| s.workflow.as_ref())
                .map_or("CI", |w| w.name.as_str())
        })
        .collect();
    let mut checks: Vec<_> = unrun
        .into_iter()
        .map(|w| Check {
            name: format!("{w} for {base}"),
            state: Ci::Pending,
            run: Place::Unplaced,
        })
        .collect();
    for s in suites
        .values()
        .filter(|s| s.conclusion.as_deref() == Some("startup_failure"))
    {
        checks.push(Check {
            name: format!(
                "{} (failed to start)",
                s.workflow
                    .as_ref()
                    .map_or("a workflow", |w| w.name.as_str())
            ),
            state: Ci::Red,
            run: Place::Unplaced,
        });
    }
    let mut effective = BTreeMap::<(Option<String>, String), CheckRun>::new();
    let mut stopped = BTreeMap::<Recovery, Vec<String>>::new();
    let mut timed_out = BTreeMap::<Recovery, Vec<String>>::new();
    for run in &runs {
        if suite_id(run).is_some_and(|id| stale.contains(&id)) {
            continue;
        }
        // GitHub's two reads disagree about a run whose suite the snapshot
        // doesn't hold: whether it's a workflow's, and so whether it was
        // replaced or its workflow is still running, can't be told.
        let unsuited = suite_id(run).is_some() && suite(run, suites).is_none();
        if unsuited {
            reads.met(
                if cancelled(run) {
                    Reading::Cancelled
                } else {
                    Reading::Checks
                },
                "a check run's suite wasn't in the snapshot".into(),
            );
        }
        let after = if cancelled(run) {
            replacement(run, &runs, suites, &stale)
        } else {
            Replacement::None
        };
        if after == Replacement::Finished {
            continue;
        }
        let key = (flow(run, suites), run.name.clone());
        if effective.get(&key).is_none_or(|old| old.id < run.id) {
            effective.insert(key, run.clone());
        }
        let mut name = run.name.clone();
        let mut place = Place::Unplaced;
        let state = if run.status != "completed" || after == Replacement::Going {
            if after == Replacement::None {
                place = placed(run);
            }
            Ci::Pending
        } else if cancelled(run) {
            if crate::api::overran(
                &reads.ask(Reading::Cancelled, source.annotations(repo, run.id))?,
            ) {
                if let Some(way) = rerun(run, suites) {
                    timed_out.entry(way).or_default().push(run.name.clone());
                }
                name.push_str(" (timed out)");
                Ci::Red
            } else {
                if let Some(way) = rerun(run, suites) {
                    stopped.entry(way).or_default().push(run.name.clone());
                }
                name.push_str(" (cancelled, to be run again)");
                Ci::Pending
            }
        } else if !matches!(
            run.conclusion.as_deref(),
            Some("success" | "neutral" | "skipped")
        ) {
            if run.conclusion.as_deref() == Some("timed_out")
                && let Some(way) = rerun(run, suites)
            {
                timed_out.entry(way).or_default().push(run.name.clone());
            }
            Ci::Red
        } else if unsuited {
            name.push_str(" (its suite unread)");
            Ci::Pending
        } else if flow(run, suites).is_some()
            && suite(run, suites).is_none_or(|s| s.status != "completed")
        {
            name = format!(
                "{} (still running)",
                suite(run, suites)
                    .and_then(|s| s.workflow.as_ref())
                    .map_or("CI", |w| w.name.as_str())
            );
            place = Place::Behind;
            Ci::Pending
        } else {
            Ci::Green
        };
        checks.push(Check {
            name,
            state,
            run: place,
        });
    }
    let spelling: BTreeMap<_, _> = if docs {
        BTreeMap::new()
    } else {
        proofs.iter().map(|p| (p.to_lowercase(), p)).collect()
    };
    let mut seen = BTreeSet::new();
    for status in reads.ask(Reading::Checks, source.statuses(repo, head))? {
        let name = status.context.to_lowercase();
        let train = name == crate::train::CONTEXT.to_ascii_lowercase();
        // Authenticate reserved train results before choosing the newest one.
        // A forged newer result must not hide the steward's blocking word.
        if train
            && !status.creator.as_ref().is_some_and(|u| {
                settings
                    .flow_authors
                    .iter()
                    .any(|s| u.login.eq_ignore_ascii_case(s))
            })
        {
            continue;
        }
        has_ci |= name != "review-settled" && !train && !proofs.contains(&name);
        if !seen.insert(name.clone()) || name == "review-settled" {
            continue;
        }
        if train && status.state != "failure" {
            continue;
        }
        let state = match status.state.as_str() {
            "success" => Ci::Green,
            "pending" | "expected" => Ci::Pending,
            _ => Ci::Red,
        };
        // A machine proof the flow says is pending waits for CI to finish.
        let place = if proofs.contains(&name) {
            Place::Behind
        } else {
            Place::Unplaced
        };
        checks.push(Check {
            name: spelling.get(&name).map_or(status.context, |p| (*p).clone()),
            state,
            run: place,
        });
    }
    for (lower, name) in spelling {
        if !checks.iter().any(|c| c.name.to_lowercase() == lower) {
            checks.push(Check {
                name: name.clone(),
                state: Ci::Pending,
                run: Place::Behind,
            });
        }
    }
    // Every cancelled run's suite was there to tell what became of it.
    reads.answered(Reading::Cancelled);
    Ok(Result {
        checks,
        docs,
        has_ci,
        runs: effective.into_values().collect(),
        stopped,
        timed_out,
    })
}
