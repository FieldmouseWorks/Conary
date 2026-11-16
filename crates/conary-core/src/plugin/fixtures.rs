//! The conformance fixtures `cn plugin check` sends. Plugins must recognize
//! the reserved repository and answer deterministically, without contacting a
//! remote or running code.
use super::*;

/// A canned request, sent only to a plugin that speaks `contract` or later
/// and advertises its kind.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub contract: &'static str,
    pub task: Task,
}

/// The reserved repository every fixture names.
pub const REPOSITORY: &str = "conary-test/plugin-conformance";

/// Side-effect-free fixtures, in the order they're checked.
pub fn fixtures() -> Vec<Fixture> {
    let repo = REPOSITORY.to_owned();
    let base = "1".repeat(40);
    let head = "2".repeat(40);
    let author = Identity {
        engine: "conary-conformance-author".into(),
        session: Some("fixture-author".into()),
    };
    let fixture = |contract, task| Fixture { contract, task };
    vec![
        fixture(
            CONTRACT_V1,
            Task::Review {
                repo: repo.clone(),
                pr: 1,
                base: base.clone(),
                head: head.clone(),
                author: author.clone(),
            },
        ),
        fixture(
            CONTRACT_V1,
            Task::Fix {
                repo: repo.clone(),
                pr: 1,
                head: head.clone(),
                problems: vec![Problem::RedCheck {
                    check: "fixture".into(),
                    text: "No writable repository is supplied".into(),
                }],
            },
        ),
        fixture(
            CONTRACT,
            Task::Fix {
                repo: repo.clone(),
                pr: 1,
                head: head.clone(),
                problems: vec![Problem::Finding {
                    finding: Finding {
                        path: "src/lib.rs".into(),
                        line: 1,
                        severity: Severity::P2,
                        text: "fixture: answer without a change".into(),
                    },
                }],
            },
        ),
        fixture(
            CONTRACT_V1,
            Task::Judge {
                repo: repo.clone(),
                pr: 1,
                head: head.clone(),
                against: "fixture: accept".into(),
                author,
                reviewers: vec![],
                why: vec![Trigger::Asked],
                context: "3".repeat(64),
            },
        ),
        fixture(
            CONTRACT_V1,
            Task::Proof {
                repo: repo.clone(),
                head: head.clone(),
                tree: base.clone(),
                checks: vec!["fixture-pass".into(), "fixture-fail".into()],
            },
        ),
        fixture(
            CONTRACT_V2,
            Task::Job {
                repo: repo.clone(),
                workdir: ".".into(),
                prompt: "fixture: finish".into(),
                model: None,
                effort: None,
            },
        ),
        fixture(
            CONTRACT,
            Task::Prepare {
                repo,
                head,
                tree: base,
                checks: vec!["fixture-pass".into(), "fixture-fail".into()],
            },
        ),
    ]
}

impl Fixture {
    /// Whether a validated answer is the one this fixture asks for.
    pub fn conforms(&self, answer: &Answer) -> bool {
        match (&self.task, answer) {
            (Task::Review { .. }, Answer::Review { findings }) => findings.is_empty(),
            (Task::Fix { problems, .. }, Answer::Fix { result, answers }) => {
                if problems
                    .iter()
                    .any(|p| matches!(p, Problem::Finding { .. }))
                {
                    matches!(result, FixResult::Unchanged)
                        && answers.as_ref().is_some_and(|a| a.iter().all(|a| !a.fixed))
                } else {
                    matches!(result, FixResult::Unable { .. })
                }
            }
            (Task::Judge { .. }, Answer::Judge { verdict, .. }) => *verdict == Judgment::Pass,
            (Task::Proof { .. }, Answer::Proof { results, .. }) => results.iter().all(|r| {
                matches!(
                    (&*r.check, &r.verdict),
                    ("fixture-pass", Verdict::Pass) | ("fixture-fail", Verdict::Fail)
                )
            }),
            (Task::Job { .. }, Answer::Job { next, .. }) => *next == Next::Done,
            (Task::Prepare { .. }, Answer::Prepare { ready, slot, .. }) => *ready && !*slot,
            _ => false,
        }
    }
}
