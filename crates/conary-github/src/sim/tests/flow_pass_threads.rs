//! A fix's answers on the findings the flow published as its own review,
//! before Codex's connector was the reviewer, go on exactly those findings'
//! threads: through the real executor, against the simulated GitHub. The
//! plugins' answers are written as their workers would save them.
use super::flow_pass_case::{Case, REPO};
use super::{TRUNK, head};
use crate::api::Source;
use crate::flow::{self, *};
use crate::sim::{Call, STEWARD};
use conary_core::plugin::{
    self, Answer, Finding, FindingAnswer, FixResult, Identity, Problem, Severity, Task,
};

fn finding(path: &str, line: u64, text: &str) -> Finding {
    Finding {
        path: path.into(),
        line,
        severity: Severity::P1,
        text: text.into(),
    }
}

/// Two findings on one line.
pub(super) fn findings() -> Vec<Finding> {
    vec![
        finding("src/lib.rs", 2, "first on line two"),
        finding("src/lib.rs", 2, "second on line two"),
    ]
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

/// The flow's own review of [`findings`] at `now`, each a thread of its
/// own, as it published them before the connector was the reviewer; and a
/// pass at `now` that first sees them, which hands nothing on until the
/// quiet period after them has passed.
pub(super) fn reviewed(c: &Case, now: u64) -> Publication {
    let p = c.flow_findings(&findings(), findings().len(), now);
    assert!(matches!(step(c, now), Step::Wait { .. }));
    p
}

/// Passes a minute apart from `*t` until one waits on the reviewer: every
/// finding is answered and nothing else is owed.
pub(super) fn until_review(c: &Case, now: u64, t: &mut u64) {
    for _ in 0..40 {
        *t += 60;
        let r = report(c, now + *t);
        if r.next.as_ref().unwrap().stage == Stage::Review {
            assert_eq!(r.error, None);
            return;
        }
    }
    panic!("never waited on the reviewer");
}

/// The fix job the flow started last.
fn last_fix(c: &Case) -> flow::jobs::Work {
    fix_work(c, &format!("3-{}", c.record().sequence))
}
#[test]
fn a_fix_answers_each_of_the_flows_findings_on_exactly_its_own_thread() {
    let c = Case::new();
    let now = flow::jobs::now();
    let p = reviewed(&c, now);
    // Two findings on one line, each its own thread.
    let t = targets(&c);
    let target = |i| t.target(&p.mark(i)).unwrap();
    let (Target::Thread(first), Target::Thread(second)) = (target(0), target(1)) else {
        panic!("each finding has its thread");
    };
    assert_ne!(first, second);
    assert_eq!(threads(&c).len(), 2);
    // Its threads are its findings, not blocking threads: a fix gets them,
    // each problem with its target.
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    let work = last_fix(&c);
    let id = work.job.id.clone();
    assert_eq!(
        work.handed,
        (0..2)
            .map(|i| Some(Handed {
                mark: p.mark(i),
                target: target(i),
            }))
            .collect::<Vec<_>>()
    );
    unchanged(&c, now + 1, "kept because");
    // The first reply's answer is lost: it went up, and the retry reads it
    // back rather than replying twice.
    c.hub.lose(Call::FlowReply);
    let r = report(&c, now + 2);
    assert_eq!(r.next.unwrap().step, Step::PostFix(id.clone()));
    assert!(r.error.is_some());
    c.hub.heal(Call::FlowReply);
    // Once every finding is answered, it waits on the reviewer.
    until_review(&c, now, &mut 2);
    let all = threads(&c);
    for (i, thread) in [(0, &first), (1, &second)] {
        let thread = &all[thread];
        assert!(thread.resolved);
        assert_eq!(thread.comments.len(), 2, "one reply on thread {i}");
        assert_eq!(
            thread.comments[1].body,
            answer_body(&format!("kept because {i}"), &p.mark(i), &id)
        );
        assert_eq!(thread.comments[1].author.as_deref(), Some(STEWARD));
    }
    assert_eq!(answers(&c).len(), 2);
    assert_eq!(c.hub.pull(REPO, 3).unwrap().head.sha, head(3));
}

#[test]
fn a_second_review_on_the_same_head_is_handed_only_its_own_findings() {
    let c = Case::new();
    let now = flow::jobs::now();
    let first = reviewed(&c, now);
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    unchanged(&c, now + 1, "first answer");
    let mut t = 0;
    until_review(&c, now, &mut t);
    // The same findings again, on the same head, found later.
    let second = c.flow_findings(&findings(), 2, now + t);
    assert_ne!(first.issuance, second.issuance);
    // The first review's answers say nothing of the second's findings.
    let read = targets(&c);
    for i in 0..2 {
        assert!(read.threads(&first.mark(i)).iter().all(|(_, r)| *r));
        assert!(read.threads(&second.mark(i)).iter().all(|(_, r)| !*r));
        assert_ne!(read.target(&second.mark(i)), None);
    }
    until(&c, now, &mut t, Step::HandFix(Kind::Findings));
    let handed: Vec<_> = last_fix(&c)
        .handed
        .into_iter()
        .map(|h| h.unwrap().mark)
        .collect();
    assert_eq!(handed, (0..2).map(|i| second.mark(i)).collect::<Vec<_>>());
}

#[test]
fn a_reply_goes_only_on_its_own_findings_thread() {
    let c = Case::new();
    let now = flow::jobs::now();
    reviewed(&c, now);
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    unchanged(&c, now + 1, "answer");
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
        report(&c, now + 60 * t);
    }
    assert!(answers(&c).is_empty());
    assert_eq!(c.hub.count(Call::FlowReply), 0);
    assert!(threads(&c).values().all(|t| !t.resolved));
}

#[test]
fn a_reply_for_a_head_that_moved_is_never_posted() {
    let c = Case::new();
    let now = flow::jobs::now();
    reviewed(&c, now);
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    let id = last_fix(&c).job.id;
    unchanged(&c, now + 1, "answer");
    // Someone pushes before the answers go up.
    let mut patches = c.hub.patches(&head(3));
    patches.insert("someone's".into());
    c.hub.commit_as(&head(5), &[head(3)], patches);
    c.hub.push(REPO, 3, &head(5));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(5));
    for t in 1..10 {
        let r = report(&c, now + 60 * t);
        assert_ne!(r.next.unwrap().step, Step::PostFix(id.clone()));
    }
    assert!(answers(&c).is_empty());
    assert_eq!(c.hub.count(Call::FlowReply), 0);
}

#[test]
fn a_superseded_fix_posts_nothing() {
    let c = Case::new();
    let now = flow::jobs::now();
    reviewed(&c, now);
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    // A later fix of the same findings was started after it.
    let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
    let mut record = c.record();
    let mut later = last_fix(&c);
    record.sequence += 1;
    later.job.id = format!("3-{}", record.sequence);
    let path = later
        .path
        .with_file_name(format!("later-{}.json", record.sequence));
    std::fs::copy(&later.path, &path).unwrap();
    c.deferred.0.borrow_mut().push(path.clone());
    later.path = path;
    let id = later.job.id.clone();
    record.jobs.push(later);
    data.save(3, &record).unwrap();
    drop(data);
    unchanged(&c, now + 1, "answer");
    until_review(&c, now, &mut 0);
    let all = answers(&c);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|a| a.contains(&format!(r#""fix":"{id}""#))));
}

#[test]
fn an_earlier_heads_open_finding_threads_are_answered_on_their_own_threads() {
    let c = Case::new();
    let now = flow::jobs::now();
    let p = reviewed(&c, now);
    let before = threads(&c);
    // Trunk moves and is merged in: the threads on head 3 stay on head 4,
    // still owed their answers.
    let tip = c.hub.add(TRUNK, "trunk-only patch");
    c.hub.set_branch(REPO, "main", &tip);
    let mut patches = c.hub.patches(&tip);
    patches.extend(c.hub.patches(&head(3)));
    c.hub.commit_as(&head(4), &[head(3), tip], patches);
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    let mut t = 0;
    until(&c, now, &mut t, Step::HandFix(Kind::Findings));
    let handed: Vec<_> = last_fix(&c)
        .handed
        .into_iter()
        .map(|h| h.unwrap().mark)
        .collect();
    assert_eq!(handed, (0..2).map(|i| p.mark(i)).collect::<Vec<_>>());
    assert!(handed.iter().all(|m| m.head == head(3)));
    unchanged(&c, now + t, "carried");
    until_review(&c, now, &mut t);
    let after = threads(&c);
    assert_eq!(
        after.keys().collect::<Vec<_>>(),
        before.keys().collect::<Vec<_>>()
    );
    assert!(after.values().all(|t| t.resolved && t.comments.len() == 2));
    assert_eq!(answers(&c).len(), 2);
}

#[test]
fn an_acknowledged_write_waits_to_be_read_back_and_is_never_sent_again() {
    let c = Case::new();
    let now = flow::jobs::now();
    reviewed(&c, now);
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    // The fix's answers go up, but the threads lag: the fix isn't done,
    // nothing is resent, and no other look or fix starts.
    unchanged(&c, now + 1, "answer");
    c.hub.stale(Call::FlowSnapshot);
    c.hub.stale(Call::Comments);
    let mut t = 0;
    for _ in 0..5 {
        t += 60;
        let next = step_or_error(&c, now + t);
        assert!(
            !matches!(next, Step::AskReview(_) | Step::HandFix(_)),
            "{next:?}"
        );
    }
    assert_eq!(c.hub.count(Call::FlowReply), 2);
    c.hub.fresh(Call::FlowSnapshot);
    c.hub.fresh(Call::Comments);
    until_review(&c, now, &mut t);
    assert_eq!(c.hub.count(Call::FlowReply), 2);
    assert_eq!(answers(&c).len(), 2);
    assert!(threads(&c).values().all(|t| t.resolved));
}

#[test]
fn repeated_unchanged_rounds_never_count_as_missing_answers() {
    let c = Case::new();
    let now = flow::jobs::now();
    let mut t = 0;
    for round in 0..3 {
        c.flow_findings(&findings(), 2, now + t);
        until(&c, now, &mut t, Step::HandFix(Kind::Findings));
        unchanged(&c, now + t, &format!("round {round}"));
        until_review(&c, now, &mut t);
        assert!(c.record().memory.unanswered.is_empty(), "round {round}");
    }
    assert_eq!(answers(&c).len(), 6);
}

#[test]
fn a_reply_whose_answer_is_lost_is_not_sent_again_while_reads_lag() {
    let c = Case::new();
    let now = flow::jobs::now();
    reviewed(&c, now);
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    // Every reply goes up and its answer is lost, and reads don't show it:
    // within the settling time, nothing is sent twice.
    unchanged(&c, now + 1, "answer");
    c.hub.lose(Call::FlowReply);
    c.hub.stale(Call::FlowSnapshot);
    let mut t = 0;
    while t + 60 < flow::publish::SETTLE {
        t += 60;
        step_or_error(&c, now + t);
    }
    assert_eq!(c.hub.count(Call::FlowReply), 2);
    c.hub.fresh(Call::FlowSnapshot);
    c.hub.heal(Call::FlowReply);
    until_review(&c, now, &mut t);
    assert_eq!(c.hub.count(Call::FlowReply), 2);
    assert_eq!(answers(&c).len(), 2);
}

#[test]
fn a_reply_that_never_went_up_is_sent_again_after_settling_at_most_three_times() {
    for recovers in [true, false] {
        let c = Case::new();
        let now = flow::jobs::now();
        reviewed(&c, now);
        assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
        unchanged(&c, now + 1, "answer");
        // The replies' requests never reach GitHub: each is sent once, and
        // not again within the settling time.
        c.hub.fail(Call::FlowReply);
        let mut t = 0;
        while t + 60 < flow::publish::SETTLE {
            t += 60;
            step_or_error(&c, now + t);
        }
        assert_eq!(c.hub.count(Call::FlowReply), 2);
        if recovers {
            // Then they go through: each is sent once more, after settling.
            c.hub.heal(Call::FlowReply);
            until_review(&c, now, &mut t);
            assert!(t >= flow::publish::SETTLE);
            assert_eq!(c.hub.count(Call::FlowReply), 4);
            assert_eq!(answers(&c).len(), 2);
        } else {
            while t < 6 * flow::publish::SETTLE {
                t += 60;
                step_or_error(&c, now + t);
            }
            // No reply was sent more than three times, and one that was is
            // never sent again.
            let tries: Vec<u32> = last_fix(&c).sent.writes.values().map(|a| a.tries).collect();
            assert_eq!(tries.len(), 2);
            assert!(
                tries.iter().all(|n| *n <= flow::publish::TRIES),
                "{tries:?}"
            );
            assert!(tries.contains(&flow::publish::TRIES), "{tries:?}");
            let sent = c.hub.count(Call::FlowReply);
            assert_eq!(sent, tries.iter().sum::<u32>() as usize);
            while t < 12 * flow::publish::SETTLE {
                t += 60;
                step_or_error(&c, now + t);
            }
            assert_eq!(c.hub.count(Call::FlowReply), sent);
        }
    }
}
