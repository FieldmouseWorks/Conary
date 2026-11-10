//! A dry pass's decisions beside what the steward running the repo now said
//! of each PR: the stage its `waiting: …` label names and its
//! `review-settled` status. Read-only, for running the flow in the shadow
//! of the steward it replaces before any repository switches over.

use std::collections::BTreeSet;

use serde::Serialize;

use super::{Report, SETTLED, Stage, Step};
use crate::api::Source;
use conary_core::op::Repo;

/// One PR: the flow's decision, the steward's, and whether their stages
/// agree.
#[derive(Debug, Serialize)]
pub struct Beside {
    pub repo: Repo,
    pub pr: u64,
    /// The head both were about, as the PR shows it after the pass.
    pub head: Option<String>,
    pub agree: bool,
    pub flow: Option<Decided>,
    pub steward: Option<Said>,
    pub error: Option<String>,
}

/// What the flow decided.
#[derive(Debug, Serialize)]
pub struct Decided {
    pub stage: Stage,
    pub step: Step,
    pub description: String,
}

/// What the steward says of the PR on GitHub.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Said {
    /// The stages its labels name: one, or none for a draft or a held PR.
    pub stages: Vec<Stage>,
    /// Its newest `review-settled` status on the head, if it posted one.
    pub state: Option<String>,
    pub description: Option<String>,
}

/// How many of a repository's PRs agreed.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Tally {
    pub repo: Option<Repo>,
    pub prs: usize,
    pub agree: usize,
    pub disagree: usize,
    pub errors: usize,
}

/// Each dry-run report beside what the steward said of its PR. The steward's
/// status counts only from `authors` (its App's bot), or from anyone when
/// none are named. Disagreements come first, then errors, then agreements,
/// each by PR number.
pub fn beside<G: Source>(
    github: &G,
    reports: Vec<Report>,
    authors: &BTreeSet<String>,
) -> (Vec<Beside>, Tally) {
    let mut tally = Tally::default();
    let mut out: Vec<Beside> = reports
        .into_iter()
        .map(|report| {
            tally.repo.get_or_insert_with(|| report.repo.clone());
            tally.prs += 1;
            let flow = report.next.map(|next| Decided {
                stage: next.stage,
                step: next.step,
                description: next.description,
            });
            let read = said(github, &report.repo.to_string(), report.pr, authors);
            let (head, steward, read_error) = match read {
                Ok((head, said)) => (Some(head), Some(said), None),
                Err(e) => (None, None, Some(e)),
            };
            let error = report.error.or(read_error);
            let agree = error.is_none()
                && match (&flow, &steward) {
                    (Some(flow), Some(steward)) => stages(flow.stage) == steward.stages,
                    _ => false,
                };
            match (agree, &error) {
                (_, Some(_)) => tally.errors += 1,
                (true, None) => tally.agree += 1,
                (false, None) => tally.disagree += 1,
            }
            Beside {
                repo: report.repo,
                pr: report.pr,
                head,
                agree,
                flow,
                steward,
                error,
            }
        })
        .collect();
    out.sort_by_key(|b| {
        let rank = match (&b.error, b.agree) {
            (None, false) => 0,
            (Some(_), _) => 1,
            (None, true) => 2,
        };
        (rank, b.pr)
    });
    (out, tally)
}

/// The stages a decision's label would name: none for the null stage.
fn stages(stage: Stage) -> Vec<Stage> {
    stage.label().map(|_| stage).into_iter().collect()
}

fn said<G: Source>(
    github: &G,
    repo: &str,
    pr: u64,
    authors: &BTreeSet<String>,
) -> Result<(String, Said), String> {
    let pull = github
        .pull(repo, pr)
        .map_err(|e| format!("reading the steward's labels: {e}"))?;
    let stages = pull
        .labels
        .iter()
        .filter_map(|l| Stage::from_label(&l.name))
        .collect();
    let statuses = github
        .statuses(repo, &pull.head.sha)
        .map_err(|e| format!("reading the steward's status: {e}"))?;
    // GitHub lists a commit's statuses newest first.
    let settled = statuses.into_iter().find(|s| {
        s.context == SETTLED
            && (authors.is_empty()
                || s.creator
                    .as_ref()
                    .is_some_and(|c| authors.contains(&c.login)))
    });
    Ok((
        pull.head.sha,
        Said {
            stages,
            state: settled.as_ref().map(|s| s.state.clone()),
            description: settled.and_then(|s| s.description),
        },
    ))
}
