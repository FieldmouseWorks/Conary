//! GitHub's answers as its REST API sent them, recorded from
//! FieldmouseWorks/Conary#255 (`gh api`), one item of each list kept and
//! long text and diffs cut out. The simulated GitHub builds each answer from
//! one of these and writes its own fields over it, so what the code under
//! test parses is in GitHub's own shape, fields nothing reads included.

use serde_json::{Value, json};

fn load(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// `GET /repos/{repo}/pulls/{n}`.
pub(super) fn pull() -> Value {
    load(include_str!("fixtures/pull.json"))
}

/// One of `GET /repos/{repo}/commits/{sha}/check-runs`'s `check_runs`.
pub(super) fn check_run() -> Value {
    load(include_str!("fixtures/check-runs.json"))["check_runs"][0].clone()
}

/// One of `GET /repos/{repo}/commits/{sha}/statuses`.
pub(super) fn status() -> Value {
    load(include_str!("fixtures/statuses.json"))[0].clone()
}

/// `GET /repos/{repo}/commits/{sha}`.
pub(super) fn commit() -> Value {
    load(include_str!("fixtures/commit.json"))
}

/// One of `GET /repos/{repo}/issues/{n}/timeline`.
pub(super) fn event() -> Value {
    load(include_str!("fixtures/timeline.json"))[0].clone()
}

/// `GET /repos/{repo}/compare/{base}...{head}`.
pub(super) fn compare() -> Value {
    load(include_str!("fixtures/compare.json"))
}

/// A user as the recorded answers show one, as `login`, of `kind`
/// (`User` or `Bot`).
pub(super) fn user(login: &str, kind: &str) -> Value {
    let mut u = load(include_str!("fixtures/pull.json"))["user"].clone();
    u["login"] = json!(login);
    u["type"] = json!(kind);
    u
}
