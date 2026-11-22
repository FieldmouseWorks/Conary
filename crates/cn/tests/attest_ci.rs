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
    let mut command = Command::new(env!("CARGO_BIN_EXE_cn"));
    command.args(["attest"]).args(args);
    if args.first() == Some(&"ci") {
        command.args([
            "--repository",
            "owner/repo",
            "--workflow-ref",
            "owner/repo/.github/workflows/ci.yml@refs/pull/1/merge",
            "--default-branch",
            "main",
        ]);
    }
    command
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
    std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
    std::fs::write(repo.join(".github/workflows/ci.yml"), "reviewed CI wiring").unwrap();
    std::fs::write(repo.join("code"), "fixed\n").unwrap();
    let head = commit(repo);
    git(repo, &["fetch", ".", "HEAD:refs/remotes/origin/main"]);
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
    std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
    std::fs::write(repo.join(".github/workflows/ci.yml"), "reviewed CI wiring").unwrap();
    std::fs::write(repo.join("code"), "old\n").unwrap();
    let base = commit(repo);
    git(repo, &["fetch", ".", "HEAD:refs/remotes/origin/main"]);
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

#[test]
fn changed_ci_wiring_cannot_sign_fabricated_success_or_an_unrelated_negative() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
    let caller = repo.join(".github/workflows/ci.yml");
    std::fs::write(
        &caller,
        "jobs: ${{ toJSON(needs) }}\nnegative-command: cargo test\n",
    )
    .unwrap();
    let base = commit(repo);
    git(repo, &["fetch", ".", "HEAD:refs/remotes/origin/main"]);
    std::fs::write(
        &caller,
        "jobs: fabricated-success\nnegative-command: false\n",
    )
    .unwrap();
    let head = commit(repo);
    let log = repo.join("summary.json");
    let out = repo.join("signed");
    let common = [
        "ci",
        "--repo",
        repo.to_str().unwrap(),
        "--commit",
        &head,
        "--jobs",
        r#"{"tests":{"result":"success"}}"#,
        "--log",
        log.to_str().unwrap(),
        "--log-artifact",
        "test-log",
        "--out",
        out.to_str().unwrap(),
    ];
    for extra in [
        vec![],
        vec![
            "--base",
            &base,
            "--head",
            &head,
            "--negative-result",
            "fail",
            "--negative-check",
            "negative-run",
        ],
    ] {
        let result = cn(&[&common[..], &extra].concat());
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("candidate CI wiring"));
        assert!(!out.exists());
        assert!(!log.exists());
    }
}

/// A PR that moves a test module its old code declares, as the reusable
/// workflow runs it: the negative tree fails for the missing file alone, so
/// nothing is attested; a failing test in a file that stayed still is.
#[test]
fn a_moved_test_module_is_judged_by_trees_with_it_put_back() {
    let judged = |stayed: &str| {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("candidate");
        std::fs::create_dir_all(repo.join("tests")).unwrap();
        git(&repo, &["init", "-q"]);
        // The old code runs each test file it names.
        std::fs::write(repo.join("code"), "old\n").unwrap();
        std::fs::write(repo.join("suite"), "tests/a.sh\ntests/c.sh\n").unwrap();
        std::fs::write(repo.join("tests/a.sh"), "true\n").unwrap();
        std::fs::write(repo.join("tests/c.sh"), "true\n").unwrap();
        let base = commit(&repo);
        std::fs::rename(repo.join("tests/a.sh"), repo.join("tests/b.sh")).unwrap();
        std::fs::write(repo.join("suite"), "tests/b.sh\ntests/c.sh\n").unwrap();
        std::fs::write(repo.join("code"), "new\n").unwrap();
        std::fs::write(repo.join("tests/c.sh"), stayed).unwrap();
        let head = commit(&repo);
        let plan = cn(&[
            "prepare-negative",
            "--repo",
            repo.to_str().unwrap(),
            "--base",
            &base,
            "--head",
            &head,
        ]);
        let plan_file = dir.path().join("plan.json");
        std::fs::write(&plan_file, made(plan).to_string()).unwrap();
        // Each planned tree in a checkout of its own, as the workflow has it.
        let runner = dir.path().join("run-tree");
        // No /usr/bin/env in a build sandbox: the bash on PATH.
        let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|d| d.join("bash"))
            .find(|b| b.is_file())
            .unwrap();
        std::fs::write(
            &runner,
            format!(
                "#!{}\nset -u\nw={}\nrm -rf \"$w/t\" && mkdir \"$w/t\" || exit 2\ngit -C \"$w/candidate\" archive \"$1\" | tar -x -C \"$w/t\" || exit 2\ncd \"$w/t\" && while read -r t; do bash \"$t\" || exit 1; done < suite\n",
                bash.display(),
                dir.path().display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).unwrap();
        cn(&[
            "judge-negative",
            "--plan",
            plan_file.to_str().unwrap(),
            "--run",
            runner.to_str().unwrap(),
        ])
    };
    let moved = judged("true\n");
    assert!(!moved.status.success());
    assert!(moved.stdout.is_empty(), "nothing is said to be signed");
    assert!(String::from_utf8_lossy(&moved.stderr).contains("can't tell"));
    let caught = judged("grep -qx new code\n");
    assert!(caught.status.success());
    assert_eq!(String::from_utf8_lossy(&caught.stdout), "fail\n");
}
