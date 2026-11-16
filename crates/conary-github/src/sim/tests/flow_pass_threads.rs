//! Each finding the flow publishes gets its own target, and a fix's answers
//! go on exactly those targets: through the real executor, against the
//! simulated GitHub with no threads made in advance. The plugins' answers
//! are written as their workers would save them.
use super::flow_pass_case::{Case, REPO};
use super::{TRUNK, head};
use crate::api::Source;
use crate::branches::Branches;
use crate::flow::{self, *};
use crate::sim::{Call, STEWARD};
use conary_core::plugin::{
    self, Answer, Finding, FindingAnswer, FixResult, Identity, Problem, Severity, Task,
};
use std::collections::BTreeSet;

/// One hunk in `src/lib.rs`, new-side lines 1 to 5.
const DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1..2 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,5 @@
 a
+b
+c
+d
 e
";

fn finding(path: &str, line: u64, text: &str) -> Finding {
    Finding {
        path: path.into(),
        line,
        severity: Severity::P1,
        text: text.into(),
    }
}

/// Two findings on one line in the diff, a line outside its hunks, and a
/// file outside it.
pub(super) fn findings() -> Vec<Finding> {
    vec![
        finding("src/lib.rs", 2, "first on line two"),
        finding("src/lib.rs", 2, "second on line two"),
        finding("src/lib.rs", 40, "outside the hunks"),
        finding("README.md", 1, "outside the diff"),
    ]
}

pub(super) fn case(diff: bool) -> Case {
    let c = Case::new();
    if diff {
        let tip = c.hub.branch(REPO, "main").unwrap().unwrap();
        c.hub.flow_diff(&tip, &head(3), DIFF);
    }
    c
}

/// Answers every launched worker with `answer`, completed at `at`.
pub(super) fn respond(c: &Case, at: u64, answer: Answer) {
    for path in c.deferred.0.borrow_mut().drain(..) {
        let completion = flow::jobs::Completion {
            at,
            result: Some(Ok(plugin::Response {
                contract: plugin::CONTRACT.into(),
                by: Identity {
                    engine: "fixture".into(),
                    session: Some("fixture-session".into()),
                },
                execution: None,
                answer: answer.clone(),
            })),
        };
        std::fs::write(
            path.with_extension("answer"),
            serde_json::to_vec(&completion).unwrap(),
        )
        .unwrap();
    }
}

/// A fix that changes nothing and answers each finding it was handed.
pub(super) fn unchanged(c: &Case, at: u64, reply: &str) {
    let path = c.deferred.0.borrow()[0].clone();
    let invocation: flow::jobs::Invocation =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let Task::Fix { problems, .. } = invocation.task else {
        panic!("not a fix");
    };
    let answers = problems
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Problem::Finding { .. }))
        .map(|(problem, _)| FindingAnswer {
            problem,
            fixed: false,
            reply: format!("{reply} {problem}"),
        })
        .collect();
    respond(
        c,
        at,
        Answer::Fix {
            result: FixResult::Unchanged,
            answers: Some(answers),
        },
    );
}

fn report(c: &Case, now: u64) -> flow::Report {
    c.pass(now, false).remove(0)
}

pub(super) fn step(c: &Case, now: u64) -> Step {
    let r = report(c, now);
    assert_eq!(r.error, None);
    r.next.unwrap().step
}

/// A pass's step, whether or not its effect failed.
fn step_or_error(c: &Case, now: u64) -> Step {
    report(c, now).next.unwrap().step
}

/// Passes a minute apart from `*t` until one decides `want`.
pub(super) fn until(c: &Case, now: u64, t: &mut u64, want: Step) {
    for _ in 0..20 {
        *t += 60;
        if step(c, now + *t) == want {
            return;
        }
    }
    panic!("never reached {want:?}");
}

/// As [`until`], through passes whose effects fail while GitHub catches up.
fn until_waiting(c: &Case, now: u64, t: &mut u64, want: Step) {
    for _ in 0..40 {
        *t += 60;
        if step_or_error(c, now + *t) == want {
            return;
        }
    }
    panic!("never reached {want:?}");
}

/// The latest review the flow published, read back.
fn publication(c: &Case) -> Publication {
    let records = c.hub.reviews(REPO, 3).unwrap();
    records
        .iter()
        .rev()
        .find_map(|r| Publication::read(r, "reviewer", &c.config.settings))
        .unwrap()
}

pub(super) fn threads(c: &Case) -> std::collections::BTreeMap<String, ReviewThread> {
    flow::Read::snapshot(&c.hub, REPO, 3).unwrap().threads
}

fn targets(c: &Case) -> Targets {
    Targets::read(
        &threads(c),
        &c.hub.comments(REPO, 3).unwrap(),
        &c.config.settings,
    )
}

/// Every answer on GitHub: thread replies and PR comments.
fn answers(c: &Case) -> Vec<String> {
    let mut all: Vec<String> = threads(c)
        .into_values()
        .flat_map(|t| t.comments.into_iter().skip(1).map(|c| c.body))
        .collect();
    all.extend(
        c.hub
            .comments(REPO, 3)
            .unwrap()
            .into_iter()
            .filter_map(|c| c.body)
            .filter(|b| b.contains("<!-- conary-answer:v1 ")),
    );
    all
}

fn fix_work(c: &Case, id: &str) -> flow::jobs::Work {
    c.record()
        .jobs
        .into_iter()
        .find(|w| w.job.id == id)
        .unwrap()
}

/// Reviewed with [`findings`] at `now`, and published at `now + 1`.
pub(super) fn reviewed(c: &Case, now: u64) {
    assert_eq!(step(c, now), Step::QueueReview("reviewer".into()));
    respond(
        c,
        now,
        Answer::Review {
            findings: findings(),
        },
    );
    assert_eq!(step(c, now + 1), Step::PostReview("reviewer".into()));
}

#[test]
fn each_finding_gets_its_own_target_and_a_fix_answers_on_exactly_those() {
    let c = case(true);
    let now = flow::jobs::now();
    reviewed(&c, now);
    // One review. Its two findings in the diff became two threads on one
    // line; the other two became marked PR comments.
    assert_eq!(c.hub.reviews(REPO, 3).unwrap().len(), 1);
    let p = publication(&c);
    let t = targets(&c);
    let target = |i| t.target(&p.mark(i)).unwrap();
    let (Target::Thread(first), Target::Thread(second)) = (target(0), target(1)) else {
        panic!("findings in the diff have threads");
    };
    assert_ne!(first, second);
    assert_eq!(threads(&c).len(), 2);
    assert_eq!((target(2), target(3)), (Target::Comment, Target::Comment));
    assert_eq!(c.hub.comments(REPO, 3).unwrap().len(), 2);
    // Its threads are its findings, not blocking threads: a fix gets them,
    // each problem with its target.
    assert_eq!(step(&c, now + 2), Step::HandFix(Kind::Findings));
    let work = fix_work(&c, "3-2");
    assert_eq!(
        work.handed,
        (0..4)
            .map(|i| Some(Handed {
                mark: p.mark(i),
                target: target(i),
            }))
            .collect::<Vec<_>>()
    );
    unchanged(&c, now + 2, "kept because");
    // The first reply's answer is lost: it went up, and the retry reads it
    // back rather than replying twice.
    c.hub.lose(Call::FlowReply);
    let r = report(&c, now + 3);
    assert_eq!(r.next.unwrap().step, Step::PostFix("3-2".into()));
    assert!(r.error.is_some());
    c.hub.heal(Call::FlowReply);
    // The reviewers look again once every finding is answered.
    until(&c, now, &mut 3, Step::QueueReview("reviewer".into()));
    let all = threads(&c);
    for (i, id) in [(0, &first), (1, &second)] {
        let thread = &all[id];
        assert!(thread.resolved);
        assert_eq!(thread.comments.len(), 2, "one reply on thread {i}");
        assert_eq!(
            thread.comments[1].body,
            answer_body(&format!("kept because {i}"), &p.mark(i), "3-2")
        );
        assert_eq!(thread.comments[1].author.as_deref(), Some(STEWARD));
    }
    let comments: Vec<String> = c
        .hub
        .comments(REPO, 3)
        .unwrap()
        .into_iter()
        .filter_map(|c| c.body)
        .collect();
    for i in [2, 3] {
        let body = answer_body(&format!("kept because {i}"), &p.mark(i), "3-2");
        assert_eq!(comments.iter().filter(|c| **c == body).count(), 1);
    }
    assert_eq!(answers(&c).len(), 4);
    assert_eq!(c.hub.pull(REPO, 3).unwrap().head.sha, head(3));
}

#[test]
fn a_second_review_on_the_same_head_is_answered_only_by_its_own_answers() {
    // With no diff every finding is a PR comment; with one, two are threads.
    for diff in [false, true] {
        let c = case(diff);
        let now = flow::jobs::now();
        reviewed(&c, now);
        let first = publication(&c);
        assert_eq!(step(&c, now + 2), Step::HandFix(Kind::Findings));
        unchanged(&c, now + 2, "first answer");
        let mut t = 2;
        until(&c, now, &mut t, Step::QueueReview("reviewer".into()));
        // The same findings again, on the same head, found later.
        respond(
            &c,
            now + t,
            Answer::Review {
                findings: findings(),
            },
        );
        until(&c, now, &mut t, Step::PostReview("reviewer".into()));
        let second = publication(&c);
        assert_ne!(first.issuance, second.issuance);
        // The first review's answers say nothing of the second's findings.
        let read = targets(&c);
        for i in 0..4 {
            assert!(read.answered(&first.mark(i)));
            assert!(!read.answered(&second.mark(i)), "finding {i}, diff {diff}");
            assert_ne!(read.target(&second.mark(i)), None);
        }
        until(&c, now, &mut t, Step::HandFix(Kind::Findings));
        let handed: Vec<_> = fix_work(&c, "3-4")
            .handed
            .into_iter()
            .map(|h| h.unwrap().mark)
            .collect();
        assert_eq!(
            handed,
            (0..4).map(|i| second.mark(i)).collect::<Vec<_>>(),
            "diff {diff}"
        );
    }
}

#[test]
fn a_lost_or_partial_publication_is_completed_without_duplicates() {
    let c = case(true);
    let now = flow::jobs::now();
    assert_eq!(step(&c, now), Step::QueueReview("reviewer".into()));
    respond(
        &c,
        now,
        Answer::Review {
            findings: findings(),
        },
    );
    // The review goes up but its answer is lost; then the first PR comment
    // goes up and its answer is lost too.
    c.hub.lose(Call::FlowReview);
    let r = report(&c, now + 1);
    assert_eq!(r.next.unwrap().step, Step::PostReview("reviewer".into()));
    assert!(r.error.is_some());
    c.hub.heal(Call::FlowReview);
    c.hub.lose(Call::FlowComment);
    let mut t = 1;
    loop {
        t += 60;
        let r = report(&c, now + t);
        let next = r.next.unwrap().step;
        // Nothing is handed on before every target is on GitHub.
        assert_ne!(next, Step::HandFix(Kind::Findings));
        if next == Step::PostReview("reviewer".into()) {
            assert!(r.error.is_some());
            break;
        }
        assert!(t < 1200);
    }
    assert_eq!(c.hub.comments(REPO, 3).unwrap().len(), 1);
    c.hub.heal(Call::FlowComment);
    // The last PR comment goes up, but reading the conversation lags: what
    // isn't read back isn't published, and nothing is handed on.
    c.hub.stale(Call::Comments);
    loop {
        t += 60;
        let r = report(&c, now + t);
        assert_ne!(r.next.unwrap().step, Step::HandFix(Kind::Findings));
        if c.hub.count(Call::FlowComment) == 2 {
            assert!(r.error.is_some());
            break;
        }
        assert!(t < 2400);
    }
    c.hub.fresh(Call::Comments);
    // Had that counted as complete, the fix would be handed now.
    assert_ne!(
        step_or_error(&c, now + t + 1),
        Step::HandFix(Kind::Findings)
    );
    until(&c, now, &mut t, Step::HandFix(Kind::Findings));
    assert_eq!(c.hub.count(Call::FlowComment), 2);
    assert_eq!(c.hub.reviews(REPO, 3).unwrap().len(), 1);
    assert_eq!(c.hub.count(Call::FlowReview), 1);
    assert_eq!(threads(&c).len(), 2);
    assert_eq!(c.hub.comments(REPO, 3).unwrap().len(), 2);
    let p = publication(&c);
    let read = targets(&c);
    let all: BTreeSet<_> = (0..4)
        .map(|i| format!("{:?}", read.target(&p.mark(i)).unwrap()))
        .collect();
    assert_eq!(all.len(), 3, "two threads and the conversation");
}

#[test]
fn a_reply_goes_only_on_its_own_findings_thread() {
    let c = case(true);
    let now = flow::jobs::now();
    reviewed(&c, now);
    assert_eq!(step(&c, now + 2), Step::HandFix(Kind::Findings));
    unchanged(&c, now + 2, "answer");
    // The two threads' first comments trade places: each now names the
    // other's finding.
    let ids: Vec<String> = threads(&c).into_keys().collect();
    c.hub.edit_flow(REPO, 3, |s| {
        let a = s.threads[&ids[0]].comments[0].clone();
        let b = s.threads[&ids[1]].comments[0].clone();
        s.threads.get_mut(&ids[0]).unwrap().comments[0] = b;
        s.threads.get_mut(&ids[1]).unwrap().comments[0] = a;
    });
    for t in 1..10 {
        report(&c, now + 2 + 60 * t);
    }
    assert!(answers(&c).is_empty());
    assert_eq!(c.hub.count(Call::FlowReply), 0);
    assert!(threads(&c).values().all(|t| !t.resolved));
}

#[test]
fn a_reply_for_a_head_that_moved_is_never_posted() {
    let c = case(true);
    let now = flow::jobs::now();
    reviewed(&c, now);
    assert_eq!(step(&c, now + 2), Step::HandFix(Kind::Findings));
    unchanged(&c, now + 2, "answer");
    // Someone pushes before the answers go up.
    let mut patches = c.hub.patches(&head(3));
    patches.insert("someone's".into());
    c.hub.commit_as(&head(5), &[head(3)], patches);
    c.hub.push(REPO, 3, &head(5));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(5));
    for t in 1..10 {
        let r = report(&c, now + 2 + 60 * t);
        assert_ne!(r.next.unwrap().step, Step::PostFix("3-2".into()));
    }
    assert!(answers(&c).is_empty());
    assert_eq!(c.hub.count(Call::FlowReply), 0);
}

#[test]
fn a_superseded_fix_posts_nothing() {
    let c = case(true);
    let now = flow::jobs::now();
    reviewed(&c, now);
    assert_eq!(step(&c, now + 2), Step::HandFix(Kind::Findings));
    // A later fix of the same findings was started after it.
    let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
    let mut record = c.record();
    let mut later = fix_work(&c, "3-2");
    record.sequence += 1;
    later.job.id = format!("3-{}", record.sequence);
    let path = later
        .path
        .with_file_name(format!("later-{}.json", record.sequence));
    std::fs::copy(&later.path, &path).unwrap();
    c.deferred.0.borrow_mut().push(path.clone());
    later.path = path;
    record.jobs.push(later);
    data.save(3, &record).unwrap();
    drop(data);
    unchanged(&c, now + 2, "answer");
    until(&c, now, &mut 2, Step::QueueReview("reviewer".into()));
    let all = answers(&c);
    assert_eq!(all.len(), 4);
    assert!(all.iter().all(|a| a.contains(r#""fix":"3-3""#)));
}

#[test]
fn a_carried_reviews_findings_are_answered_on_its_own_threads() {
    let c = case(true);
    let now = flow::jobs::now();
    reviewed(&c, now);
    let p = publication(&c);
    let before = threads(&c);
    // Trunk moves and is merged in: the review on head 3 carries to head 4,
    // and its threads stay on it.
    let tip = c.hub.add(TRUNK, "trunk-only patch");
    c.hub.set_branch(REPO, "main", &tip);
    let mut patches = c.hub.patches(&tip);
    patches.extend(c.hub.patches(&head(3)));
    c.hub.commit_as(&head(4), &[head(3), tip], patches);
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    let mut t = 2;
    until(&c, now, &mut t, Step::HandFix(Kind::Findings));
    let handed: Vec<_> = fix_work(&c, &format!("3-{}", c.record().sequence))
        .handed
        .into_iter()
        .map(|h| h.unwrap().mark)
        .collect();
    assert_eq!(handed, (0..4).map(|i| p.mark(i)).collect::<Vec<_>>());
    assert!(handed.iter().all(|m| m.head == head(3)));
    unchanged(&c, now + t, "carried");
    until(&c, now, &mut t, Step::QueueReview("reviewer".into()));
    let after = threads(&c);
    assert_eq!(
        after.keys().collect::<Vec<_>>(),
        before.keys().collect::<Vec<_>>()
    );
    assert!(after.values().all(|t| t.resolved && t.comments.len() == 2));
    assert_eq!(answers(&c).len(), 4);
}

#[test]
fn an_acknowledged_write_waits_to_be_read_back_and_is_never_sent_again() {
    let c = case(true);
    let now = flow::jobs::now();
    assert_eq!(step(&c, now), Step::QueueReview("reviewer".into()));
    respond(
        &c,
        now,
        Answer::Review {
            findings: findings(),
        },
    );
    // GitHub takes the review and the comments, but reading them lags.
    c.hub.stale(Call::Reviews);
    c.hub.stale(Call::Comments);
    let mut t = 1;
    for _ in 0..5 {
        t += 60;
        assert_ne!(step_or_error(&c, now + t), Step::HandFix(Kind::Findings));
    }
    c.hub.fresh(Call::Reviews);
    for _ in 0..5 {
        t += 60;
        assert_ne!(step_or_error(&c, now + t), Step::HandFix(Kind::Findings));
    }
    c.hub.fresh(Call::Comments);
    until(&c, now, &mut t, Step::HandFix(Kind::Findings));
    assert_eq!(c.hub.count(Call::FlowReview), 1);
    assert_eq!(c.hub.count(Call::FlowComment), 2);
    // The fix's answers go up, but the threads and conversation lag: the
    // fix isn't done, nothing is resent, and no other look or fix starts.
    unchanged(&c, now + t, "answer");
    c.hub.stale(Call::FlowSnapshot);
    c.hub.stale(Call::Comments);
    for _ in 0..5 {
        t += 60;
        let next = step_or_error(&c, now + t);
        assert!(
            !matches!(next, Step::QueueReview(_) | Step::HandFix(_)),
            "{next:?}"
        );
    }
    assert_eq!(c.hub.count(Call::FlowReply), 2);
    assert_eq!(c.hub.count(Call::FlowComment), 4);
    c.hub.fresh(Call::FlowSnapshot);
    c.hub.fresh(Call::Comments);
    until(&c, now, &mut t, Step::QueueReview("reviewer".into()));
    assert_eq!(answers(&c).len(), 4);
    assert!(threads(&c).values().all(|t| t.resolved));
}

#[test]
fn a_reviews_plan_is_kept_when_a_diff_appears_after_it_was_sent() {
    // No diff at first: every finding goes to the conversation. The review
    // goes up, its answer is lost, and then GitHub gives the diff.
    let c = case(false);
    let now = flow::jobs::now();
    assert_eq!(step(&c, now), Step::QueueReview("reviewer".into()));
    respond(
        &c,
        now,
        Answer::Review {
            findings: findings(),
        },
    );
    c.hub.lose(Call::FlowReview);
    assert!(report(&c, now + 1).error.is_some());
    c.hub.heal(Call::FlowReview);
    let tip = c.hub.branch(REPO, "main").unwrap().unwrap();
    c.hub.flow_diff(&tip, &head(3), DIFF);
    let mut t = 1;
    until(&c, now, &mut t, Step::HandFix(Kind::Findings));
    assert_eq!(c.hub.count(Call::FlowReview), 1);
    assert!(threads(&c).is_empty());
    let p = publication(&c);
    let read = targets(&c);
    assert!((0..4).all(|i| read.target(&p.mark(i)) == Some(Target::Comment)));
}

#[test]
fn repeated_unchanged_rounds_never_count_as_missing_answers() {
    let c = case(true);
    let now = flow::jobs::now();
    let mut t = 0;
    for round in 0..3 {
        if round == 0 {
            assert_eq!(step(&c, now), Step::QueueReview("reviewer".into()));
        }
        respond(
            &c,
            now + t,
            Answer::Review {
                findings: findings(),
            },
        );
        until(&c, now, &mut t, Step::HandFix(Kind::Findings));
        unchanged(&c, now + t, &format!("round {round}"));
        until(&c, now, &mut t, Step::QueueReview("reviewer".into()));
        assert!(c.record().memory.unanswered.is_empty(), "round {round}");
    }
    assert_eq!(answers(&c).len(), 12);
}

#[test]
fn a_write_whose_answer_is_lost_is_not_sent_again_while_reads_lag() {
    let c = case(true);
    let now = flow::jobs::now();
    assert_eq!(step(&c, now), Step::QueueReview("reviewer".into()));
    respond(
        &c,
        now,
        Answer::Review {
            findings: findings(),
        },
    );
    // Every write goes up and its answer is lost, and reads don't show it.
    for call in [Call::FlowReview, Call::FlowComment, Call::FlowReply] {
        c.hub.lose(call);
    }
    c.hub.stale(Call::Reviews);
    c.hub.stale(Call::Comments);
    let mut t = 1;
    // Within the settling time, nothing is sent twice.
    while t + 60 < flow::publish::SETTLE {
        t += 60;
        assert_ne!(step_or_error(&c, now + t), Step::HandFix(Kind::Findings));
    }
    assert_eq!(c.hub.count(Call::FlowReview), 1);
    c.hub.fresh(Call::Reviews);
    while t + 60 < 2 * flow::publish::SETTLE {
        t += 60;
        assert_ne!(step_or_error(&c, now + t), Step::HandFix(Kind::Findings));
    }
    assert_eq!(c.hub.count(Call::FlowComment), 2);
    c.hub.fresh(Call::Comments);
    until(&c, now, &mut t, Step::HandFix(Kind::Findings));
    assert_eq!(c.hub.count(Call::FlowReview), 1);
    assert_eq!(c.hub.count(Call::FlowComment), 2);
    assert_eq!(c.hub.comments(REPO, 3).unwrap().len(), 2);
    // The same for a fix's replies.
    unchanged(&c, now + t, "answer");
    c.hub.stale(Call::FlowSnapshot);
    let from = t;
    while t + 60 < from + flow::publish::SETTLE {
        t += 60;
        step_or_error(&c, now + t);
    }
    assert_eq!(c.hub.count(Call::FlowReply), 2);
    c.hub.fresh(Call::FlowSnapshot);
    for call in [Call::FlowReview, Call::FlowComment, Call::FlowReply] {
        c.hub.heal(call);
    }
    until(&c, now, &mut t, Step::QueueReview("reviewer".into()));
    assert_eq!(c.hub.count(Call::FlowReply), 2);
    assert_eq!(answers(&c).len(), 4);
}

#[test]
fn a_write_that_never_went_up_is_sent_again_after_settling_at_most_three_times() {
    for recovers in [true, false] {
        let c = case(false);
        let now = flow::jobs::now();
        assert_eq!(step(&c, now), Step::QueueReview("reviewer".into()));
        respond(
            &c,
            now,
            Answer::Review {
                findings: findings(),
            },
        );
        // With no diff, every finding is a PR comment. Their requests never
        // reach GitHub: each is sent once, and not again within the
        // settling time.
        c.hub.fail(Call::FlowComment);
        let mut t = 1;
        while t + 60 < flow::publish::SETTLE {
            t += 60;
            step_or_error(&c, now + t);
        }
        assert_eq!(c.hub.count(Call::FlowComment), 4);
        if recovers {
            // Then they go through: each is sent once more, after settling.
            c.hub.heal(Call::FlowComment);
            until_waiting(&c, now, &mut t, Step::HandFix(Kind::Findings));
            assert!(t >= flow::publish::SETTLE);
            assert_eq!(c.hub.count(Call::FlowComment), 8);
            assert_eq!(c.hub.comments(REPO, 3).unwrap().len(), 4);
        } else {
            while t < 6 * flow::publish::SETTLE {
                t += 60;
                step_or_error(&c, now + t);
            }
            assert_eq!(
                c.hub.count(Call::FlowComment),
                4 * flow::publish::TRIES as usize
            );
        }
    }
}
