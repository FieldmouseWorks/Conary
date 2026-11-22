//! The train's ledger observations are local inputs, separate from GitHub's
//! word on whether an isolated PR merges, and the step of a train repo's
//! train at the end of its pass ([`step`]).
use super::decide::Decision;
use super::pass::{Pass, Report};
use super::persist::Result;
use super::*;
use conary_core::Store;
use conary_core::ledger::State;
use conary_core::op::Repo;
#[derive(Debug, Clone, Default)]
pub struct TrainState {
    pub seat: Option<Seat>,
    pub held: Option<(String, String)>,
}
#[derive(Debug, Clone)]
pub struct Seat {
    pub head: String,
    pub ahead: u32,
    pub candidate: Option<Candidate>,
}
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub contains: bool,
    pub landing: bool,
    pub peers: Vec<u64>,
}
/// A permission the pass granted a PR of a train repository, for the step
/// at the pass's end to look at again ([`step`]).
pub(super) struct Granted {
    pub facts: Facts,
    pub step: Step,
}
pub(super) fn apply(f: &Facts, decision: Decision) -> Decision {
    let (stage, step, description) = decision;
    let by_train = f.settings.train && f.revision.base == f.default_branch;
    let ready = matches!(
        step,
        Step::Merge | Step::JoinTrain | Step::CloseAlreadyLanded
    );
    if f.queued_merge && f.open && (by_train || !ready) {
        return (
            stage,
            Step::DisableAutoMerge,
            "Take GitHub's queued merge off before proceeding".into(),
        );
    }
    if let Some(seat) = &f.train.seat {
        if !by_train || step != Step::JoinTrain {
            return (
                stage,
                Step::LeaveTrain,
                format!("{description}; leave the train while unsettled"),
            );
        }
        if seat.head == f.revision.head {
            let text = if let Some(candidate) = &seat.candidate {
                let id: String = candidate.id.chars().take(8).collect();
                if candidate.contains {
                    let peers = candidate
                        .peers
                        .iter()
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "In the train, {} in candidate {id}{}",
                        if candidate.landing {
                            "landing"
                        } else {
                            "gating"
                        },
                        if peers.is_empty() {
                            String::new()
                        } else {
                            format!(" with {peers}")
                        }
                    )
                } else {
                    format!(
                        "In the train, behind candidate {id}, {} ahead of it",
                        seat.ahead
                    )
                }
            } else if seat.ahead > 0 {
                format!("In the train, {} ahead of it", seat.ahead)
            } else {
                "In the train, next in line".into()
            };
            return (Stage::Train, Step::Wait { until: None }, text);
        }
    }
    if step == Step::JoinTrain
        && let Some((head, why)) = &f.train.held
        && head == &f.revision.head
    {
        return (
            Stage::Train,
            step,
            format!("Held out of the train: {why}; ask to join again"),
        );
    }
    (stage, step, description)
}

/// One step of the repo's train (`crate::train::step`), at the end of a pass
/// that saw to every open PR, under the repository's lock the pass holds:
/// stepping is landing, so it's the landing owner's, and two passes never
/// step at once. It's taken after the PRs, so this pass's joins are in it,
/// and the way the steward takes it: not while the repository is paused, and
/// not when the decision of a PR in the train failed this pass, as it may not
/// be settled any more. A train with nothing in it or in flight, whose last
/// candidate finished, has nothing to step, unless it holds a PR out as red
/// alone: only a step sees that PR's gate pass since.
///
/// The pass decided on each PR before it waited for the lock the step holds,
/// and another writer can hold that for long. So under the lock it looks
/// again at each permission it `granted` a PR that's seated, and doesn't
/// step when one no longer stands. A PR seated without one had its settled
/// status withdrawn by its decision, which the step reads before it lands it.
///
/// What a step changes under a PR (it landed, went red, was released,
/// refused or taken out) books that PR's next pass at once, since the train's own statuses
/// start none. A step that fails is said on the reports of the PRs in the
/// train, which books each again; with none to say it on, the pass fails.
pub(super) fn step<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    repo: &Repo,
    data: &Data,
    granted: &[Granted],
    store: &mut Store,
    reports: &mut [Report],
) -> Result<()> {
    // Who's seated, and so whose failed decision keeps it from stepping, is
    // read under the lock the step lands under.
    store.holding(|store| held(pass, github, repo, data, granted, store, reports))?
}

/// [`step`], under the ledger's lock.
fn held<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    repo: &Repo,
    data: &Data,
    granted: &[Granted],
    store: &mut Store,
    reports: &mut [Report],
) -> Result<()> {
    let ledger = store.ledger();
    let Some(train) = ledger.train(repo) else {
        return Ok(());
    };
    let finished = train.candidates().last().is_none_or(|id| {
        ledger
            .candidate(*id)
            .is_some_and(|c| matches!(c.state, State::Finished { .. }))
    });
    if train.joined().is_empty()
        && train.in_flight().is_none()
        && finished
        && ledger.red_alone(repo).is_empty()
    {
        return Ok(());
    }
    let seated: Vec<u64> = train
        .joined()
        .iter()
        .filter_map(|m| ledger.change(m.change).map(|c| c.pr))
        .collect();
    // As the steward: a PR in the train whose decision failed may not be
    // settled any more. One no longer open has no decision to fail.
    let unseen = |pr: &u64| reports.iter().any(|r| r.pr == *pr && r.error.is_some());
    let paused = || pause::read(pass.root, repo).map(|paused| paused.is_some());
    // A paused repository has no step to look again for.
    if seated.iter().any(unseen) || paused()? {
        return Ok(());
    }
    for Granted { facts, step } in granted {
        let pr = facts.number;
        if !seated.contains(&pr) {
            continue;
        }
        let looked = data.load(pr).and_then(|record| {
            super::execute::permission(pass, github, facts, step, &record, &mut Some(&mut *store))
        });
        if let Err(error) = looked {
            let mut said = format!("looking again before the train's step: {error}");
            if let Some(withdrawn) = error.downcast_ref::<super::degraded::Withdrawn>()
                && let Err(e) = super::degraded::record(store, repo, pr, &withdrawn.0, |_| false)
            {
                said = format!("{said}; recording it failed: {e}");
            }
            if let Some(report) = reports.iter_mut().find(|r| r.pr == pr) {
                report.error = Some(said);
            }
            return Ok(());
        }
    }
    // Read last before the step: it merges, which a pause keeps off.
    if paused()? {
        return Ok(());
    }
    let stepped = pass
        .config
        .evidence
        .as_ref()
        .ok_or_else(|| "the train needs a producer verifier (evidence) to step".into())
        .and_then(|evidence| {
            let settings = crate::train::Settings {
                size: pass.config.settings.train_size.get(),
                require: evidence.require.clone(),
                trust: evidence.trust()?,
            };
            let config = super::ledger::config(&pass.config.settings);
            Ok(crate::train::step(
                store, github, github, &config, repo, &settings,
            )?)
        });
    match stepped {
        Ok(stepped) => {
            let touched: Vec<u64> = (stepped.conflicts.iter().map(|c| c.pr))
                .chain(stepped.empty.iter().map(|e| e.pr))
                .chain(stepped.red.iter().map(|r| r.pr))
                .chain(stepped.refused.iter().map(|r| r.pr))
                .chain(stepped.landed.iter().copied())
                .chain(stepped.unready.iter().copied())
                .chain(stepped.reseated.iter().copied())
                .chain(stepped.released.iter().copied())
                .collect();
            for report in reports.iter_mut().filter(|r| touched.contains(&r.pr)) {
                if let Some(next) = &mut report.next {
                    next.wait_until = Some(pass.now.saturating_add(1));
                }
            }
            for left in &stepped.unread {
                if let Some(report) = reports.iter_mut().find(|r| r.pr == left.pr) {
                    report.error = Some(format!(
                        "reading whether the gate it was red alone in passed since: {}",
                        left.why
                    ));
                }
            }
            Ok(())
        }
        Err(error) => {
            let error: Box<dyn std::error::Error + Send + Sync> = error;
            let said = format!("stepping the train: {error}");
            let mut told = false;
            for report in reports.iter_mut().filter(|r| seated.contains(&r.pr)) {
                report.error = Some(said.clone());
                told = true;
            }
            if told { Ok(()) } else { Err(said.into()) }
        }
    }
}
