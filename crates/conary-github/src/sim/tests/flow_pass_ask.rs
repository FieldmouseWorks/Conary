//! The review ask through the real executor: a pass with a review ask
//! command asks through it, as `COMMAND REPO PR HEAD`, and never comments
//! on the flow's token, whose App's ask Codex's connector ignores; one
//! without a command comments. Either way the ask is remembered only once
//! it went up.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::api::Source;
use crate::flow::{self, *};
use crate::sim::Call;

/// A command at `dir/token` that appends its arguments, one line, to
/// `dir/asked`, then exits with `exit`.
fn command(dir: &std::path::Path, exit: u8) -> String {
    let asked = dir.join("asked");
    crate::tokens::tests::script(
        dir,
        &format!("echo \"$@\" >> {}; exit {exit}", asked.display()),
    )
}
fn asked(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("asked")).unwrap_or_default()
}
fn asks(c: &Case) -> usize {
    c.hub
        .comments(REPO, 3)
        .unwrap()
        .into_iter()
        .filter_map(|c| c.body)
        .filter(|b| b.starts_with("@codex review"))
        .count()
}

#[test]
fn a_review_ask_command_asks_for_the_head_and_nothing_comments() {
    let scripts = tempfile::tempdir().unwrap();
    let mut c = Case::new();
    // Arguments of its own come before the repo, PR and head.
    c.review_ask = Some(format!("{} --as owner", command(scripts.path(), 0)));
    let now = flow::jobs::now();
    c.pass(now, false);
    let report = c.pass(now + 1800, false);
    assert!(matches!(
        report[0].next.as_ref().unwrap().step,
        Step::AskReview(_)
    ));
    assert_eq!(report[0].error, None);
    assert_eq!(
        asked(scripts.path()),
        format!("--as owner {REPO} 3 {}\n", head(3))
    );
    assert_eq!(asks(&c), 0);
    assert_eq!(c.hub.count(Call::FlowComment), 0);
    assert_eq!(c.record().memory.asked, Some((head(3), now + 1800)));
}

#[test]
fn a_review_ask_command_that_fails_is_not_remembered_and_asks_again() {
    let scripts = tempfile::tempdir().unwrap();
    let mut c = Case::new();
    c.review_ask = Some(command(scripts.path(), 1));
    let now = flow::jobs::now();
    c.pass(now, false);
    let report = c.pass(now + 1800, false);
    assert!(matches!(
        report[0].next.as_ref().unwrap().step,
        Step::AskReview(_)
    ));
    assert!(report[0].error.is_some());
    // It ran, said no, and nothing else asked in its place.
    assert_eq!(asked(scripts.path()).lines().count(), 1);
    assert_eq!(asks(&c), 0);
    assert_eq!(c.record().memory.asked, None);
    // Fixed, the next pass asks again rather than waiting an hour.
    c.review_ask = Some(command(scripts.path(), 0));
    let mut t = now + 1800;
    while c.record().memory.asked.is_none() {
        t += 600;
        assert!(t < now + 3600, "never asked again");
        c.pass(t, false);
    }
    assert_eq!(c.record().memory.asked, Some((head(3), t)));
    assert_eq!(asked(scripts.path()).lines().count(), 2);
    assert_eq!(asks(&c), 0);
}

#[test]
fn without_a_review_ask_command_the_token_comments_the_ask() {
    let c = Case::new();
    let now = flow::jobs::now();
    c.pass(now, false);
    let report = c.pass(now + 1800, false);
    assert_eq!(report[0].error, None);
    assert_eq!(asks(&c), 1);
    assert_eq!(c.record().memory.asked, Some((head(3), now + 1800)));
}
