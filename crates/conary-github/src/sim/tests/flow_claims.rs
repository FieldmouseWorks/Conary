use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use std::collections::BTreeMap;
const PUSHED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const THEIRS: &str = "cccccccccccccccccccccccccccccccccccccccc";
fn claim() -> Case {
    let mut c = Case::new();
    c.hub.flow_thread(REMI, 3, "thread", false);
    c.claims.push(FixClaim {
        kind: Kind::Findings,
        job: Job {
            id: "fix-1".into(),
            revision: c.scope(),
            state: JobState::Done,
            started: 1860,
            repeated: false,
        },
        answer: FixResult::Answer(FixAnswer {
            pushed: Some(PUSHED.into()),
            threads: BTreeMap::from([("thread".into(), true)]),
            description: None,
            comment: false,
        }),
        posted: false,
    });
    c
}
#[test]
fn an_unpushed_fix_posts_nothing_and_retries_as_a_job_without_its_answer() {
    // unpushed.py:115-142: claiming a refused push is no answer; a second unanswered job goes to the owner.
    let mut c = claim();
    assert_eq!(c.pass(2400).step, Step::Wait { until: Some(2460) });
    assert_eq!(c.memory.unposted[&Kind::Findings].1, Unposted::Unpushed);
    assert!(c.memory.answered_jobs.contains("fix-1"));
    assert_eq!(c.pass(2460).step, Step::HandFix(Kind::Findings));
    c.claims[0].job.id = "fix-2".into();
    c.claims[0].job.repeated = true;
    assert!(matches!(c.pass(3000).step, Step::Wait { .. }));
    assert_eq!(c.pass(3060).step, Step::ListOwner);
}
#[test]
fn only_the_actual_pushed_head_allows_fix_answers_to_post() {
    // unpushed.py:143: once its push landed, replies and description can go up, then facts must be reread.
    let mut c = claim();
    c.push(PUSHED);
    assert_eq!(c.pass(2400).step, Step::PostFix("fix-1".into()));
    assert!(!c.memory.answered_jobs.contains("fix-1"));
    c.claims[0].posted = true;
    assert_eq!(c.pass(2401).step, Step::Wait { until: Some(2401) });
    assert!(c.memory.answered_jobs.contains("fix-1"));
}
#[test]
fn a_foreign_push_stops_a_live_fix_but_an_apps_push_does_not() {
    // unpushed.py:160 / 171: GitHub's push actor distinguishes somebody else's head.
    for (who, stop) in [("person", true), ("workers[bot]", false)] {
        let mut c = claim();
        c.settings.job_login = Some("workers[bot]".into());
        c.claims[0].job.state = JobState::Running;
        c.push(PUSHED);
        let branch = c.hub.pull_json(REMI, 3)["head"]["ref"]
            .as_str()
            .unwrap()
            .to_owned();
        c.hub.pushed_by(REMI, &branch, PUSHED, who);
        assert_eq!(c.view(2400).step == Step::StopJob("fix-1".into()), stop);
        if !stop {
            assert!(matches!(c.view(2400).step, Step::Wait { .. }));
        }
    }
}
#[test]
fn an_unknown_pusher_is_looked_for_only_three_times_without_stopping_work() {
    // unpushed.py:171: activity may lag the push; look in two minutes, at most three times.
    let mut c = claim();
    c.settings.job_login = Some("workers[bot]".into());
    c.claims[0].job.state = JobState::Running;
    c.push(PUSHED);
    assert_eq!(c.pass(2100).step, Step::Wait { until: Some(2220) });
    assert_eq!(c.pass(2220).step, Step::Wait { until: Some(2340) });
    assert!(matches!(c.pass(2340).step, Step::Wait { until: None }));
    assert_eq!(c.memory.push_looks.values().next().unwrap().0, 3);
}
#[test]
fn without_an_app_a_running_job_carries_on_but_its_wrong_head_answers_stand_down() {
    // unpushed.py:182: user pushes are indistinguishable without the machinery App.
    let mut c = claim();
    c.claims[0].job.state = JobState::Running;
    c.push(THEIRS);
    assert!(matches!(c.pass(2100).step, Step::Wait { .. }));
    c.claims[0].job.state = JobState::Done;
    assert_ne!(c.pass(2400).step, Step::PostFix("fix-1".into()));
    assert_eq!(c.memory.unposted[&Kind::Findings].1, Unposted::Moved);
}
#[test]
fn a_queued_fix_on_an_old_head_cannot_have_pushed() {
    // unpushed.py:196 / 204: queued obsolete work is stopped, and retried until the stop succeeds.
    let mut c = claim();
    c.claims[0].job.state = JobState::Queued;
    c.push(THEIRS);
    for at in [2100, 2220, 2400] {
        assert_eq!(c.pass(at).step, Step::StopJob("fix-1".into()));
    }
    c.claims.clear();
    assert_ne!(c.view(2700).step, Step::StopJob("fix-1".into()));
}
#[test]
fn invalid_fix_claims_never_post_any_part_of_their_answers() {
    // unpushed.py / answer.rs: a malformed commit, a foreign thread, or fixed-without-push posts nothing.
    for scenario in 0..3 {
        let mut c = claim();
        if let FixResult::Answer(a) = &mut c.claims[0].answer {
            match scenario {
                0 => a.pushed = Some("not-a-commit".into()),
                1 => {
                    a.pushed = None;
                    a.threads = BTreeMap::from([("foreign-thread".into(), false)]);
                }
                _ => a.pushed = None,
            }
        }
        assert_ne!(c.pass(2400).step, Step::PostFix("fix-1".into()));
        assert_eq!(c.memory.unposted[&Kind::Findings].1, Unposted::Invalid);
    }
}
#[test]
fn an_answer_cannot_overwrite_a_description_somebody_else_changed() {
    // unpushed.py / answer.rs: the whole answer stands down when its requested description was superseded.
    let mut c = claim();
    c.push(PUSHED);
    c.description("a person's new explanation", 2300);
    if let FixResult::Answer(a) = &mut c.claims[0].answer {
        a.description = Some("another-description-fingerprint".into());
    }
    assert_ne!(c.pass(2400).step, Step::PostFix("fix-1".into()));
    assert_eq!(c.memory.unposted[&Kind::Findings].1, Unposted::Moved);
}
