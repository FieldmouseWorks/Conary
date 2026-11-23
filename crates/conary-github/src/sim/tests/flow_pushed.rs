//! Who put the head on the branch decides whether the reviewer is asked at
//! once: the connector reviews a person's push on its own, and no head an
//! App pushed unless asked, whoever opened the PR (#601).
use super::flow_case::Case;
use super::head;
use crate::flow::*;
use crate::sim::{Call, REMI};
use serde_json::json;

/// No review of the head (head 3) yet, on a person's PR.
fn unreviewed() -> Case {
    let c = Case::new();
    c.hub.remove_flow_reviews(REMI, 3);
    c
}

/// The head was put on the PR's branch by `who`, of GitHub's type `kind`.
fn pushed_by(c: &Case, who: &str, kind: &str) {
    let branch = c.hub.pull_json(REMI, 3)["head"]["ref"]
        .as_str()
        .unwrap()
        .to_owned();
    c.hub.pushed_by(REMI, &branch, &head(3), who, kind);
}

const APP: &str = "fieldmouseworks-workers[bot]";

fn asked(step: &Step) -> bool {
    matches!(step, Step::AskReview(why) if why.contains(&head(3)[..7]) && why.contains("pushed by an App"))
}

#[test]
fn an_apps_push_to_a_persons_pr_is_asked_for_on_the_pass_that_first_sees_it() {
    // With a fix job's account set, and without: the pusher is read either way.
    for job_login in [None, Some(APP.to_owned())] {
        let mut c = unreviewed();
        c.settings.job_login = job_login.clone();
        pushed_by(&c, APP, "Bot");
        let first = c.pass(60);
        assert_eq!(first.stage, Stage::Review, "{job_login:?}");
        assert!(asked(&first.step), "{job_login:?}: {:?}", first.step);
        // Asked: the same head isn't asked for again within the hour, however
        // many passes look at it, then is.
        c.memory.asked = Some((head(3), 60));
        c.memory.first_asked = Some((head(3), 60));
        for t in [61, 60 + QUIET, 1800, 60 + 3599] {
            assert_eq!(
                c.pass(t).step,
                Step::Wait {
                    until: Some(60 + 3600)
                },
                "{job_login:?} at {t}"
            );
        }
        assert!(asked(&c.pass(60 + 3600).step), "{job_login:?}");
    }
}

#[test]
fn an_apps_push_under_review_is_waited_for_not_asked_again() {
    let mut c = unreviewed();
    pushed_by(&c, APP, "Bot");
    c.hub.flow_review(REMI, 3, Verdict::Running, 30);
    let next = c.pass(60);
    assert_eq!(
        (next.stage, next.step),
        (Stage::Review, Step::Wait { until: None })
    );
    assert!(next.description.contains("is reviewing this commit"));
}

#[test]
fn a_persons_push_waits_quiet_then_is_asked_for() {
    let mut c = unreviewed();
    pushed_by(&c, "person", "User");
    c.pass(0);
    assert_eq!(c.pass(60).step, Step::Wait { until: Some(QUIET) });
    const { assert!(QUIET <= 5 * 60) };
    let late = c.pass(QUIET);
    assert!(
        matches!(&late.step, Step::AskReview(why) if why.contains("No review of")),
        "{:?}",
        late.step
    );
}

#[test]
fn a_persons_push_the_connector_starts_on_inside_quiet_is_never_asked_for() {
    let mut c = unreviewed();
    pushed_by(&c, "person", "User");
    c.pass(0);
    c.hub.flow_review(REMI, 3, Verdict::Running, 90);
    for t in [QUIET, QUIET + 1, 3600] {
        let next = c.pass(t);
        assert_eq!(
            (next.stage, next.step),
            (Stage::Review, Step::Wait { until: None }),
            "at {t}"
        );
    }
}

#[test]
fn a_persons_push_to_an_apps_pr_waits_quiet() {
    let mut c = unreviewed();
    c.hub
        .edit_pull(REMI, 3, |p| p["user"]["type"] = json!("Bot"));
    pushed_by(&c, "person", "User");
    c.pass(0);
    assert_eq!(c.pass(60).step, Step::Wait { until: Some(QUIET) });
}

#[test]
fn a_push_not_on_record_yet_goes_by_who_opened_the_pr() {
    // A person's PR: the connector's own start is waited for.
    let mut c = unreviewed();
    c.pass(0);
    assert_eq!(c.pass(60).step, Step::Wait { until: Some(QUIET) });
    // An App's: asked at once.
    let mut c = unreviewed();
    c.hub
        .edit_pull(REMI, 3, |p| p["user"]["type"] = json!("Bot"));
    assert!(matches!(c.pass(60).step, Step::AskReview(_)));
}

#[test]
fn pushers_that_cant_be_read_hold_up_nothing_and_ask_nothing_early() {
    // No fix job's account is set, so nothing needs the pushers: the pass
    // goes on, naming the read, as if no push were on record.
    let mut c = unreviewed();
    pushed_by(&c, APP, "Bot");
    c.hub.fail(Call::Pushes);
    c.pass(0);
    let next = c.pass(60);
    assert_eq!(next.step, Step::Wait { until: Some(QUIET) });
    assert!(
        next.description.contains("Waiting for"),
        "{}",
        next.description
    );
    let met = format!("{:?}", c.facts().reads.met);
    assert!(met.contains("Pushes"), "{met}");
}
