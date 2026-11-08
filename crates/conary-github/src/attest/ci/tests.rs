use super::*;
use crate::attest::make::Gate;
use std::path::Path;

fn run(repo: &Path, args: &[&str]) -> String {
    git(repo, args).unwrap().trim().to_owned()
}

fn commit(repo: &Path) -> String {
    run(repo, &["add", "."]);
    run(
        repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "test",
        ],
    );
    run(repo, &["rev-parse", "HEAD"])
}

#[test]
fn skipped_failed_empty_and_cancelled_jobs_never_become_a_pass() {
    let conclusion = |jobs: &str| summary(jobs).map(|s| s.0);
    let (_, saved) =
        summary(r#"{"unit":{"result":"success","outputs":{"arbitrary":"not evidence"}}}"#).unwrap();
    assert_eq!(saved, r#"{"unit":{"result":"success"}}"#);
    assert_eq!(
        conclusion(r#"{"unit":{"result":"success"},"integration":{"result":"success"}}"#).unwrap(),
        Gate::Pass
    );
    for result in ["skipped", "failure"] {
        assert_eq!(
            conclusion(&format!(r#"{{"tests":{{"result":"{result}"}}}}"#)).unwrap(),
            Gate::Fail
        );
    }
    for jobs in [
        "{}",
        r#"{"tests":{"result":"cancelled"}}"#,
        r#"{"tests":{"result":"pass"}}"#,
        r#"{"tests":{}}"#,
    ] {
        assert!(conclusion(jobs).is_err(), "{jobs}");
    }
}

#[test]
fn negative_has_base_code_head_tests_and_deletions_without_running_filters() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    run(repo, &["init", "-q"]);
    std::fs::create_dir(repo.join("tests")).unwrap();
    std::fs::write(repo.join("code"), "old code").unwrap();
    std::fs::write(repo.join("tests/removed"), "old test").unwrap();
    std::fs::write(repo.join("tests/renamed"), "rename me").unwrap();
    let base = commit(repo);
    std::fs::write(repo.join("code"), "new code").unwrap();
    std::fs::remove_file(repo.join("tests/removed")).unwrap();
    std::fs::rename(
        repo.join("tests/renamed"),
        repo.join("tests/new name\nwith newline"),
    )
    .unwrap();
    std::fs::write(repo.join("tests/new"), "regression").unwrap();
    let head = commit(repo);
    let before = run(repo, &["write-tree"]);
    run(
        repo,
        &["config", "filter.tripwire.clean", "touch executed; cat"],
    );
    std::fs::write(repo.join(".gitattributes"), "* filter=tripwire\n").unwrap();
    assert!(!repo.join("executed").exists(), "test setup ran filter");
    let negative = plan(repo, &base, &head).unwrap();
    let tree = negative.tree.unwrap();
    assert_eq!(run(repo, &["show", &format!("{tree}:code")]), "old code");
    assert_eq!(
        run(repo, &["show", &format!("{tree}:tests/new")]),
        "regression"
    );
    assert!(git(repo, &["show", &format!("{tree}:tests/removed")]).is_err());
    assert!(git(repo, &["show", &format!("{tree}:tests/renamed")]).is_err());
    assert_eq!(
        run(
            repo,
            &["show", &format!("{tree}:tests/new name\nwith newline")]
        ),
        "rename me"
    );
    assert!(!repo.join("executed").exists());
    assert_eq!(run(repo, &["write-tree"]), before);
    assert_eq!(
        std::fs::read_to_string(repo.join("code")).unwrap(),
        "new code"
    );
    assert_eq!(negative.base, base);
    assert_eq!(negative.tests.len(), 2);
}

#[test]
fn test_only_code_only_and_deleted_tests_need_no_negative() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    run(repo, &["init", "-q"]);
    std::fs::create_dir(repo.join("tests")).unwrap();
    std::fs::write(repo.join("code"), "old").unwrap();
    std::fs::write(repo.join("tests/test"), "old").unwrap();
    let mut base = commit(repo);
    std::fs::write(repo.join("tests/test"), "changed").unwrap();
    let head = commit(repo);
    assert!(plan(repo, &base, &head).unwrap().tree.is_none());
    base = head;
    std::fs::write(repo.join("code"), "changed").unwrap();
    let head = commit(repo);
    assert!(plan(repo, &base, &head).unwrap().tree.is_none());
    base = head;
    std::fs::write(repo.join("code"), "changed again").unwrap();
    std::fs::remove_file(repo.join("tests/test")).unwrap();
    let head = commit(repo);
    assert!(plan(repo, &base, &head).unwrap().tree.is_none());
    assert!(tree(repo, &base, Some(&head)).is_err());
    assert!(tree(repo, "HEAD", None).is_err());
}

#[test]
fn candidate_cannot_forge_jobs_change_checkout_or_replace_the_negative_command() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    run(repo, &["init", "-q"]);
    std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
    let workflow = repo.join(".github/workflows/ci.yml");
    let reviewed =
        "jobs: ${{ toJSON(needs) }}\nnegative-command: cargo test\ncheckout: github.sha\n";
    std::fs::write(&workflow, reviewed).unwrap();
    let base = commit(repo);
    run(repo, &["fetch", ".", "HEAD:refs/remotes/origin/main"]);
    let check = |candidate: &str, identity: &str| {
        trusted_caller(repo, candidate, "owner/repo", identity, "main")
    };
    let identity = "owner/repo/.github/workflows/ci.yml@refs/pull/1/merge";
    assert!(check(&base, identity).is_ok());
    assert!(check(&base, "attacker/repo/.github/workflows/ci.yml@main").is_err());
    assert!(check(&base, "owner/repo/.github/workflows/new.yml@main").is_err());
    // Tests remain candidate-controlled; their CI wiring doesn't.
    std::fs::create_dir(repo.join("tests")).unwrap();
    std::fs::write(repo.join("tests/new.rs"), "new regression").unwrap();
    assert!(check(&commit(repo), identity).is_ok());
    for edit in [
        reviewed.replace(
            "${{ toJSON(needs) }}",
            r#"'{"tests":{"result":"success"}}'"#,
        ),
        reviewed.replace("cargo test", "false"),
        reviewed.replace("github.sha", "another-tree"),
        reviewed.replace("toJSON(needs)", "toJSON(needs.omitted-producers)"),
    ] {
        std::fs::write(&workflow, edit).unwrap();
        assert!(check(&commit(repo), identity).is_err());
    }
    std::fs::write(&workflow, reviewed).unwrap();
    std::fs::create_dir_all(repo.join(".github/actions/test")).unwrap();
    std::fs::write(
        repo.join(".github/actions/test/action.yml"),
        "replacement invocation",
    )
    .unwrap();
    assert!(check(&commit(repo), identity).is_err());
    assert!(trusted_caller(repo, &base, "owner/repo", identity, "main~1").is_err());
}
