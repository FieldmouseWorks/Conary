//! `cn flow status` answers from the data directory and the configuration
//! alone: no daemon, GitHub or token, and not kept off by a pass that holds
//! the repository's lock.
use std::process::Command;

#[test]
fn status_needs_nothing_but_the_data_directory_and_configuration() {
    let data = tempfile::tempdir().unwrap();
    let dir = data.path().join("flow/owner/repo");
    std::fs::create_dir_all(&dir).unwrap();
    // A pass in progress holds the repository's lock throughout.
    let lock = std::fs::File::create(dir.join("lock")).unwrap();
    lock.try_lock().unwrap();
    std::fs::write(
        dir.join("pause"),
        r#"{"scope":"repository","reason":"moving the machine","at":7}"#,
    )
    .unwrap();
    let config = data.path().join("flow.json");
    std::fs::write(&config, r#"{"plugins":{"fix":"/bin/true"}}"#).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cn"))
        .args(["flow", "status", "Owner/Repo", "--data"])
        .arg(data.path())
        .arg("--config")
        .arg(&config)
        .env_clear()
        .env("GITHUB_API_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // No pass has read the open PRs yet, so the switches hold none.
    assert_eq!(status["read_at"], serde_json::Value::Null);
    let switches: Vec<_> = status["switches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            assert_eq!(s["prs"], serde_json::json!([]));
            (
                s["switch"].as_str().unwrap().to_string(),
                s["kind"].as_str().map(String::from),
            )
        })
        .collect();
    let named = |s: &str, kind: Option<&str>| (s.to_string(), kind.map(String::from));
    assert_eq!(
        switches,
        [
            named("pause", None),
            named("no_plugin", Some("judge")),
            named("no_plugin", Some("proof")),
            named("no_plugin", Some("job")),
            named("train_off", None),
            named("no_people", None),
            named("no_evidence", None),
        ]
    );
    assert_eq!(status["switches"][0]["reason"], "moving the machine");
    assert_eq!(status["switches"][0]["at"], 7);
}

/// Each open PR's wait, with its stage, since when and when it's overdue,
/// and the owner request it's listed for by its subject key, as the last
/// pass saved them: what burrow's stall watchdog and owner issue read.
#[test]
fn status_prints_each_prs_overdue_wait_and_owner_listing() {
    use conary_github::flow::*;
    let data = tempfile::tempdir().unwrap();
    let repo: conary_core::op::Repo = "Owner/Repo".parse().unwrap();
    let next = |stage, step| Next {
        stage,
        step,
        description: String::new(),
        wait_until: None,
        overdue_at: None,
        queued: None,
        proof: None,
        deadline: None,
        wait_on: None,
        on_usage: false,
        owner_notice: None,
        switches: Vec::new(),
        priority: Level::default(),
        degraded: Vec::new(),
        recovery: None,
        owner: None,
    };
    // PR 3 waits on review, overdue since 1600.
    let overdue = Record {
        memory: Memory {
            wait: Some(Wait {
                head: "a".repeat(40),
                base: "b".repeat(40),
                stage: Stage::Review,
                description: "Waiting for review".into(),
                failure: None,
                since: 1000,
                due: 1600,
            }),
            ..Default::default()
        },
        decision: Some(Next {
            overdue_at: Some(1600),
            wait_on: Some(WaitOn::Steward),
            ..next(Stage::Review, Step::Wait { until: None })
        }),
        ..Default::default()
    };
    // PR 5 is an outside contribution listed for the owner.
    let key = Subject {
        door: Door::Outside,
        repo: "Owner/Repo".into(),
        pr: 5,
        scope: "intent".into(),
    };
    let listed = Record {
        decision: Some(Next {
            owner: Some(Request {
                key,
                since: 1200,
                prs: [5].into(),
                marker: None,
                reason: None,
                listed: true,
            }),
            ..next(Stage::Owner, Step::ListOwner(Door::Outside))
        }),
        ..Default::default()
    };
    // PR 7 has been open past a day: a notice in the owner's digest.
    let notice = Record {
        decision: Some(Next {
            owner_notice: Some(OwnerNotice::OpenOverDay),
            ..next(Stage::Review, Step::Wait { until: None })
        }),
        ..Default::default()
    };
    {
        let store = Data::open(data.path(), &repo).unwrap();
        store.save(3, &overdue).unwrap();
        store.save(5, &listed).unwrap();
        store.save(7, &notice).unwrap();
    }
    std::fs::write(
        data.path().join("flow/owner/repo/open"),
        r#"{"at":2000,"prs":[3,5,7]}"#,
    )
    .unwrap();
    let config = data.path().join("flow.json");
    std::fs::write(&config, r#"{"plugins":{}}"#).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cn"))
        .args(["flow", "status", "Owner/Repo", "--data"])
        .arg(data.path())
        .arg("--config")
        .arg(&config)
        .env_clear()
        .env("GITHUB_API_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        status["prs"],
        serde_json::json!([
            {"pr": 3, "wait": {"stage": "review", "wait_on": "Steward", "since": 1000, "due": 1600}},
            {"pr": 5, "owner": {
                "key": {"door": "outside", "repo": "Owner/Repo", "pr": 5, "scope": "intent"},
                "since": 1200,
            }},
            {"pr": 7, "notice": "OpenOverDay"},
        ]),
        "{status}"
    );
}
