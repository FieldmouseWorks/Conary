//! Exercise the reusable workflow's Rust interface with a real git history.
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Output};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn commit(repo: &Path) -> String {
    git(repo, &["add", "."]);
    git(
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
    git(repo, &["rev-parse", "HEAD"])
}

fn cn(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cn"))
        .args(["attest"])
        .args(args)
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
        .output()
        .unwrap()
}

fn made(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn caller_jobs_bind_the_read_tree_and_summary_and_never_pass_a_skip() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    std::fs::write(repo.join("code"), "fixed\n").unwrap();
    let head = commit(repo);
    let log = repo.join("summary.json");
    let out = repo.join("signed");
    let tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
    for (jobs, result) in [
        (
            r#"{"unit":{"result":"success"},"browser":{"result":"success"}}"#,
            "pass",
        ),
        (
            r#"{"unit":{"result":"success"},"browser":{"result":"skipped"}}"#,
            "fail",
        ),
    ] {
        let files = made(cn(&[
            "ci",
            "--repo",
            repo.to_str().unwrap(),
            "--commit",
            &head,
            "--jobs",
            jobs,
            "--log",
            log.to_str().unwrap(),
            "--log-artifact",
            "test-log",
            "--out",
            out.to_str().unwrap(),
        ]));
        let predicate: Value =
            serde_json::from_slice(&std::fs::read(files["predicate"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(predicate["tree"], tree);
        assert_eq!(predicate["result"], result);
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
        assert_eq!(saved, serde_json::from_str::<Value>(jobs).unwrap());
        assert_eq!(
            std::fs::read_to_string(files["subject"].as_str().unwrap()).unwrap(),
            format!("conary commit v1\n{head}\n")
        );
    }
    assert!(
        !cn(&[
            "ci",
            "--repo",
            repo.to_str().unwrap(),
            "--commit",
            &head,
            "--jobs",
            "{}",
            "--log",
            log.to_str().unwrap(),
            "--log-artifact",
            "test-log",
            "--out",
            out.to_str().unwrap()
        ])
        .status
        .success()
    );
}

#[test]
fn regression_passes_on_head_fails_on_negative_and_gets_its_own_statement() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    std::fs::write(repo.join("code"), "old\n").unwrap();
    let base = commit(repo);
    std::fs::write(repo.join("code"), "fixed\n").unwrap();
    std::fs::create_dir(repo.join("tests")).unwrap();
    std::fs::write(repo.join("tests/regression.sh"), "grep -qx fixed code\n").unwrap();
    let head = commit(repo);
    let test = || {
        Command::new("bash")
            .arg("tests/regression.sh")
            .current_dir(repo)
            .status()
            .unwrap()
            .success()
    };
    assert!(test());
    let plan = made(cn(&[
        "prepare-negative",
        "--repo",
        repo.to_str().unwrap(),
        "--base",
        &base,
        "--head",
        &head,
    ]));
    let tree = plan["tree"].as_str().unwrap();
    git(repo, &["read-tree", "--reset", "-u", tree]);
    assert!(!test(), "the regression must fail against the old code");
    let log = repo.join("summary.json");
    let out = repo.join("signed");
    let args = [
        "ci",
        "--repo",
        repo.to_str().unwrap(),
        "--commit",
        &head,
        "--base",
        &base,
        "--head",
        &head,
        "--jobs",
        r#"{"negative-run":{"result":"success"}}"#,
        "--negative-result",
        "fail",
        "--negative-check",
        "bash tests/regression.sh",
        "--log",
        log.to_str().unwrap(),
        "--log-artifact",
        "negative-log-test",
        "--out",
        out.to_str().unwrap(),
    ];
    let files = made(cn(&args));
    let predicate: Value =
        serde_json::from_slice(&std::fs::read(files["predicate"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(predicate["tree"], tree);
    assert_eq!(predicate["result"], "fail");
    assert_eq!(predicate["tests"], json!(["tests/regression.sh"]));
    assert_eq!(predicate["checks"], json!(["bash tests/regression.sh"]));
    assert_eq!(
        std::fs::read_to_string(files["subject"].as_str().unwrap()).unwrap(),
        format!("conary negative v1\n{base}\n{head}\n")
    );
    let mut missing = args.to_vec();
    let at = missing.iter().position(|a| *a == "fail").unwrap();
    missing[at] = "";
    assert!(
        !cn(&missing).status.success(),
        "missing execution cannot attest failure"
    );
    missing[at] = "none";
    assert!(
        !cn(&missing).status.success(),
        "a mixed change cannot claim no negative needed"
    );
}
