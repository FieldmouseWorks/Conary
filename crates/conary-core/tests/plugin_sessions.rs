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
