//! conary-plugin v4: what the runner sends each contract, and what it refuses.
use conary_core::plugin::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

struct Scripted {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

/// A plugin that describes itself as `describe` and gives `answers`, in order.
fn scripted(describe: Value, answers: &[Value]) -> Scripted {
    static COMPILED: OnceLock<tempfile::TempDir> = OnceLock::new();
    let compiled = COMPILED.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("rustc")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/scripted_plugin.rs"))
            .arg("-o")
            .arg(dir.path().join("plugin"))
            .status()
            .unwrap();
        assert!(status.success());
        dir
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugin");
    // A hard link: never a file open for writing while another test spawns.
    std::fs::hard_link(compiled.path().join("plugin"), &path).unwrap();
    std::fs::write(path.with_extension("describe"), describe.to_string()).unwrap();
    let lines: Vec<_> = answers.iter().map(Value::to_string).collect();
    std::fs::write(path.with_extension("answers"), lines.join("\n")).unwrap();
    Scripted { _dir: dir, path }
}

impl Scripted {
    fn plugin(&self) -> Result<Plugin, Failure> {
        Plugin::describe(
            &self.path,
            Limits {
                deadline: Duration::from_secs(10),
                response_bytes: 65_536,
            },
        )
    }
    /// The work requests the plugin received, in order.
    fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path.with_extension("requests"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn describe(versions: &[&str], kinds: &[&str], v4_kinds: &[&str], models: Value) -> Value {
    json!({
        "contract": CONTRACT_V1,
        "by": {"engine": "scripted", "session": null},
        "kind": "describe",
        "versions": versions,
        "kinds": kinds,
        "v4_kinds": v4_kinds,
        "models": models,
        "capabilities": {"sandbox": "none", "network": false, "devices": [], "mcp": false},
        "meters": [],
    })
}

/// A v4 program that speaks every contract and advertises every flow kind.
fn v4_program() -> Value {
    describe(
        &CONTRACTS,
        &["review", "fix", "judge", "proof"],
        &["prepare"],
        json!([]),
    )
}

/// An answer in `contract`, from its own session, reporting a program's run.
fn answer(contract: &str, session: &str, body: Value) -> Value {
    let mut answer = json!({
        "contract": contract,
        "by": {"engine": "scripted", "session": session},
        "execution": {"model": null, "effort": null, "context": {"kind": "fresh"}},
    });
    answer
        .as_object_mut()
        .unwrap()
        .extend(body.as_object().unwrap().clone());
    answer
}

fn head() -> String {
    "2".repeat(40)
}

fn finding(text: &str) -> Problem {
    Problem::Finding {
        finding: Finding {
            path: "src/lib.rs".into(),
            line: 1,
            severity: Severity::P2,
            text: text.into(),
        },
    }
}

/// Two findings around a red check: findings at problems 0 and 2.
fn fix() -> Task {
    Task::Fix {
        repo: "owner/repo".into(),
        pr: 7,
        head: head(),
        problems: vec![
            finding("first"),
            Problem::RedCheck {
                check: "gate".into(),
                text: "red".into(),
            },
            finding("second"),
        ],
    }
}

fn judge(why: Vec<Trigger>) -> Task {
    Task::Judge {
        repo: "owner/repo".into(),
        pr: 7,
        head: head(),
        against: "criteria".into(),
        author: Identity {
            engine: "author-engine".into(),
            session: Some("author".into()),
        },
        reviewers: vec![],
        why,
        context: "a".repeat(64),
    }
}

fn proof() -> Task {
    Task::Proof {
        repo: "owner/repo".into(),
        head: head(),
        tree: "1".repeat(40),
        checks: vec!["gate".into()],
    }
}

fn prepare() -> Task {
    Task::Prepare {
        repo: "owner/repo".into(),
        head: head(),
        tree: "1".repeat(40),
        checks: vec!["gate".into()],
    }
}

fn refused(plugin: &Scripted, task: Task, diagnostic: &str) {
    let error = plugin.plugin().unwrap().call(task).unwrap_err();
    assert!(error.0.contains(diagnostic), "{error}");
}

fn fix_answer(result: Value, answers: Value) -> Value {
    answer(
        CONTRACT,
        "fixer",
        json!({"kind": "fix", "result": result, "answers": answers}),
    )
}

#[test]
fn a_v3_plugin_keeps_its_wire_and_passes_check() {
    let v3 = &CONTRACTS[..3];
    let plugin = scripted(
        describe(v3, &["review", "fix", "judge", "proof"], &[], json!([])),
        &[
            answer(CONTRACT_V3, "r", json!({"kind": "review", "findings": []})),
            answer(
                CONTRACT_V3,
                "f",
                json!({"kind": "fix", "result": {"status": "unable", "why": "no"}}),
            ),
            answer(
                CONTRACT_V3,
                "j",
                json!({"kind": "judge", "verdict": "pass", "why": "ok"}),
            ),
            answer(
                CONTRACT_V3,
                "p",
                json!({"kind": "proof", "results": [
                    {"check": "fixture-pass", "verdict": "pass", "why": "ok"},
                    {"check": "fixture-fail", "verdict": "fail", "why": "no"},
                ]}),
            ),
        ],
    );
    let checked = plugin.plugin().unwrap().check().unwrap();
    assert_eq!(checked, [Kind::Review, Kind::Fix, Kind::Judge, Kind::Proof]);
    let requests = plugin.requests();
    // The v4-only fixtures (a fix with a finding, prepare) aren't sent.
    assert_eq!(requests.len(), 4);
    for request in &requests {
        assert_eq!(request["contract"], CONTRACT_V3);
    }
    let fields = |r: &Value| r.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert!(
        !fields(&requests[1]).contains(&"pr".into()),
        "{}",
        requests[1]
    );
    for gone in ["pr", "why", "context"] {
        assert!(
            !fields(&requests[2]).contains(&gone.into()),
            "{}",
            requests[2]
        );
    }
    assert!(
        !fields(&requests[3]).contains(&"head".into()),
        "{}",
        requests[3]
    );
    // A review's own `pr` was always on the wire.
    assert_eq!(requests[0]["pr"], 1);
}

#[test]
fn a_v4_check_sends_every_new_field_and_kind() {
    let plugin = scripted(
        v4_program(),
        &[
            answer(CONTRACT, "r", json!({"kind": "review", "findings": []})),
            fix_answer(json!({"status": "unable", "why": "no"}), json!([])),
            fix_answer(
                json!({"status": "unchanged"}),
                json!([{"problem": 0, "fixed": false, "reply": "it stands"}]),
            ),
            answer(
                CONTRACT,
                "j",
                json!({"kind": "judge", "verdict": "pass", "why": "ok"}),
            ),
            answer(
                CONTRACT,
                "p",
                json!({"kind": "proof", "head": head(), "results": [
                    {"check": "fixture-pass", "verdict": "pass", "why": "ok"},
                    {"check": "fixture-fail", "verdict": "fail", "why": "no"},
                ]}),
            ),
            answer(
                CONTRACT,
                "q",
                json!({"kind": "prepare", "ready": true, "slot": false, "why": "nothing to do"}),
            ),
        ],
    );
    let checked = plugin.plugin().unwrap().check().unwrap();
    assert_eq!(
        checked,
        [
            Kind::Review,
            Kind::Fix,
            Kind::Judge,
            Kind::Proof,
            Kind::Prepare
        ]
    );
    let requests = plugin.requests();
    assert_eq!(requests[1]["pr"], 1);
    assert_eq!(requests[2]["problems"][0]["kind"], "finding");
    assert_eq!(requests[3]["why"], json!(["asked"]));
    assert_eq!(requests[3]["context"], "3".repeat(64));
    assert_eq!(requests[3]["pr"], 1);
    assert_eq!(requests[4]["head"], head());
    assert_eq!(requests[5]["kind"], "prepare");
    assert_eq!(requests[5]["tree"], "1".repeat(40));
}

#[test]
fn a_fix_must_answer_each_finding_once_in_order() {
    let pushed = json!({"status": "pushed", "head": "4".repeat(40)});
    let reply = |problem: usize| json!({"problem": problem, "fixed": true, "reply": "done"});
    for answers in [
        json!(null),
        json!([]),
        json!([reply(0)]),
        json!([reply(0), reply(2), reply(2)]),
        json!([reply(2), reply(0)]),
        json!([reply(0), reply(1)]),
        json!([reply(0), {"problem": 2, "fixed": true, "reply": " "}]),
    ] {
        let mut wire = fix_answer(pushed.clone(), answers.clone());
        if answers.is_null() {
            wire.as_object_mut().unwrap().remove("answers");
        }
        let plugin = scripted(v4_program(), &[wire]);
        let error = plugin.plugin().unwrap().call(fix()).unwrap_err();
        assert!(
            error.0.contains("answer each finding"),
            "{answers}: {error}"
        );
    }
    let plugin = scripted(
        v4_program(),
        &[fix_answer(pushed, json!([reply(0), reply(2)]))],
    );
    plugin.plugin().unwrap().call(fix()).unwrap();
}

#[test]
fn unchanged_and_unable_carry_no_fixed_answer() {
    let fixed = json!([
        {"problem": 0, "fixed": false, "reply": "it stands"},
        {"problem": 2, "fixed": true, "reply": "done"},
    ]);
    for result in [
        json!({"status": "unchanged"}),
        json!({"status": "unable", "why": "no"}),
    ] {
        let plugin = scripted(v4_program(), &[fix_answer(result, fixed.clone())]);
        refused(&plugin, fix(), "a fixed answer needs a pushed head");
    }
    let plugin = scripted(
        v4_program(),
        &[fix_answer(
            json!({"status": "unchanged"}),
            json!([
                {"problem": 0, "fixed": false, "reply": "it stands"},
                {"problem": 2, "fixed": false, "reply": "so does this"},
            ]),
        )],
    );
    plugin.plugin().unwrap().call(fix()).unwrap();
}

#[test]
fn an_older_fix_carries_neither_answers_nor_unchanged() {
    let v3 = describe(&CONTRACTS[..3], &["fix"], &[], json!([]));
    let unchanged = answer(
        CONTRACT_V3,
        "f",
        json!({"kind": "fix", "result": {"status": "unchanged"}}),
    );
    refused(
        &scripted(v3.clone(), &[unchanged]),
        fix(),
        "unchanged fix needs conary-plugin v4",
    );
    let answered = answer(
        CONTRACT_V3,
        "f",
        json!({"kind": "fix", "result": {"status": "unable", "why": "no"}, "answers": []}),
    );
    refused(
        &scripted(v3, &[answered]),
        fix(),
        "fix answers need conary-plugin v4",
    );
}

#[test]
fn a_judge_request_needs_its_pr_why_and_context() {
    let pass = answer(
        CONTRACT,
        "j",
        json!({"kind": "judge", "verdict": "pass", "why": "ok"}),
    );
    let plugin = scripted(v4_program(), &[pass]);
    refused(&plugin, judge(vec![]), "nonempty, unique why");
    refused(
        &plugin,
        judge(vec![Trigger::NoCi, Trigger::NoCi]),
        "nonempty, unique why",
    );
    let mut task = judge(vec![Trigger::Asked]);
    if let Task::Judge { pr, .. } = &mut task {
        *pr = 0;
    }
    refused(&plugin, task, "needs its PR");
    for bad in ["", "A".repeat(64).as_str(), "a".repeat(40).as_str()] {
        let mut task = judge(vec![Trigger::Asked]);
        if let Task::Judge { context, .. } = &mut task {
            *context = bad.into();
        }
        refused(&plugin, task, "context digest");
    }
    // Refused before the plugin ever ran.
    assert!(plugin.requests().is_empty());
    let mut unknown =
        serde_json::to_value(Request::work(CONTRACT, judge(vec![Trigger::Asked]))).unwrap();
    unknown["why"] = json!(["vibes"]);
    assert!(serde_json::from_value::<Request>(unknown).is_err());
    // Every word of the vocabulary goes out as it is spelled in the docs.
    let every = vec![
        Trigger::R2,
        Trigger::AuthorEngine,
        Trigger::HiddenFiles,
        Trigger::NoCi,
        Trigger::Asked,
        Trigger::RoundLimit,
        Trigger::MajorDependency,
    ];
    plugin.plugin().unwrap().call(judge(every)).unwrap();
    assert_eq!(
        plugin.requests()[0]["why"],
        json!([
            "r2",
            "author_engine",
            "hidden_files",
            "no_ci",
            "asked",
            "round_limit",
            "major_dependency"
        ])
    );
}

#[test]
fn a_proof_names_its_head_both_ways() {
    let results = json!([{"check": "gate", "verdict": "pass", "why": "ok"}]);
    let proof_answer = |contract: &str, head: Option<String>| {
        let mut body = json!({"kind": "proof", "results": results});
        if let Some(head) = head {
            body["head"] = json!(head);
        }
        answer(contract, "p", body)
    };
    for head in [None, Some("3".repeat(40))] {
        refused(
            &scripted(v4_program(), &[proof_answer(CONTRACT, head)]),
            proof(),
            "must name the head it ran on",
        );
    }
    scripted(v4_program(), &[proof_answer(CONTRACT, Some(head()))])
        .plugin()
        .unwrap()
        .call(proof())
        .unwrap();
    let v3 = describe(&CONTRACTS[..3], &["proof"], &[], json!([]));
    for head in [head(), "3".repeat(40)] {
        refused(
            &scripted(v3.clone(), &[proof_answer(CONTRACT_V3, Some(head))]),
            proof(),
            "names its head only in conary-plugin v4",
        );
    }
    scripted(v3, &[proof_answer(CONTRACT_V3, None)])
        .plugin()
        .unwrap()
        .call(proof())
        .unwrap();
    let mut headless = proof();
    if let Task::Proof { head, .. } = &mut headless {
        head.clear();
    }
    refused(
        &scripted(v4_program(), &[]),
        headless,
        "full Git object IDs",
    );
}

#[test]
fn prepare_is_for_proof_programs_that_speak_v4() {
    let model = json!([{"id": "m", "efforts": []}]);
    for (describe, diagnostic) in [
        (
            describe(&CONTRACTS, &["review"], &["prepare"], model.clone()),
            "invalid declared models",
        ),
        (
            describe(&CONTRACTS, &["review"], &["prepare"], json!([])),
            "needs the proof kind",
        ),
        (
            describe(&CONTRACTS, &["proof", "prepare"], &[], json!([])),
            "belongs in v4_kinds",
        ),
        (
            describe(&CONTRACTS[..3], &["proof"], &["prepare"], json!([])),
            "prepare kind needs conary-plugin v4",
        ),
    ] {
        let error = scripted(describe, &[]).plugin().err().unwrap();
        assert!(error.0.contains(diagnostic), "{error}");
    }
}

#[test]
fn a_prepare_answer_is_never_a_verdict() {
    let ready = json!({"kind": "prepare", "ready": true, "slot": true, "why": "built"});
    let mut with_results = ready.clone();
    with_results["results"] = json!([{"check": "gate", "verdict": "pass", "why": "ok"}]);
    refused(
        &scripted(v4_program(), &[answer(CONTRACT, "q", with_results)]),
        prepare(),
        "never a verdict",
    );
    let proof = json!({"kind": "proof", "head": head(), "results": [
        {"check": "gate", "verdict": "pass", "why": "ok"}
    ]});
    refused(
        &scripted(v4_program(), &[answer(CONTRACT, "q", proof)]),
        prepare(),
        "never a verdict",
    );
    let mut blank = ready.clone();
    blank["why"] = json!(" ");
    refused(
        &scripted(v4_program(), &[answer(CONTRACT, "q", blank)]),
        prepare(),
        "does not match request or required reason",
    );
    let plugin = scripted(v4_program(), &[answer(CONTRACT, "q", ready)]);
    let response = plugin.plugin().unwrap().call(prepare()).unwrap();
    assert!(matches!(
        response.answer,
        Answer::Prepare {
            ready: true,
            slot: true,
            ..
        }
    ));
}
