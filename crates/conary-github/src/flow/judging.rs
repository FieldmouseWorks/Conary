//! What a judge is asked to judge on: the review findings and a fix's
//! answers to them, as one digest that the flow and the judge producer
//! compute the same way.
use super::jobs::{self, Purpose};
use super::persist::Result;
use super::{Record, Revision};
use conary_core::plugin::{Answer, Finding, FindingAnswer, Problem, Response, Task, Trigger};

/// The sha256, in lowercase hex, of a discussion: each finding in order,
/// with the answer a fix gave it, if any. Only the finding's own fields and
/// the answer's `fixed` and `reply` count, never which invocation carried
/// them, so the same discussion always has the same digest.
pub fn judging_context<'a>(
    discussion: impl IntoIterator<Item = (&'a Finding, Option<&'a FindingAnswer>)>,
) -> String {
    // Structs, not maps: the key order is fixed by these declarations, never
    // by a JSON library's map feature.
    #[derive(serde::Serialize)]
    struct Entry<'a> {
        finding: &'a Finding,
        answer: Option<Answered<'a>>,
    }
    #[derive(serde::Serialize)]
    struct Answered<'a> {
        fixed: bool,
        reply: &'a str,
    }
    let entries: Vec<_> = discussion
        .into_iter()
        .map(|(finding, answer)| Entry {
            finding,
            answer: answer.map(|a| Answered {
                fixed: a.fixed,
                reply: &a.reply,
            }),
        })
        .collect();
    super::evidence::receipt_digest(&serde_json::to_vec(&entries).expect("a discussion is JSON"))
}

/// The PR's discussion so far, as a log in the order its jobs ran: each
/// validated review's findings, unanswered, then each v4 fix's findings as it
/// was handed them, with its answers. Nothing is matched across jobs: which
/// finding a thread or answer belongs to is the flow's thread identity, not
/// this log's.
pub(super) fn discussion(record: &Record) -> Result<Vec<(Finding, Option<FindingAnswer>)>> {
    let mut discussion = Vec::new();
    for work in &record.jobs {
        let Some(Ok(Response { answer, .. })) = jobs::inspect(work, jobs::now())?.result else {
            continue;
        };
        match (&work.purpose, answer) {
            (Purpose::Review(_), Answer::Review { findings }) => {
                discussion.extend(findings.into_iter().map(|f| (f, None)));
            }
            (
                Purpose::Fix(_),
                Answer::Fix {
                    answers: Some(answers),
                    ..
                },
            ) => {
                let Task::Fix { problems, .. } = invocation(&work.path)?.task else {
                    return Err("a fix job's invocation is not a fix".into());
                };
                for answer in answers {
                    let Some(Problem::Finding { finding }) = problems.get(answer.problem) else {
                        return Err("a fix answer names no finding it was handed".into());
                    };
                    discussion.push((finding.clone(), Some(answer)));
                }
            }
            _ => {}
        }
    }
    Ok(discussion)
}

/// The reasons a judge request carries: the decision's, or, when only a
/// judge already started on this revision holds, that judge's own.
pub(super) fn why(
    record: &Record,
    revision: &Revision,
    decided: &[Trigger],
) -> Result<Vec<Trigger>> {
    if !decided.is_empty() {
        return Ok(decided.to_vec());
    }
    let earlier = record
        .jobs
        .iter()
        .rev()
        .find(|w| matches!(w.purpose, Purpose::Judge) && &w.job.revision == revision)
        .ok_or("a judge needs a reason")?;
    let invocation = invocation(&earlier.path)?;
    match invocation.task {
        Task::Judge { why, .. } if !why.is_empty() => Ok(why),
        _ => Err("the earlier judge carries no reason".into()),
    }
}

fn invocation(path: &std::path::Path) -> Result<jobs::Invocation> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Job, JobState};
    use conary_core::plugin::Identity;

    fn revision(head: &str) -> Revision {
        Revision {
            head: head.into(),
            base: "base".into(),
            description: "d".into(),
        }
    }

    /// A record holding one judge job on `head`, asked for `asked`.
    fn judged(dir: &std::path::Path, head: &str, asked: Vec<Trigger>) -> Record {
        let path = dir.join("judge.request");
        let task = Task::Judge {
            repo: "o/r".into(),
            pr: 3,
            head: "1".repeat(40),
            against: "d".into(),
            author: Identity {
                engine: "e".into(),
                session: None,
            },
            reviewers: vec![],
            why: asked,
            context: "a".repeat(64),
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&jobs::Invocation {
                executable: "plugin".into(),
                task,
                choice: None,
                deadline: 0,
                response_bytes: 1,
            })
            .unwrap(),
        )
        .unwrap();
        Record {
            jobs: vec![jobs::Work {
                purpose: Purpose::Judge,
                job: Job {
                    id: "3-1".into(),
                    revision: revision(head),
                    state: JobState::Done,
                    started: 0,
                    repeated: false,
                },
                path,
                deadline: 0,
                posted: false,
                recipes: Default::default(),
            }],
            ..Record::default()
        }
    }

    #[test]
    fn the_context_digest_is_of_the_documented_encoding() {
        let finding = Finding {
            path: "src/lib.rs".into(),
            line: 1,
            severity: conary_core::plugin::Severity::P2,
            text: "t".into(),
        };
        let answer = FindingAnswer {
            problem: 4,
            fixed: false,
            reply: "r".into(),
        };
        let wire = concat!(
            r#"[{"finding":{"path":"src/lib.rs","line":1,"severity":"p2","text":"t"},"#,
            r#""answer":{"fixed":false,"reply":"r"}},"#,
            r#"{"finding":{"path":"src/lib.rs","line":1,"severity":"p2","text":"t"},"answer":null}]"#
        );
        assert_eq!(
            judging_context([(&finding, Some(&answer)), (&finding, None)]),
            super::super::evidence::receipt_digest(wire.as_bytes())
        );
        assert_eq!(
            judging_context([]),
            super::super::evidence::receipt_digest(b"[]")
        );
    }

    #[test]
    fn a_judge_asked_again_carries_the_reasons_it_was_first_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let record = judged(dir.path(), "h", vec![Trigger::NoCi]);
        // The decision's own reasons win when it has any.
        assert_eq!(
            why(&record, &revision("h"), &[Trigger::Asked]).unwrap(),
            [Trigger::Asked]
        );
        // Only the earlier judge holds: its reasons, never an empty list.
        assert_eq!(why(&record, &revision("h"), &[]).unwrap(), [Trigger::NoCi]);
        // Another revision's judge is no reason here.
        assert!(why(&record, &revision("other"), &[]).is_err());
        assert!(why(&Record::default(), &revision("h"), &[]).is_err());
        let record = judged(dir.path(), "h", vec![]);
        assert!(why(&record, &revision("h"), &[]).is_err());
    }
}
