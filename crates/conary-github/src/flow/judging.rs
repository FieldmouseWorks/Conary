//! What a judge is asked to judge on: the review findings and a fix's
//! answers to them, and every review thread, review body and marked PR
//! comment on GitHub, as one digest that the flow and the judge producer
//! compute the same way.
use std::collections::BTreeMap;

use super::jobs::{self, Purpose};
use super::persist::Result;
use super::{Basis, Record, ReviewThread, Revision, ThreadComment};
use conary_core::plugin::{Answer, Finding, FindingAnswer, Problem, Response, Task, Trigger};

/// The sha256, in lowercase hex, of what a judge reads: the flow's
/// discussion log, each finding in order with the answer a fix gave it, if
/// any; and every review thread on GitHub, by id, with its comments as they
/// read now. Only the finding's own fields and the answer's `fixed` and
/// `reply` count, never which invocation carried them, so the same
/// discussion always has the same digest. A thread counts by its comments'
/// authors and text, never by whether it's resolved: resolving a thread
/// adds nothing a judge hasn't read, and a finding or answer added or edited
/// on GitHub is another discussion. So does each of `reviews`, the review
/// bodies [`review_bodies`] keeps, by id, author and text; and each of
/// `conversation`, the PR comments [`conversation`] keeps, in order, by
/// author and text: a finding with no line in the diff, or an answer to
/// one, added, edited or removed.
pub fn judging_context<'a>(
    discussion: impl IntoIterator<Item = (&'a Finding, Option<&'a FindingAnswer>)>,
    threads: &BTreeMap<String, ReviewThread>,
    reviews: &BTreeMap<u64, ThreadComment>,
    conversation: &[ThreadComment],
) -> String {
    // Structs, not maps: the key order is fixed by these declarations, never
    // by a JSON library's map feature.
    #[derive(serde::Serialize)]
    struct Context<'a> {
        log: Vec<Entry<'a>>,
        threads: Vec<Thread<'a>>,
        reviews: Vec<Body<'a>>,
        conversation: &'a [ThreadComment],
    }
    #[derive(serde::Serialize)]
    struct Body<'a> {
        id: u64,
        author: Option<&'a str>,
        body: &'a str,
    }
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
    #[derive(serde::Serialize)]
    struct Thread<'a> {
        id: &'a str,
        comments: &'a [ThreadComment],
    }
    let log = discussion
        .into_iter()
        .map(|(finding, answer)| Entry {
            finding,
            answer: answer.map(|a| Answered {
                fixed: a.fixed,
                reply: &a.reply,
            }),
        })
        .collect();
    let threads = threads
        .iter()
        .map(|(id, t)| Thread {
            id,
            comments: &t.comments,
        })
        .collect();
    let reviews = reviews
        .iter()
        .map(|(id, r)| Body {
            id: *id,
            author: r.author.as_deref(),
            body: &r.body,
        })
        .collect();
    super::evidence::receipt_digest(
        &serde_json::to_vec(&Context {
            log,
            threads,
            reviews,
            conversation,
        })
        .expect("a discussion is JSON"),
    )
}

/// The review bodies a judge reads, by review id: every submitted review
/// whose body says something, findings and answers included. An approval is
/// a person's permission, not discussion: counting it would make the owner's
/// own release of an owner verdict ask for another judgment. A pending
/// review isn't published yet.
pub fn review_bodies(records: &[crate::api::Review]) -> BTreeMap<u64, ThreadComment> {
    records
        .iter()
        .filter(|r| {
            !matches!(r.state.as_deref(), Some("APPROVED" | "PENDING"))
                && r.body.as_deref().is_some_and(|b| !b.trim().is_empty())
        })
        .filter_map(|r| {
            Some((
                r.id?,
                ThreadComment {
                    author: r.user.as_ref().map(|u| u.login.clone()),
                    body: r.body.clone()?,
                },
            ))
        })
        .collect()
}

/// The PR comments a judge reads, in order: each that carries a finding's or
/// an answer's marker, whoever wrote it. The rest of the conversation, a
/// sealed verdict included, isn't the findings' discussion.
pub fn conversation(comments: &[crate::api::Comment]) -> Vec<ThreadComment> {
    comments
        .iter()
        .filter_map(|c| {
            let body = c.body.as_deref()?;
            super::threads::discussed(body).then(|| ThreadComment {
                author: c.user.as_ref().map(|u| u.login.clone()),
                body: body.into(),
            })
        })
        .collect()
}

/// The PR's discussion so far, as a log in the order its jobs ran: each
/// validated review's findings, unanswered, then each v4 fix's findings as it
/// was handed them, with its answers. Nothing is matched across jobs: which
/// finding a thread or answer belongs to is the flow's thread identity, not
/// this log's.
fn discussion(record: &Record, now: u64) -> Result<Vec<(Finding, Option<FindingAnswer>)>> {
    let mut discussion = Vec::new();
    for work in &record.jobs {
        let Some(Ok(Response { answer, .. })) = jobs::inspect(work, now)?.result else {
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

/// The PR's [`judging_context`] at `now`, over its log and what `f` read on
/// GitHub: what a judge asked now reads.
pub(super) fn context(record: &Record, f: &super::Facts, now: u64) -> Result<String> {
    let discussion = discussion(record, now)?;
    Ok(judging_context(
        discussion.iter().map(|(f, a)| (f, a.as_ref())),
        &f.threads,
        &f.review_bodies,
        &f.conversation,
    ))
}

/// What a judge job was asked to judge on, which the issuance answering it
/// seals: `None` for a request from before the flow said (an older
/// contract's, whose `context` is empty).
pub(super) fn asked(work: &jobs::Work) -> Result<Option<Basis>> {
    match invocation(&work.path)?.task {
        Task::Judge { context, .. } if context.is_empty() => Ok(None),
        Task::Judge { why, context, .. } => Ok(Some(Basis { why, context })),
        _ => Err("a judge job's invocation is not a judge".into()),
    }
}

/// The reasons a judge request carries: the decision's when it has any.
/// Otherwise only a judge already started on this head and base holds, as
/// `decide` keeps it owed, whatever the description now says: that judge's
/// own reasons, or `started` when it predates them. Never another head's or
/// base's.
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
        .find(|w| {
            matches!(w.purpose, Purpose::Judge)
                && w.job.revision.head == revision.head
                && w.job.revision.base == revision.base
        })
        .ok_or("a judge needs a reason")?;
    match invocation(&earlier.path)?.task {
        Task::Judge { why, .. } if why.is_empty() => Ok(vec![Trigger::Started]),
        Task::Judge { why, .. } => Ok(why),
        _ => Err("a judge job's invocation is not a judge".into()),
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
                handed: Vec::new(),
                sent: Default::default(),
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
        let threads = BTreeMap::from([(
            "T1".to_string(),
            ReviewThread {
                resolved: true,
                comments: vec![
                    ThreadComment {
                        author: Some("someone".into()),
                        body: "b".into(),
                    },
                    ThreadComment {
                        author: None,
                        body: "c".into(),
                    },
                ],
            },
        )]);
        let reviews = BTreeMap::from([(
            7,
            ThreadComment {
                author: Some("reviewer".into()),
                body: "a finding".into(),
            },
        )]);
        let wire = concat!(
            r#"{"log":[{"finding":{"path":"src/lib.rs","line":1,"severity":"p2","text":"t"},"#,
            r#""answer":{"fixed":false,"reply":"r"}},"#,
            r#"{"finding":{"path":"src/lib.rs","line":1,"severity":"p2","text":"t"},"answer":null}],"#,
            r#""threads":[{"id":"T1","comments":[{"author":"someone","body":"b"},"#,
            r#"{"author":null,"body":"c"}]}],"#,
            r#""reviews":[{"id":7,"author":"reviewer","body":"a finding"}],"#,
            r#""conversation":[{"author":"reviewer","body":"elsewhere"}]}"#
        );
        assert_eq!(
            judging_context(
                [(&finding, Some(&answer)), (&finding, None)],
                &threads,
                &reviews,
                &[ThreadComment {
                    author: Some("reviewer".into()),
                    body: "elsewhere".into(),
                }],
            ),
            super::super::evidence::receipt_digest(wire.as_bytes())
        );
        assert_eq!(
            judging_context([], &BTreeMap::new(), &BTreeMap::new(), &[]),
            super::super::evidence::receipt_digest(
                br#"{"log":[],"threads":[],"reviews":[],"conversation":[]}"#
            )
        );
    }

    #[test]
    fn a_review_body_counts_unless_it_is_an_approval_pending_or_blank() {
        let review = |id: u64, state: &str, body: &str| crate::api::Review {
            id: Some(id),
            state: Some(state.into()),
            user: Some(crate::api::User {
                login: "someone".into(),
                kind: "User".into(),
            }),
            body: Some(body.into()),
            commit_id: None,
            html_url: None,
            submitted_at: None,
        };
        let kept = review_bodies(&[
            review(1, "COMMENTED", "a finding"),
            review(2, "CHANGES_REQUESTED", "another"),
            review(3, "DISMISSED", "withdrawn, still read"),
            review(4, "APPROVED", "looks right"),
            review(5, "PENDING", "a draft"),
            review(6, "COMMENTED", "  \n"),
        ]);
        assert_eq!(kept.keys().copied().collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(kept[&1].author.as_deref(), Some("someone"));
        assert_eq!(kept[&1].body, "a finding");
    }

    #[test]
    fn a_thread_counts_by_its_comments_never_by_whether_it_is_resolved() {
        let comment = |body: &str| ThreadComment {
            author: Some("someone".into()),
            body: body.into(),
        };
        let threads = |resolved, comments: Vec<ThreadComment>| {
            BTreeMap::from([("T1".to_string(), ReviewThread { resolved, comments })])
        };
        let context = |t| judging_context([], &t, &BTreeMap::new(), &[]);
        let open = context(threads(false, vec![comment("a finding")]));
        // Resolving it adds nothing a judge hasn't read.
        assert_eq!(open, context(threads(true, vec![comment("a finding")])));
        // A new thread, an edited comment, an answer or another author is
        // another discussion.
        for other in [
            BTreeMap::new(),
            threads(false, vec![comment("an edited finding")]),
            threads(false, vec![comment("a finding"), comment("an answer")]),
            threads(
                false,
                vec![ThreadComment {
                    author: Some("another".into()),
                    body: "a finding".into(),
                }],
            ),
        ] {
            assert_ne!(open, context(other));
        }
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
        // A new description on the same head and base keeps them.
        let described = Revision {
            description: "another".into(),
            ..revision("h")
        };
        assert_eq!(why(&record, &described, &[]).unwrap(), [Trigger::NoCi]);
        // Another head's or base's judge is no reason here.
        assert!(why(&record, &revision("other"), &[]).is_err());
        let rebased = Revision {
            base: "other".into(),
            ..revision("h")
        };
        assert!(why(&record, &rebased, &[]).is_err());
        assert!(why(&Record::default(), &revision("h"), &[]).is_err());
        // A judge from before v4 recorded no reasons: it was started.
        let record = judged(dir.path(), "h", vec![]);
        assert_eq!(
            why(&record, &revision("h"), &[]).unwrap(),
            [Trigger::Started]
        );
    }
}
