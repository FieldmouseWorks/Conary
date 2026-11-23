//! FP11: a review loop that won't settle goes to the stuck role at each
//! trigger's boundary, once, and so does a stall the flow couldn't bring
//! back. Without a `job` plugin, as on main, none of it happens.
use super::flow_case::Case;
use super::flow_pass_case::{self as pass, REPO};
use super::head;
use crate::flow::{self, *};
use crate::sim::{CONNECTOR, REMI, STEWARD};
use conary_core::plugin::{Finding, Severity, Task};

/// `rounds` reviews of the head with findings, the limit three, and a
/// `job` plugin: no thread yet.
fn looping(rounds: u64) -> Case {
    let mut c = Case::new();
    c.stuck_role = true;
    c.settings.rounds = 3;
    for t in 1..=rounds {
        c.review("reviewer", Verdict::Findings, t);
    }
    c
}

fn handed(n: &Next) -> Option<Stuck> {
    match n.step {
        Step::Stuck(why, _) => Some(why),
        _ => None,
    }
}

#[test]
fn three_rounds_with_findings_hand_over_and_two_dont() {
    let two = looping(2);
    two.threads(1);
    assert_eq!(two.view(4000).step, Step::HandFix(Kind::Findings));
    let mut three = looping(3);
    three.threads(1);
    let n = three.view(4000);
    assert_eq!(handed(&n), Some(Stuck::RoundLimit), "{n:?}");
    assert!(
        n.description
            .starts_with("Handing it to the stuck role: 3 review rounds had findings"),
        "{}",
        n.description
    );
    // Without a job plugin, as on main: past the limit, a fix, then a judge.
    three.stuck_role = false;
    assert_eq!(three.view(4000).step, Step::HandFix(Kind::Findings));
}

/// One round with findings and a thread open, fix growth's share half the
/// original diff of 100 lines, and the diff now `diff` lines.
fn grown(diff: u64) -> Case {
    let mut c = looping(1);
    c.threads(1);
    c.settings.fix_growth = Some(50);
    c.memory.original = Some(100);
    c.hub.edit_flow(REMI, 3, |s| s.diff = Some(diff));
    c
}

#[test]
fn growth_just_past_the_share_hands_over_and_just_under_doesnt() {
    assert_eq!(grown(150).view(4000).step, Step::HandFix(Kind::Findings));
    let n = grown(151).view(4000);
    assert_eq!(handed(&n), Some(Stuck::FixGrowth), "{n:?}");
    // No share set, no growth hands anything over.
    let mut c = grown(1000);
    c.settings.fix_growth = None;
    assert_eq!(c.view(4000).step, Step::HandFix(Kind::Findings));
    // Nor without a job plugin.
    let mut c = grown(1000);
    c.stuck_role = false;
    assert_eq!(c.view(4000).step, Step::HandFix(Kind::Findings));
}

#[test]
fn the_first_fix_round_fixes_the_diff_later_heads_grow_from() {
    let mut c = looping(1);
    c.threads(1);
    c.settings.fix_growth = Some(50);
    c.hub.edit_flow(REMI, 3, |s| s.diff = Some(100));
    assert_eq!(c.pass(4000).step, Step::HandFix(Kind::Findings));
    assert_eq!(c.memory.original, Some(100));
    // The fix pushes; the next round's findings meet a grown diff.
    c.push(&head(4));
    c.review("reviewer", Verdict::Findings, 4500);
    c.threads(1);
    c.hub.edit_flow(REMI, 3, |s| s.diff = Some(151));
    c.pass(20_000);
    let n = c.pass(30_000);
    assert_eq!(handed(&n), Some(Stuck::FixGrowth), "{n:?}");
    assert_eq!(c.memory.original, Some(100));
}

const FINDING: &str = "**<sub><sub>![P2 Badge](https://img.shields.io/badge/P2-yellow?style=flat)</sub></sub>  Title**\n\nWhy.";

/// [`FINDING`] on thread T1, which the flow answered and resolved, and
/// thread T2 opened by `by` saying `again`, both on one file.
fn raised(by: &str, again: &str) -> Case {
    raised_after(FINDING, by, again)
}

/// [`raised`], the answered finding saying `earlier`.
fn raised_after(earlier: &str, by: &str, again: &str) -> Case {
    let c = looping(1);
    c.hub
        .flow_thread_at(REMI, 3, "T1", ("src/lib.rs", 3), (CONNECTOR, earlier));
    c.hub.flow_thread_comments(
        REMI,
        3,
        "T1",
        &[
            (CONNECTOR, earlier),
            (STEWARD, "Answered: it can't happen."),
        ],
    );
    c.hub.flow_thread(REMI, 3, "T1", true);
    c.hub
        .flow_thread_at(REMI, 3, "T2", ("src/lib.rs", 5), (by, again));
    c
}

#[test]
fn an_answered_finding_raised_again_unchanged_hands_over_and_a_changed_one_doesnt() {
    let n = raised(CONNECTOR, FINDING).view(4000);
    assert_eq!(handed(&n), Some(Stuck::RaisedAgain), "{n:?}");
    // A changed finding isn't a repeat.
    let changed = format!("{FINDING} And another case.");
    assert_eq!(
        raised(CONNECTOR, &changed).view(4000).step,
        Step::HandFix(Kind::Findings)
    );
    // Nor is one raised by anyone but the reviewer.
    assert_eq!(
        raised("someone", FINDING).view(4000).step,
        Step::HandFix(Kind::Findings)
    );
    // Nor one review author's finding in another's words: each repeats
    // only what it raised itself, with either switch.
    for disputed in [Disputed::Block, Disputed::Judge] {
        let mut c = raised("other-reviewer[bot]", FINDING);
        c.settings
            .review_authors
            .insert("other-reviewer[bot]".into());
        c.settings.disputed_p2 = disputed;
        assert_eq!(
            c.view(4000).step,
            Step::HandFix(Kind::Findings),
            "{disputed:?}"
        );
    }
    // Without a job plugin, as on main, a repeat is a finding to fix.
    let mut c = raised(CONNECTOR, FINDING);
    c.stuck_role = false;
    assert_eq!(c.view(4000).step, Step::HandFix(Kind::Findings));
}

#[test]
fn the_disputed_p2_switch_blocks_by_default_and_otherwise_sends_it_to_the_judge() {
    let mut c = raised(CONNECTOR, FINDING);
    c.stuck_role = false;
    assert_eq!(c.settings.disputed_p2, Disputed::Block);
    assert_eq!(c.view(4000).step, Step::HandFix(Kind::Findings));
    // Set to judge, the repeat blocks nothing: the flow concedes it, then
    // the judge decides, with no other review round on the head.
    c.settings.disputed_p2 = Disputed::Judge;
    assert_eq!(c.pass(4000).step, Step::Concede);
    c.hub.flow_thread(REMI, 3, "T2", true);
    c.memory.conceded = Some(c.scope().head);
    assert_eq!(c.view(4001).step, Step::Judge(vec![Trigger::RoundLimit]));
    // With a job plugin too, it hands nothing to the stuck role.
    let mut c = raised(CONNECTOR, FINDING);
    c.settings.disputed_p2 = Disputed::Judge;
    assert_eq!(c.view(4000).step, Step::Concede);
    // A P1 raised again still blocks.
    let p1 = FINDING.replace("P2", "P1");
    let mut c = raised_after(&p1, CONNECTOR, &p1);
    c.settings.disputed_p2 = Disputed::Judge;
    assert_eq!(handed(&c.view(4000)), Some(Stuck::RaisedAgain));
}

#[test]
fn a_conceded_documentation_head_still_goes_to_the_judge() {
    // Documentation alone needs no review round, and a concession on it
    // still owes the judge it promised.
    let mut c = raised(CONNECTOR, FINDING);
    c.hub.touch(&head(3), &["docs/a.md"]);
    c.settings.disputed_p2 = Disputed::Judge;
    c.hub.flow_thread(REMI, 3, "T2", true);
    c.memory.conceded = Some(c.scope().head);
    assert_eq!(c.view(4000).step, Step::Judge(vec![Trigger::RoundLimit]));
    // Without it, as on main, that head would land.
    c.memory.conceded = None;
    assert_eq!(c.view(4000).step, Step::Merge);
}

#[test]
fn findings_on_the_stuck_roles_head_go_to_the_judge_with_no_fix_round() {
    let mut c = looping(3);
    c.threads(1);
    let at = c.scope();
    // Handed, as the executor saves it with its job, and that job running.
    c.memory.stuck.push(Handoff {
        why: Stuck::RoundLimit,
        head: at.head.clone(),
        stage: None,
        at: 4000,
        job: "3-1".into(),
        answered: false,
    });
    c.jobs.insert(
        Kind::Stuck,
        Job {
            id: "3-1".into(),
            revision: at,
            state: JobState::Running,
            started: 4000,
            repeated: false,
        },
    );
    assert_eq!(c.view(4100).description, "A fix job still owns the branch");
    // It pushed and ended: its head gets one more review, and nothing else.
    c.push(&head(4));
    c.jobs.get_mut(&Kind::Stuck).unwrap().state = JobState::Done;
    c.threads(1);
    for now in [4200, 9000] {
        let n = c.pass(now);
        assert!(
            matches!(n.step, Step::Wait { .. } | Step::AskReview(_)),
            "{n:?}"
        );
        assert_eq!(n.stage, Stage::Review);
    }
    // Its findings are conceded to the judge, never handed to a fix or to
    // the stuck role again.
    c.review("reviewer", Verdict::Findings, 9100);
    assert_eq!(c.pass(9200).step, Step::Concede);
    c.threads(0);
    assert_eq!(c.view(9300).step, Step::Judge(vec![Trigger::RoundLimit]));
    // A clean review of its head still goes to the judge.
    c.review("reviewer", Verdict::Clean, 9400);
    assert_eq!(c.view(9500).step, Step::Judge(vec![Trigger::RoundLimit]));
}

#[test]
fn a_stuck_role_job_that_ended_without_its_answer_concedes_nothing() {
    let mut c = looping(3);
    c.threads(1);
    let at = c.scope();
    c.memory.stuck.push(Handoff {
        why: Stuck::RoundLimit,
        head: at.head.clone(),
        stage: None,
        at: 4000,
        job: "3-1".into(),
        answered: false,
    });
    c.jobs.insert(
        Kind::Stuck,
        Job {
            id: "3-1".into(),
            revision: at,
            state: JobState::Broken,
            started: 4000,
            repeated: false,
        },
    );
    // It waits, overdue in its status in time, and nothing hands it over
    // again: no fix round, no concession, no second job, no stall's.
    for now in [4100, 4100 + 7200, 4100 + 30 * 7200] {
        let n = c.pass(now);
        assert_eq!(n.step, Step::Wait { until: None }, "{n:?}");
        assert_eq!(n.description, flow::STUCK_ENDED);
    }
    assert_eq!(c.memory.stuck.len(), 1);
    // A push moves it on: the PR is decided afresh, and its clean head lands.
    c.push(&head(4));
    c.threads(0);
    c.review("reviewer", Verdict::Clean, 4100 + 31 * 7200);
    let n = c.pass(4100 + 32 * 7200);
    assert_eq!(n.step, Step::Merge, "{n:?}");
    assert!(c.memory.stuck.is_empty());
}

#[test]
fn a_follow_up_is_an_issue_of_the_prs_own_repository() {
    let repo = REMI.parse().unwrap();
    let said = |line: &str| flow::stuck_followup(&format!("unchanged\n{line}\nwhat it did"), &repo);
    assert_eq!(
        said("follow-up: https://github.com/fieldmouseworks/remi/issues/12").as_deref(),
        Some("https://github.com/fieldmouseworks/remi/issues/12")
    );
    for other in [
        "follow-up: none",
        "follow-up: https://github.com/FieldmouseWorks/burrow/issues/12",
        "follow-up: https://github.com/FieldmouseWorks/remi/pull/12",
        "follow-up: https://github.com/FieldmouseWorks/remi/issues/12x",
        "follow-up: https://github.com/FieldmouseWorks/remi/issues/",
    ] {
        assert_eq!(said(other), None, "{other}");
    }
}

// The pass, executing: one job per loop and per stall, however often it
// wakes, and what that job is asked.

fn report(c: &pass::Case, now: u64) -> flow::Report {
    c.pass(now, false).remove(0)
}

fn step(c: &pass::Case, now: u64) -> Step {
    let r = report(c, now);
    assert_eq!(r.error, None, "{r:?}");
    r.next.unwrap().step
}

/// The stuck role's jobs the flow started on PR 3.
fn stuck_jobs(c: &pass::Case) -> Vec<flow::jobs::Work> {
    c.record()
        .jobs
        .into_iter()
        .filter(|w| matches!(w.purpose, flow::jobs::Purpose::Stuck(_)))
        .collect()
}

fn with_job_plugin(mut c: pass::Case) -> pass::Case {
    c.config.plugins.job = Some(pass::example());
    c
}

#[test]
fn repeated_wakes_start_one_stuck_role_job_for_a_loop() {
    let mut c = with_job_plugin(pass::Case::new());
    c.config.plugins.judge = Some(pass::example());
    c.config.settings.rounds = 1;
    let now = flow::jobs::now();
    let finding = Finding {
        path: "src/lib.rs".into(),
        line: 2,
        severity: Severity::P1,
        text: "A finding".into(),
    };
    let threads = c.connector_findings(&[finding], now);
    assert!(matches!(step(&c, now), Step::Wait { .. }));
    let n = report(&c, now + 1).next.unwrap();
    assert_eq!(handed(&n), Some(Stuck::RoundLimit), "{n:?}");
    let jobs = stuck_jobs(&c);
    assert_eq!(jobs.len(), 1);
    let invocation: flow::jobs::Invocation =
        serde_json::from_slice(&std::fs::read(&jobs[0].path).unwrap()).unwrap();
    let Task::Job {
        workdir, prompt, ..
    } = invocation.task
    else {
        panic!("not a job");
    };
    assert_eq!(workdir, ".");
    let prompt: serde_json::Value = serde_json::from_str(&prompt).unwrap();
    assert_eq!(prompt["pr"], 3);
    assert_eq!(prompt["why"], "round_limit");
    assert!(prompt["detail"].as_str().is_some_and(|d| !d.is_empty()));
    assert_eq!(prompt.as_object().unwrap().len(), 3);
    // Wakes while it runs, a push of its own among them, start nothing more
    // and stop nothing.
    for t in [2, 60, 600] {
        assert!(matches!(step(&c, now + t), Step::Wait { .. }));
    }
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    let n = report(&c, now + 700).next.unwrap();
    assert_eq!(n.description, "A fix job still owns the branch");
    // It answers that it pushed, naming the PR's follow-up issue: its head
    // gets one more review, which finds the same.
    answer(
        &c,
        &jobs[0],
        &format!(
            "pushed {}\nfollow-up: {FOLLOW_UP}\nFixed one, left one.",
            head(4)
        ),
    );
    for t in [800, 900] {
        let n = report(&c, now + t).next.unwrap();
        assert_eq!(n.stage, Stage::Review, "{n:?}");
        assert!(handed(&n).is_none());
    }
    c.hub.flow_review(REPO, 3, Verdict::Findings, now + 950);
    // The flow concedes the open finding with its answer, and then the
    // judge decides: no fix round, and no second stuck-role job.
    assert_eq!(step(&c, now + 1000), Step::Concede);
    let thread = &pass_threads(&c)[&threads[0]];
    assert!(thread.resolved);
    let answer = &thread.comments.last().unwrap().body;
    assert!(
        answer.contains("stuck role (round_limit)")
            && answer.contains(&format!("Its follow-up issue: {FOLLOW_UP}")),
        "{answer}"
    );
    let n = report(&c, now + 1100).next.unwrap();
    assert_eq!(n.step, Step::Judge(vec![Trigger::RoundLimit]), "{n:?}");
    assert_eq!(stuck_jobs(&c).len(), 1);
    assert!(
        !c.record()
            .jobs
            .iter()
            .any(|w| matches!(w.purpose, flow::jobs::Purpose::Fix(_)))
    );
}

const FOLLOW_UP: &str = "https://github.com/conary-test/flow-simulation/issues/9";

/// The stuck role's job `work` answers, done, saying `summary`, as the
/// stuck role's plugin says it.
fn answer(c: &pass::Case, work: &flow::jobs::Work, summary: &str) {
    answer_next(c, work, summary, conary_core::plugin::Next::Done);
}

/// [`answer`], ending `next`.
fn answer_next(
    c: &pass::Case,
    work: &flow::jobs::Work,
    summary: &str,
    next: conary_core::plugin::Next,
) {
    let completion = flow::jobs::Completion {
        at: flow::jobs::now(),
        result: Some(Ok(conary_core::plugin::Response {
            contract: conary_core::plugin::CONTRACT.into(),
            by: conary_core::plugin::Identity {
                engine: "stuck".into(),
                session: Some("stuck-session".into()),
            },
            execution: None,
            answer: conary_core::plugin::Answer::Job {
                summary: summary.into(),
                next,
            },
        })),
    };
    std::fs::write(
        work.path.with_extension("answer"),
        serde_json::to_vec(&completion).unwrap(),
    )
    .unwrap();
    c.deferred.0.borrow_mut().retain(|p| *p != work.path);
}

#[test]
fn a_stuck_role_job_that_asks_or_awaits_settles_nothing() {
    for next in [
        conary_core::plugin::Next::Ask {
            question: "Drop the feature?".into(),
            default: "Keep it".into(),
            one_way: true,
        },
        conary_core::plugin::Next::Await {
            url: "https://github.com/conary-test/flow-simulation/pull/9".into(),
        },
    ] {
        let mut c = with_job_plugin(pass::Case::new());
        c.config.settings.rounds = 1;
        let now = flow::jobs::now();
        let finding = Finding {
            path: "src/lib.rs".into(),
            line: 2,
            severity: Severity::P1,
            text: "A finding".into(),
        };
        let threads = c.connector_findings(&[finding], now);
        step(&c, now);
        assert_eq!(
            handed(&report(&c, now + 1).next.unwrap()),
            Some(Stuck::RoundLimit)
        );
        let job = stuck_jobs(&c).remove(0);
        answer_next(
            &c,
            &job,
            "unchanged\nfollow-up: none\nWaiting on a call.",
            next.clone(),
        );
        // Its loop isn't broken on it: nothing is conceded or judged, no
        // fix round or second job starts, and the PR waits, overdue in time.
        for t in [2, 60, 7200, 30 * 7200] {
            let n = report(&c, now + t).next.unwrap();
            assert_eq!(n.description, flow::STUCK_ENDED, "{next:?}: {n:?}");
        }
        assert!(!pass_threads(&c)[&threads[0]].resolved);
        assert_eq!(c.record().jobs.len(), 1, "{next:?}");
    }
}

#[test]
fn a_stuck_role_job_a_hold_stopped_is_handed_again_once_its_lifted() {
    let mut c = with_job_plugin(pass::Case::new());
    c.config.settings.rounds = 1;
    let now = flow::jobs::now();
    let finding = Finding {
        path: "src/lib.rs".into(),
        line: 2,
        severity: Severity::P1,
        text: "A finding".into(),
    };
    c.connector_findings(&[finding], now);
    step(&c, now);
    assert_eq!(
        handed(&report(&c, now + 1).next.unwrap()),
        Some(Stuck::RoundLimit)
    );
    let first = stuck_jobs(&c)[0].job.id.clone();
    // The hold stops it: no loop was broken by it, and nothing is conceded.
    c.hub.label(REPO, 3, &["hold"]);
    report(&c, now + 2);
    c.run_worker();
    for t in [3, 4] {
        report(&c, now + t);
    }
    assert!(
        c.record().memory.stuck.is_empty(),
        "{:?}",
        c.record().memory.stuck
    );
    // Lifted, the loop is handed again, to a job of its own, once.
    c.hub.label(REPO, 3, &[]);
    let mut handoffs = 0;
    for t in [5, 6, 60, 120] {
        handoffs += usize::from(handed(&report(&c, now + t).next.unwrap()).is_some());
    }
    assert_eq!(handoffs, 1);
    let jobs = stuck_jobs(&c);
    assert_eq!(jobs.len(), 2);
    assert_ne!(jobs[1].job.id, first);
    assert_eq!(c.record().memory.stuck[0].job, jobs[1].job.id);
}

fn pass_threads(c: &pass::Case) -> std::collections::BTreeMap<String, ReviewThread> {
    flow::Read::snapshot(&c.hub, REPO, 3).unwrap().threads
}

/// Two hours, the simulated flow's `stall_after` at the PR's level.
const STALL: u64 = 7200;
const START: u64 = 1_000_000;

/// A settled PR whose required check ran in a suite of no App the flow
/// knows a way to run again: its wait on CI stalls, and no recovery applies.
fn stalled() -> pass::Case {
    let c = pass::Case::new();
    c.clean_review();
    c.hub.put_checks(
        &head(3),
        serde_json::json!([{"id":701,"name":"check","status":"completed","conclusion":"cancelled",
            "check_suite":{"id":7}}]),
    );
    c
}

#[test]
fn a_stall_no_recovery_brought_back_goes_to_the_stuck_role_once() {
    // As on main: overdue, and nothing more.
    let c = stalled();
    step(&c, START);
    for now in [START + STALL, START + STALL + 60] {
        assert_eq!(step(&c, now), Step::Wait { until: None });
    }
    assert!(stuck_jobs(&c).is_empty());
    // With a job plugin, the stall goes to the stuck role at its deadline.
    let c = with_job_plugin(stalled());
    let waiting = report(&c, START).next.unwrap();
    assert_eq!(waiting.stage, Stage::Ci);
    assert_eq!(step(&c, START + STALL - 1), Step::Wait { until: None });
    let n = report(&c, START + STALL).next.unwrap();
    assert_eq!(handed(&n), Some(Stuck::Stall), "{n:?}");
    assert!(
        n.description
            .starts_with("Overdue; handing it to the stuck role"),
        "{}",
        n.description
    );
    let jobs = stuck_jobs(&c);
    assert_eq!(jobs.len(), 1);
    let invocation: flow::jobs::Invocation =
        serde_json::from_slice(&std::fs::read(&jobs[0].path).unwrap()).unwrap();
    let Task::Job { prompt, .. } = invocation.task else {
        panic!("not a job");
    };
    let prompt: serde_json::Value = serde_json::from_str(&prompt).unwrap();
    assert_eq!(prompt["why"], "stall");
    assert!(
        prompt["detail"]
            .as_str()
            .is_some_and(|d| d.contains(&waiting.description)),
        "{prompt}"
    );
    // Repeated wakes, while it runs and after it ends, hand nothing more,
    // and nothing lists the owner.
    for now in [START + STALL + 60, START + 2 * STALL] {
        let n = report(&c, now).next.unwrap();
        assert!(handed(&n).is_none() && n.owner.is_none(), "{n:?}");
    }
    c.run_worker();
    for now in [START + 3 * STALL, START + 5 * STALL] {
        let n = report(&c, now).next.unwrap();
        assert!(handed(&n).is_none() && n.owner.is_none(), "{n:?}");
        assert_eq!(n.stage, Stage::Ci);
    }
    assert_eq!(stuck_jobs(&c).len(), 1);
}

#[test]
fn a_switch_report_names_the_stuck_role_off_without_a_job_plugin() {
    let c = pass::Case::new();
    let off = |c: &pass::Case| {
        flow::status(c.dir.path(), &REPO.parse().unwrap(), &c.config, START)
            .unwrap()
            .switches
            .iter()
            .any(|s| s.switch == Switch::NoPlugin { kind: "job".into() })
    };
    assert!(off(&c));
    assert!(!off(&with_job_plugin(c)));
}

#[test]
fn a_stall_on_a_pr_the_flow_doesnt_work_goes_to_no_one() {
    // Three failed status writes make a stall at once, decided before the
    // flow reads whether it works the PR at all: an outside contribution's
    // goes to no stuck role.
    for trusted in [true, false] {
        let mut c = Case::new();
        c.stuck_role = true;
        if !trusted {
            c.hub.edit_pull(REMI, 3, |p| {
                p["user"]["login"] = serde_json::json!("stranger");
            });
        }
        c.failed = Some(Failure::Status);
        let mut steps = Vec::new();
        for now in [1000, 1001, 1002, 1003] {
            steps.push(c.pass(now));
        }
        assert_eq!(
            steps.iter().any(|n| handed(n) == Some(Stuck::Stall)),
            trusted,
            "trusted {trusted}: {steps:?}"
        );
    }
}

/// Through real passes, `disputed_p2` set to `judge`, and an answered P2
/// raised again, unchanged, on thread T2, the reviewer's findings at `now`.
fn disputed_pass(now: u64) -> pass::Case {
    let mut c = pass::Case::new();
    c.config.settings.disputed_p2 = Disputed::Judge;
    c.config.plugins.judge = Some(pass::example());
    c.hub
        .flow_thread_at(REPO, 3, "T1", ("src/lib.rs", 3), (CONNECTOR, FINDING));
    c.hub.flow_thread_comments(
        REPO,
        3,
        "T1",
        &[
            (CONNECTOR, FINDING),
            (STEWARD, "Answered: it can't happen."),
        ],
    );
    c.hub.flow_thread(REPO, 3, "T1", true);
    c.hub
        .flow_thread_at(REPO, 3, "T2", ("src/lib.rs", 5), (CONNECTOR, FINDING));
    c.hub.flow_review(REPO, 3, Verdict::Findings, now);
    c
}

#[test]
fn a_conceded_thread_is_resolved_only_once_its_answer_shows() {
    let now = flow::jobs::now();
    let c = disputed_pass(now);
    // The answer fails on its way up: it may or may not have gone.
    c.hub.fail(crate::sim::Call::FlowReply);
    assert_eq!(report(&c, now + 1).next.unwrap().step, Step::Concede);
    c.hub.heal(crate::sim::Call::FlowReply);
    // Every pass after, while the answer isn't there, leaves the thread
    // open for it; once it's sent again and shows, the thread is resolved.
    for t in [70, 300, 590, 700, 800, 900, 1000] {
        report(&c, now + t);
        let t2 = &pass_threads(&c)["T2"];
        let answered = t2
            .comments
            .iter()
            .any(|c| c.author.as_deref() == Some(STEWARD));
        assert!(answered || !t2.resolved, "resolved unanswered at +{t}");
    }
    let t2 = &pass_threads(&c)["T2"];
    assert!(t2.resolved && t2.comments.len() == 2, "{t2:?}");
}

#[test]
fn a_concession_cut_short_still_goes_to_the_judge() {
    let now = flow::jobs::now();
    let c = disputed_pass(now);
    // Its answer goes up, and resolving it fails: the head already owes a
    // judge.
    c.hub.fail(crate::sim::Call::FlowResolve);
    let r = report(&c, now + 1);
    assert_eq!(r.next.unwrap().step, Step::Concede);
    assert!(r.error.is_some());
    assert_eq!(c.record().memory.conceded, Some(head(3)));
    // Resolved after all (the write went through, the answer was lost), and
    // the reviewer clean: the judge still decides, and nothing lands first.
    c.hub.heal(crate::sim::Call::FlowResolve);
    c.hub.flow_thread(REPO, 3, "T2", true);
    c.clean_review_at(now + 2);
    let mut steps = Vec::new();
    for t in [3, 120, 600, 3600, 7200] {
        steps.push(report(&c, now + t).next.unwrap().step);
    }
    assert!(
        !steps
            .iter()
            .any(|s| matches!(s, Step::Merge | Step::JoinTrain)),
        "{steps:?}"
    );
    assert!(
        steps.contains(&Step::Judge(vec![Trigger::RoundLimit])),
        "{steps:?}"
    );
}
