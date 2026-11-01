//! A candidate's gate (`docs/DESIGN.md` section 4.7): the checks on its
//! commit, recorded as its evidence, and what they add up to. Any check's
//! failure is red, which needs no proof. Green rests on the required
//! checks, and each of their passes counts only with its producer's
//! attestation ([`crate::attest`]).

use std::collections::{BTreeMap, BTreeSet};

use conary_core::Store;
use conary_core::ledger::Candidate;
use conary_core::op::{Attest, Body, CheckKind};

use super::{Failed, Forge, Settings, TrainError, record};
use crate::api::{CheckRun, Source};
use crate::attest::attested;
use crate::ingest::{Config, run_result, status_result};

/// What a candidate's checks say so far.
pub(super) enum Gate {
    /// The checks it hasn't heard from yet, or whose passes aren't attested
    /// (and why): none named when no check has reported at all.
    Waiting(Vec<String>),
    Green,
    Red(Vec<Failed>),
}

/// A check's latest word on a commit.
enum Said<'a> {
    Pass(Option<&'a CheckRun>),
    Pending,
    Fail(Option<String>),
}

/// Records the CI results on the candidate's commit, as ingest records a
/// PR's, and says what they add up to. Red once any check's latest result
/// is a failure: a failed test has had its second try inside its run
/// (`docs/DESIGN.md` section 4.7). Green once every check has passed, the
/// required ones among them, each required one's latest run a success its
/// producer attests. A required status can't be: no status carries an
/// attestation. Waiting otherwise, with no check at all too, so a gate that
/// never starts, or never attests, is a stall, never a pass.
pub(super) fn gate(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    settings: &Settings,
    c: &Candidate,
) -> Result<Gate, TrainError> {
    let full = c.repo.to_string();
    let runs = source.check_runs(&full, c.commit.as_str())?;
    // Newest first from GitHub; recorded in the order they happened.
    let statuses: Vec<_> = source
        .statuses(&full, c.commit.as_str())?
        .into_iter()
        .filter(|s| config.is_check(&s.context))
        .collect();

    // Each check's latest word. A run gives way only to a rerun of it: a
    // later run (GitHub's ids grow) with its name in its suite, so runs of
    // one name from two workflows or Apps each count. A status gives way to
    // a later one in its context, and statuses and runs never stand in for
    // each other.
    let mut latest: BTreeMap<(Option<u64>, &str), &CheckRun> = BTreeMap::new();
    for run in &runs {
        let check = (run.check_suite.as_ref().map(|s| s.id), run.name.as_str());
        if latest.get(&check).is_none_or(|old| run.id > old.id) {
            latest.insert(check, run);
        }
    }
    let mut said: Vec<(&str, Said)> = latest
        .into_values()
        .map(|run| {
            let s = match (run.status.as_str(), run.conclusion.as_deref()) {
                ("completed", Some("success" | "neutral" | "skipped")) => Said::Pass(Some(run)),
                ("completed", _) => Said::Fail(run.html_url.clone()),
                _ => Said::Pending,
            };
            (run.name.as_str(), s)
        })
        .collect();
    // Newest first from GitHub: a context's first status is its latest.
    let mut contexts = BTreeSet::new();
    for status in &statuses {
        if contexts.insert(status.context.as_str()) {
            let s = match status.state.as_str() {
                "success" => Said::Pass(None),
                "pending" => Said::Pending,
                _ => Said::Fail(status.target_url.clone()),
            };
            said.push((&status.context, s));
        }
    }
    let failed: Vec<Failed> = said
        .iter()
        .filter_map(|(check, s)| match s {
            Said::Fail(url) => Some(Failed {
                check: (*check).to_owned(),
                url: url.clone(),
            }),
            _ => None,
        })
        .collect();

    // A red gate needs no attestation; a required check's pass does.
    let mut attests: BTreeMap<u64, Attest> = BTreeMap::new();
    let mut unattested = Vec::new();
    if failed.is_empty() {
        for (check, s) in &said {
            let Said::Pass(run) = s else { continue };
            if !settings.require.iter().any(|r| r == check) {
                continue;
            }
            let Some(run) = run else {
                unattested.push(format!("{check}: a status carries no attestation"));
                continue;
            };
            match attestation(forge, source, settings, c, run)? {
                Ok(attest) => {
                    attests.insert(run.id, attest);
                }
                Err(why) => unattested.push(format!("{check}: {why}")),
            }
        }
    }

    for run in &runs {
        let Some(f) = run_result(run) else { continue };
        record(
            store,
            Body::CandidateEvidence {
                candidate: c.id,
                kind: CheckKind::Ci,
                check: f.check,
                result: f.result,
                source: f.source,
                attest: attests.get(&run.id).cloned(),
            },
        )?;
    }
    for f in statuses.iter().rev().filter_map(status_result) {
        record(
            store,
            Body::CandidateEvidence {
                candidate: c.id,
                kind: CheckKind::Ci,
                check: f.check,
                result: f.result,
                source: f.source,
                attest: None,
            },
        )?;
    }

    if !failed.is_empty() {
        return Ok(Gate::Red(failed));
    }
    let mut waiting: Vec<String> = said
        .iter()
        .filter(|(_, s)| matches!(s, Said::Pending))
        .map(|(check, _)| (*check).to_owned())
        .collect();
    waiting.extend(
        settings
            .require
            .iter()
            .filter(|r| !said.iter().any(|(check, _)| check == r))
            .cloned(),
    );
    waiting.extend(unattested);
    Ok(if said.is_empty() || !waiting.is_empty() {
        Gate::Waiting(waiting)
    } else {
        Gate::Green
    })
}

/// A required check run's attestation: the one its evidence already holds,
/// checked when it was recorded, or one checked now.
fn attestation(
    forge: &dyn Forge,
    source: &dyn Source,
    settings: &Settings,
    c: &Candidate,
    run: &CheckRun,
) -> Result<Result<Attest, String>, TrainError> {
    if run.conclusion.as_deref() != Some("success") {
        return Ok(Err(format!(
            "it concluded {}, which attests nothing",
            run.conclusion.as_deref().unwrap_or("nothing")
        )));
    }
    // A job's link names it: no other run's evidence has it.
    let kept = run.html_url.as_ref().and_then(|url| {
        c.evidence
            .iter()
            .filter(|e| &e.source == url)
            .find_map(|e| e.attest.clone())
    });
    if let Some(attest) = kept {
        return Ok(Ok(attest));
    }
    Ok(attested(
        forge,
        source,
        &c.repo,
        c.commit.as_str(),
        c.tree.as_str(),
        run,
        settings.fresh.as_deref(),
    )?)
}
