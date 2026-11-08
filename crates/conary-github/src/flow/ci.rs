//! The check decision from prs.py. A suite which judged another diff is
//! neither green nor red for this PR. Cancelled work is pending until an
//! equivalent, completed workflow run replaces it, or annotations prove a timeout.

use std::collections::{BTreeMap, BTreeSet};

use crate::api::{ApiError, CheckRun};
use conary_core::policy::Paths;

use super::{Check, Ci, Read};

/// Workflow metadata from GitHub's suite/workflow-run reads. Missing metadata
/// is not evidence that a cancelled run was replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub path: Option<String>,
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

/// Evaluate one consistent set of check and suite reads. Annotation reads are
/// lazy: a replacement run clears an old cancellation without asking why the
/// old run stopped. API failure propagates, never guesses a timeout verdict.
pub(super) fn checks(
    source: &dyn Read,
    repo: &str,
    head: &str,
    base: &str,
    paths: &Paths,
    suites: &BTreeMap<u64, Suite>,
    proofs: &BTreeSet<String>,
) -> std::result::Result<Result, ApiError> {
    let mut grouped: BTreeMap<(u64, String), CheckRun> = BTreeMap::new();
    let mut runs = Vec::new();
    let raw_runs = source.check_runs(repo, head)?;
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
    let fork = if said.iter().any(|(_, (b, f))| *b == base && f.is_some()) {
        source
            .branch(repo, base)
            .ok()
            .flatten()
            .and_then(|tip| source.merge_base(repo, &tip, head).ok())
    } else {
        None
    };
    let stale: BTreeSet<_> = said
        .iter()
        .filter(|(_, (b, f))| *b != base || f.is_some_and(|f| fork.as_deref() != Some(f)))
        .filter_map(|(r, _)| suite_id(r))
        .collect();
    let docs = said.iter().any(|(r, (_, f))| {
        f.is_some()
            && !suite_id(r).is_some_and(|id| stale.contains(&id))
            && r.status == "completed"
            && r.conclusion.as_deref() == Some("success")
    }) && !paths.cut
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
        });
    }
    for run in &runs {
        if suite_id(run).is_some_and(|id| stale.contains(&id)) {
            continue;
        }
        let after = if cancelled(run) {
            replacement(run, &runs, suites, &stale)
        } else {
            Replacement::None
        };
        if after == Replacement::Finished {
            continue;
        }
        let mut name = run.name.clone();
        let state = if run.status != "completed" || after == Replacement::Going {
            Ci::Pending
        } else if cancelled(run) {
            if source
                .annotations(repo, run.id)?
                .iter()
                .any(|m| m.contains("has exceeded the maximum execution time"))
            {
                name.push_str(" (timed out)");
                Ci::Red
            } else {
                name.push_str(" (cancelled, to be run again)");
                Ci::Pending
            }
        } else if !matches!(
            run.conclusion.as_deref(),
            Some("success" | "neutral" | "skipped")
        ) {
            Ci::Red
        } else if flow(run, suites).is_some()
            && suite(run, suites).is_none_or(|s| s.status != "completed")
        {
            name = format!(
                "{} (still running)",
                suite(run, suites)
                    .and_then(|s| s.workflow.as_ref())
                    .map_or("CI", |w| w.name.as_str())
            );
            Ci::Pending
        } else {
            Ci::Green
        };
        checks.push(Check { name, state });
    }
    let spelling: BTreeMap<_, _> = if docs {
        BTreeMap::new()
    } else {
        proofs.iter().map(|p| (p.to_lowercase(), p)).collect()
    };
    let mut seen = BTreeSet::new();
    for status in source.statuses(repo, head)? {
        let name = status.context.to_lowercase();
        has_ci |= name != "review-settled"
            && !name.starts_with(crate::train::CONTEXT)
            && !proofs.contains(&name);
        if !seen.insert(name.clone()) || name == "review-settled" {
            continue;
        }
        if name.starts_with(crate::train::CONTEXT)
            && (status.state != "failure"
                || !status
                    .creator
                    .as_ref()
                    .is_some_and(|u| u.login == "fieldmouseworks-steward[bot]"))
        {
            continue;
        }
        let state = match status.state.as_str() {
            "success" => Ci::Green,
            "pending" | "expected" => Ci::Pending,
            _ => Ci::Red,
        };
        checks.push(Check {
            name: spelling.get(&name).map_or(status.context, |p| (*p).clone()),
            state,
        });
    }
    for (lower, name) in spelling {
        if !checks.iter().any(|c| c.name.to_lowercase() == lower) {
            checks.push(Check {
                name: name.clone(),
                state: Ci::Pending,
            });
        }
    }
    Ok(Result {
        checks,
        docs,
        has_ci,
    })
}
