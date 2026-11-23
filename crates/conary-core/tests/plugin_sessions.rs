//! A judge producer outside the runner checks a fresh judge with the flow's
//! own rule, through the public API.
use conary_core::plugin::{Context, Identity, Sessions, Task};

fn identity(engine: &str, session: &str) -> Identity {
    Identity {
        engine: engine.into(),
        session: Some(session.into()),
    }
}

fn judge(pr: u64) -> Task {
    Task::Judge {
        repo: "o/r".into(),
        pr,
        head: "a".repeat(40),
        against: "b".repeat(40),
        author: identity("writer", "w1"),
        reviewers: vec![identity("reviewer", "r1")],
        why: Vec::new(),
        context: String::new(),
    }
}

#[test]
fn a_fresh_judge_in_a_new_session_is_accepted() {
    let sessions = Sessions::default();
    sessions
        .accept(&judge(1), &Context::Fresh, &identity("judge", "j1"))
        .unwrap();
}

#[test]
fn a_judge_in_the_authors_or_a_reviewers_session_is_refused() {
    let sessions = Sessions::default();
    for session in ["w1", "r1"] {
        let refused = sessions
            .accept(&judge(1), &Context::Fresh, &identity("judge", session))
            .unwrap_err();
        assert_eq!(refused.0, "judge reused a writer or reviewer session");
    }
}

#[test]
fn a_fresh_judge_reusing_an_earlier_judges_session_is_refused() {
    let sessions = Sessions::default();
    sessions
        .accept(&judge(1), &Context::Fresh, &identity("judge", "j1"))
        .unwrap();
    let refused = sessions
        .accept(&judge(2), &Context::Fresh, &identity("judge", "j1"))
        .unwrap_err();
    assert_eq!(refused.0, "initial judge reused an existing session");
}

#[test]
fn a_judge_without_a_session_is_refused() {
    let refused = Sessions::default()
        .accept(
            &judge(1),
            &Context::Fresh,
            &Identity {
                engine: "judge".into(),
                session: None,
            },
        )
        .unwrap_err();
    assert_eq!(refused.0, "judge needs a session");
}

#[test]
fn a_continuation_without_an_earlier_judgment_is_refused() {
    let continued = Context::Continue {
        session: "j1".into(),
    };
    let refused = Sessions::default()
        .accept(&judge(1), &continued, &identity("judge", "j1"))
        .unwrap_err();
    assert_eq!(
        refused.0,
        "judge cannot continue a foreign session or scope"
    );
}

#[test]
fn a_continuation_is_accepted_only_for_the_task_it_judged() {
    let sessions = Sessions::default();
    let continued = Context::Continue {
        session: "j1".into(),
    };
    sessions
        .accept(&judge(1), &Context::Fresh, &identity("judge", "j1"))
        .unwrap();
    sessions
        .accept(&judge(1), &continued, &identity("judge", "j1"))
        .unwrap();
    let refused = sessions
        .accept(&judge(2), &continued, &identity("judge", "j1"))
        .unwrap_err();
    assert_eq!(
        refused.0,
        "judge cannot continue a foreign session or scope"
    );
    // The refused task left the session bound to the one it judged.
    sessions
        .accept(&judge(1), &continued, &identity("judge", "j1"))
        .unwrap();
}

#[test]
fn a_forked_judge_is_refused() {
    let forked = Context::Fork {
        session: "j0".into(),
    };
    let refused = Sessions::default()
        .accept(&judge(1), &forked, &identity("judge", "j1"))
        .unwrap_err();
    assert_eq!(refused.0, "judge cannot fork a session");
}
