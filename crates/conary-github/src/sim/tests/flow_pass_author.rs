//! Who wrote the head, as a judge request names it, through the real
//! executor: a head no `Engine:` trailer names is judged against the account
//! that opened it, App or person, and a history with no one author waits,
//! saying so. Neither is a failed step retried forever (#551).
use super::flow_pass_case::{Case, REPO, example};
use super::head;
use crate::flow::{self, *};
use conary_core::plugin::{Identity, Task};

const APP: &str = "workers[bot]";

/// An R2 change, opened by an App, its one commit's message `message`,
/// with a judge plugin: it needs a judge.
fn opened_by_the_app(message: &str) -> Case {
    let mut c = Case::new();
    c.config.plugins.judge = Some(example());
    c.config.trusted_authors.push(APP.into());
    c.policy = conary_core::Policy::parse(br#"{"r2":["^src/"]}"#).unwrap();
    c.hub.edit_pull(REPO, 3, |p| {
        p["user"] = crate::sim::fixtures::user(APP, "Bot");
    });
    c.hub.commit_details(
        &head(3),
        Some(message),
        Some("owner@example.org"),
        Some("owner@example.org"),
    );
    c.clean_review();
    c
}

/// Nothing struck, nothing failed: what the pass decided is what it did.
fn unstruck(c: &Case, report: &flow::Report) {
    assert_eq!(report.error, None);
    let record = c.record();
    assert_eq!(record.failed, None);
    assert!(
        record.memory.strikes.is_empty(),
        "{:?}",
        record.memory.strikes
    );
    assert_eq!(record.memory.retry, None);
}

#[test]
fn an_app_opened_head_no_engine_names_is_judged_against_the_app() {
    let c = opened_by_the_app("change: fixture\n\nCo-Authored-By: Someone <someone@example.org>\n");
    let now = flow::jobs::now();
    for t in 0..3 {
        let report = c.pass(now + t, false).remove(0);
        unstruck(&c, &report);
        assert!(
            matches!(
                report.next.as_ref().unwrap().step,
                Step::Judge(_) | Step::Wait { .. }
            ),
            "{:?}",
            report.next
        );
    }
    let judges: Vec<_> = c
        .record()
        .jobs
        .into_iter()
        .filter(|w| matches!(w.purpose, flow::jobs::Purpose::Judge))
        .collect();
    assert_eq!(judges.len(), 1);
    let invocation: flow::jobs::Invocation =
        serde_json::from_slice(&std::fs::read(&judges[0].path).unwrap()).unwrap();
    let Task::Judge { author, why, .. } = invocation.task else {
        panic!("not a judge");
    };
    assert_eq!(why, [Trigger::R2]);
    assert_eq!(
        author,
        Identity {
            engine: APP.into(),
            session: None
        }
    );
    // The runner takes the request, and the judge's answer with it.
    c.run_worker();
    let done = flow::jobs::inspect(&judges[0], now + 10).unwrap();
    assert!(matches!(done.result, Some(Ok(_))), "{:?}", done.result);
}

#[test]
fn a_head_more_than_one_engine_wrote_waits_for_its_judge_saying_why() {
    let c = opened_by_the_app("change: fixture\n\nEngine: one\nEngine: another\n");
    let now = flow::jobs::now();
    for t in 0..3 {
        let report = c.pass(now + t, false).remove(0);
        unstruck(&c, &report);
        let next = report.next.unwrap();
        assert_eq!(next.step, Step::Wait { until: None });
        assert_eq!(
            next.description,
            "Needs a judge; its commits name more than one author engine"
        );
    }
    assert!(c.record().jobs.is_empty());
}
