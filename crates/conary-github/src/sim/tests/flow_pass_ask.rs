//! A late review is asked through the configured door where it covers the
//! repository, and through the flow's own comment elsewhere.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::api::Source;
use crate::flow::{self, ReviewAsk, Step};
use crate::sim::Call;
use std::path::Path;

/// A door at `dir/door` that records its arguments in `dir/args` and exits
/// with `exit`. It's run as `/bin/sh door`, so no test execs a file another
/// thread may still hold open for writing.
fn door(c: &mut Case, dir: &Path, exit: u8, repos: &[&str]) {
    std::fs::write(
        dir.join("door"),
        format!(
            "echo \"$@\" >> '{}'\nexit {exit}\n",
            dir.join("args").display()
        ),
    )
    .unwrap();
    c.config.review_ask = Some(ReviewAsk {
        command: vec!["/bin/sh".into(), dir.join("door").display().to_string()],
        repos: repos.iter().map(|r| r.parse().unwrap()).collect(),
    });
}
fn args(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("args"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
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
fn a_covered_repository_is_asked_through_the_door_and_never_commented_on() {
    let mut c = Case::new();
    let dir = tempfile::tempdir().unwrap();
    // Listed in another case, as GitHub spells it either way.
    door(&mut c, dir.path(), 0, &[&REPO.to_uppercase()]);
    let now = flow::jobs::now();
    c.pass(now, false);
    let report = c.pass(now + 1800, false);
    assert!(matches!(
        report[0].next.as_ref().unwrap().step,
        Step::AskReview(_)
    ));
    assert_eq!(report[0].error, None);
    assert_eq!(args(dir.path()), [format!("{REPO} 3 {}", head(3))]);
    assert_eq!(c.record().memory.asked, Some((head(3), now + 1800)));
    assert_eq!(c.hub.count(Call::FlowComment), 0);
    assert_eq!(asks(&c), 0);
}

#[test]
fn a_door_that_fails_records_no_ask_posts_no_comment_and_is_run_again() {
    let mut c = Case::new();
    let dir = tempfile::tempdir().unwrap();
    door(&mut c, dir.path(), 1, &[REPO]);
    let now = flow::jobs::now();
    c.pass(now, false);
    let report = c.pass(now + 1800, false);
    assert!(matches!(
        report[0].next.as_ref().unwrap().step,
        Step::AskReview(_)
    ));
    let error = report[0].error.as_deref().unwrap();
    assert!(error.contains("review ask command"), "{error}");
    // Its arguments never reach the error: one might be a credential.
    assert!(!error.contains("door"), "{error}");
    assert_eq!(args(dir.path()).len(), 1);
    assert_eq!(c.record().memory.asked, None);
    assert_eq!(c.hub.count(Call::FlowComment), 0);
    // Once it answers, a later pass asks, through it, once.
    door(&mut c, dir.path(), 0, &[REPO]);
    let mut t = now + 1800;
    while c.record().memory.asked.is_none() {
        t += 600;
        assert!(t < now + 3600, "never asked again");
        c.pass(t, false);
    }
    assert_eq!(c.record().memory.asked, Some((head(3), t)));
    assert_eq!(args(dir.path()).len(), 2);
    assert_eq!(asks(&c), 0);
}

#[test]
fn a_repository_the_door_doesnt_cover_is_asked_with_the_flow_s_comment() {
    let mut c = Case::new();
    let dir = tempfile::tempdir().unwrap();
    door(&mut c, dir.path(), 0, &["someone/else"]);
    let now = flow::jobs::now();
    c.pass(now, false);
    c.pass(now + 1800, false);
    assert!(args(dir.path()).is_empty());
    assert_eq!(c.record().memory.asked, Some((head(3), now + 1800)));
    assert_eq!(asks(&c), 1);
}

#[test]
fn an_empty_door_command_is_refused_as_the_configuration_loads() {
    for command in ["[]", "[\"\"]"] {
        let file = format!(r#"{{"review_ask": {{"command": {command}, "repos": ["{REPO}"]}}}}"#);
        assert!(flow::schema::load::<flow::Config>(file.as_bytes()).is_err());
    }
    let file = format!(r#"{{"review_ask": {{"command": ["/bin/door"], "repos": ["{REPO}"]}}}}"#);
    let (config, _) = flow::schema::load::<flow::Config>(file.as_bytes()).unwrap();
    assert_eq!(config.review_ask.unwrap().command, ["/bin/door"]);
}
