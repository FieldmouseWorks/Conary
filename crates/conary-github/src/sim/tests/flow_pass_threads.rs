//! The reviewer's findings, threads Codex's connector opens, go to a fix,
//! and its answers go up as replies on exactly those threads, which are then
//! resolved: through the real executor, against the simulated GitHub. The
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
        respond_to(&path, at, answer.clone());
    }
}

/// Answers the worker launched with `path` with `answer`, completed at `at`.
fn respond_to(path: &std::path::Path, at: u64, answer: Answer) {
    {
        let completion = flow::jobs::Completion {
            at,
            result: Some(Ok(plugin::Response {
                contract: plugin::CONTRACT.into(),
                by: Identity {
                    engine: "fixture".into(),
                    session: Some("fixture-session".into()),
                },
                execution: None,
                answer,
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
    respond(c, at, unchanged_answer(&path, reply));
}

/// What a fix that changes nothing and answers each finding it was handed,
/// launched with `path`, answers.
fn unchanged_answer(path: &std::path::Path, reply: &str) -> Answer {
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
    Answer::Fix {
        result: FixResult::Unchanged,
        answers: Some(answers),
    }
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

/// Every answer on GitHub: the replies on its threads.
fn answers(c: &Case) -> Vec<String> {
    threads(c)
        .into_values()
        .flat_map(|t| t.comments.into_iter().skip(1).map(|c| c.body))
        .collect()
}

fn fix_work(c: &Case, id: &str) -> flow::jobs::Work {
    c.record()
        .jobs
        .into_iter()
        .find(|w| w.job.id == id)
        .unwrap()
}

/// The connector's review of [`findings`] at `now`, each a thread of its
/// own; and a pass at `now` that first sees them, which hands nothing on
/// until the quiet period after them has passed. Returns the threads' ids.
pub(super) fn reviewed(c: &Case, now: u64) -> Vec<String> {
    let threads = c.connector_findings(&findings(), now);
    assert!(matches!(step(c, now), Step::Wait { .. }));
    threads
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
fn a_fix_answers_each_of_the_connectors_findings_on_exactly_its_own_thread() {
    let c = Case::new();
    let now = flow::jobs::now();
    let ids = reviewed(&c, now);
    // Two findings on one line, each its own thread.
    let (first, second) = (ids[0].clone(), ids[1].clone());
    assert_ne!(first, second);
    assert_eq!(threads(&c).len(), 2);
    // A fix gets each thread as a problem: where it is, its severity, and
    // what it says, under its author.
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    let work = last_fix(&c);
    assert_eq!(
        work.handed,
        ids.iter().cloned().map(Some).collect::<Vec<_>>()
    );
    let path = c.deferred.0.borrow()[0].clone();
    let invocation: flow::jobs::Invocation =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let Task::Fix { problems, .. } = invocation.task else {
        panic!("not a fix");
    };
    for (i, problem) in problems.iter().enumerate() {
        let Problem::Finding { finding } = problem else {
            panic!("not a finding");
        };
        assert_eq!((finding.path.as_str(), finding.line), ("src/lib.rs", 2));
        assert_eq!(finding.severity, Severity::P1);
        assert!(finding.text.contains(&ids[i]), "{}", finding.text);
        assert!(
            finding
                .text
                .contains(&format!("{}: ", crate::sim::CONNECTOR)),
            "{}",
            finding.text
        );
        assert!(finding.text.contains(&findings()[i].text));
    }
    let id = work.job.id.clone();
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
        assert_eq!(thread.comments[1].body, format!("kept because {i}"));
        assert_eq!(thread.comments[1].author.as_deref(), Some(STEWARD));
    }
    assert_eq!(answers(&c).len(), 2);
    assert_eq!(c.hub.pull(REPO, 3).unwrap().head.sha, head(3));
}

#[test]
fn the_connectors_findings_go_through_a_fix_to_another_look() {
    let c = Case::new();
    let now = flow::jobs::now();
    reviewed(&c, now);
    // Its open threads go to a fix, not a wait.
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    let id = last_fix(&c).job.id;
    unchanged(&c, now + 1, "kept because");
    assert_eq!(step(&c, now + 2), Step::PostFix(id));
    // Each answered and resolved, the connector is asked to look again: no
    // push will bring it.
    let mut t = 2;
    let asked = loop {
        t += 60;
        assert!(t < 40 * 60, "never asked for another look");
        if let Step::AskReview(why) = step(&c, now + t) {
            break why;
        }
    };
    assert!(asked.contains("answered"), "{asked}");
    assert!(
        threads(&c)
            .values()
            .all(|t| t.resolved && t.comments.len() == 2)
    );
    assert!(
        c.hub
            .comments(REPO, 3)
            .unwrap()
            .iter()
            .filter_map(|c| c.body.as_deref())
            .any(|b| b == "@codex review")
    );
    assert_eq!(c.record().memory.asked.map(|(h, _)| h), Some(head(3)));
}

/// GraphQL names the flow's App by its slug alone, typed a `Bot`
/// (as the sim reads it back): its reply still completes the fix, and
/// a user who took that name and said the same is no answer of its.
#[test]
fn a_fixs_reply_read_back_as_graphql_gives_it_completes_the_step() {
    let slug = STEWARD.strip_suffix("[bot]").unwrap();
    for spoofed in [false, true] {
        let c = Case::new();
        let now = flow::jobs::now();
        let ids = reviewed(&c, now);
        assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
        let id = last_fix(&c).job.id;
        unchanged(&c, now + 1, "kept because");
        if spoofed {
            c.hub.edit_flow(REPO, 3, |s| {
                for (i, thread) in ids.iter().enumerate() {
                    let comments = &mut s.threads.get_mut(thread).unwrap().comments;
                    comments.push(ThreadComment {
                        author: Some(slug.into()),
                        body: format!("kept because {i}"),
                    });
                }
            });
        }
        let posted = report(&c, now + 2);
        assert_eq!(posted.error, None, "spoofed: {spoofed}");
        assert_eq!(posted.next.unwrap().step, Step::PostFix(id.clone()));
        // Sent whatever the user said, and only once.
        until_review(&c, now, &mut 2);
        assert_eq!(
            c.hub.count(Call::FlowReply),
            ids.len(),
            "spoofed: {spoofed}"
        );
        assert!(c.record().memory.answered_jobs.contains(&id));
        let read = threads(&c);
        for thread in &ids {
            let last = read[thread].comments.last().unwrap();
            assert_eq!(last.author.as_deref(), Some(STEWARD));
        }
    }
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
    let second = c.connector_findings(&findings(), now + t);
    // The first review's answered threads are settled; the second's aren't.
    let read = threads(&c);
    for i in 0..2 {
        assert!(read[&first[i]].resolved);
        assert!(!read[&second[i]].resolved);
    }
    until(&c, now, &mut t, Step::HandFix(Kind::Findings));
    let handed: Vec<_> = last_fix(&c).handed.into_iter().flatten().collect();
    assert_eq!(handed, second);
}

#[test]
fn a_reply_goes_only_on_a_thread_still_on_github() {
    let c = Case::new();
    let now = flow::jobs::now();
    let ids = reviewed(&c, now);
    assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Findings));
    unchanged(&c, now + 1, "answer");
    // One of the two threads is gone before the answers go up.
    c.hub.edit_flow(REPO, 3, |s| {
        s.threads.remove(&ids[0]);
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
    record.jobs.push(later);
    data.save(3, &record).unwrap();
    drop(data);
    // Each answers in its own words.
    let paths: Vec<_> = c.deferred.0.borrow_mut().drain(..).collect();
    respond_to(&paths[0], now + 1, unchanged_answer(&paths[0], "earlier"));
    respond_to(&paths[1], now + 1, unchanged_answer(&paths[1], "later"));
    until_review(&c, now, &mut 0);
    let all = answers(&c);
    assert_eq!(all, ["later 0", "later 1"]);
}

#[test]
fn an_earlier_heads_open_threads_are_answered_on_their_own_threads() {
    let c = Case::new();
    let now = flow::jobs::now();
    let ids = reviewed(&c, now);
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
    let handed: Vec<_> = last_fix(&c).handed.into_iter().flatten().collect();
    assert_eq!(handed, ids);
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
        c.connector_findings(&findings(), now + t);
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
