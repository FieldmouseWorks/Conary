//! Configuration -> durable invocation -> negotiated plugin -> verdict provenance.
use conary_core::plugin::{Choice, Context, Identity, Limits, Plugin, Task};
use conary_github::flow::{Config, jobs};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn fixture(mode: &str) -> (tempfile::TempDir, PathBuf, Plugin) {
    // Keep aliases on the executable's filesystem: /tmp may be another mount.
    let dir = tempfile::tempdir_in(
        Path::new(env!("CARGO_BIN_EXE_conary-execution-test-plugin"))
            .parent()
            .unwrap(),
    )
    .unwrap();
    let path = dir.path().join(mode);
    std::fs::hard_link(env!("CARGO_BIN_EXE_conary-execution-test-plugin"), &path).unwrap();
    let plugin = Plugin::describe(
        &path,
        Limits {
            deadline: Duration::from_secs(10),
            response_bytes: 65_536,
        },
    )
    .unwrap();
    (dir, path, plugin)
}

fn judge() -> Task {
    Task::Judge {
        repo: "fixture/repo".into(),
        head: "1".repeat(40),
        against: "criteria".into(),
        author: Identity {
            engine: "configured-engine".into(),
            session: Some("writer-session".into()),
        },
        reviewers: vec![Identity {
            engine: "another-engine".into(),
            session: Some("reviewer-session".into()),
        }],
    }
}

fn choice() -> Choice {
    Choice {
        engine: "configured-engine".into(),
        model: Some("fixture-model".into()),
        effort: Some("deliberate".into()),
    }
}

fn worker(path: &Path, config: &Config, task: Task) -> jobs::Completion {
    let invocation = path.with_extension("request");
    std::fs::write(
        &invocation,
        serde_json::to_vec(&jobs::Invocation {
            executable: path.into(),
            choice: config
                .execution
                .get(&conary_core::plugin::Kind::Judge)
                .cloned(),
            task,
            deadline: jobs::now() + 20,
            response_bytes: 65_536,
        })
        .unwrap(),
    )
    .unwrap();
    jobs::worker(&invocation).unwrap();
    serde_json::from_slice(&std::fs::read(invocation.with_extension("answer")).unwrap()).unwrap()
}

#[test]
fn configured_engine_model_effort_reach_judge_and_persist_with_verdict() {
    for (engine, model, effort, mode) in [
        ("configured-engine", "fixture-model", "deliberate", "normal"),
        ("other-engine", "alternate-model", "quick", "other-engine"),
    ] {
        let (_dir, path, _) = fixture(mode);
        // The same JSON interface the CLI and conaryd read; no engine default in code.
        let config: Config = serde_json::from_value(serde_json::json!({
            "execution": { "judge": { "engine":engine, "model":model, "effort":effort } }
        }))
        .unwrap();
        let response = worker(&path, &config, judge()).result.unwrap().unwrap();
        assert_eq!(response.by.engine, engine);
        let actual = response.execution.unwrap();
        assert_eq!(actual.model.as_deref(), Some(model));
        assert_eq!(actual.effort.as_deref(), Some(effort));
        assert_eq!(actual.context, Context::Fresh);
        assert!(matches!(
            response.answer,
            conary_core::plugin::Answer::Judge { .. }
        ));
    }
}

#[test]
fn judge_rejects_foreign_writer_and_reviewer_continuations_and_all_forks() {
    let (_dir, _path, plugin) = fixture("normal");
    for session in ["writer-session", "reviewer-session", "unknown-session"] {
        for context in [
            Context::Continue {
                session: session.into(),
            },
            Context::Fork {
                session: session.into(),
            },
        ] {
            let error = plugin
                .call_with(judge(), Some(choice()), context, None)
                .unwrap_err();
            assert!(error.0.contains("judge cannot"), "{error}");
        }
    }
}

#[test]
fn judge_can_continue_only_its_own_validated_session_in_the_same_scope() {
    let (_dir, _path, plugin) = fixture("normal");
    let response = plugin
        .call_with(judge(), Some(choice()), Context::Fresh, None)
        .unwrap();
    let context = Context::Continue {
        session: response.by.session.unwrap(),
    };
    assert!(
        plugin
            .call_with(judge(), Some(choice()), context.clone(), None)
            .is_ok()
    );
    let mut other_scope = judge();
    if let Task::Judge { head, .. } = &mut other_scope {
        *head = "2".repeat(40);
    }
    assert!(
        plugin
            .call_with(other_scope, Some(choice()), context.clone(), None)
            .is_err()
    );
    // A separate job's handle cannot import even another judge's session.
    let (_other_dir, _other_path, other) = fixture("normal");
    assert!(
        other
            .call_with(judge(), Some(choice()), context, None)
            .is_err()
    );
    assert!(
        plugin
            .call(judge())
            .unwrap_err()
            .0
            .contains("initial judge reused")
    );
}

#[test]
fn judge_rejects_disguised_foreign_sessions_and_execution_changes() {
    for (mode, reason) in [
        ("writer", "author cannot grade"),
        ("reviewer", "writer or reviewer session"),
        ("hidden-fork", "actual execution differs"),
        ("wrong-model", "actual execution differs"),
        ("no-provenance", "did not report actual execution"),
    ] {
        let (_dir, _path, plugin) = fixture(mode);
        let error = plugin
            .call_with(judge(), Some(choice()), Context::Fresh, None)
            .unwrap_err();
        assert!(error.0.contains(reason), "{mode}: {error}");
    }
}

#[test]
fn execution_choices_are_checked_for_review_and_fix_too() {
    let (_dir, _path, plugin) = fixture("normal");
    for task in [
        Task::Review {
            repo: "fixture/repo".into(),
            pr: 1,
            base: "2".repeat(40),
            head: "1".repeat(40),
            author: Identity {
                engine: "other".into(),
                session: Some("writer".into()),
            },
        },
        Task::Fix {
            repo: "fixture/repo".into(),
            head: "1".repeat(40),
            problems: vec![],
        },
        judge(),
    ] {
        let response = plugin
            .call_with(task.clone(), Some(choice()), Context::Fresh, None)
            .unwrap();
        assert_eq!(response.execution.unwrap().model, choice().model);
        let mut invalid = choice();
        invalid.engine = "unconfigured-engine".into();
        assert!(
            plugin
                .call_with(task.clone(), Some(invalid), Context::Fresh, None)
                .unwrap_err()
                .0
                .contains("choice is not offered")
        );
        let mut invalid = choice();
        invalid.effort = Some("unoffered".into());
        assert!(
            plugin
                .call_with(task, Some(invalid), Context::Fresh, None)
                .is_err()
        );
    }
}

#[test]
fn fresh_same_engine_judge_does_not_need_an_author_session_from_untrusted_trailers() {
    let (_dir, _path, plugin) = fixture("normal");
    let mut task = judge();
    if let Task::Judge { author, .. } = &mut task {
        author.session = None;
    }
    assert!(plugin.call(task).is_ok());
}

#[test]
fn legacy_judges_cannot_silently_ignore_the_session_boundary() {
    let (_dir, _path, plugin) = fixture("legacy");
    assert!(
        plugin
            .call(judge())
            .unwrap_err()
            .0
            .contains("need conary-plugin v3")
    );
}

#[test]
fn execution_configuration_rejects_unknown_kinds() {
    assert!(
        serde_json::from_value::<Config>(serde_json::json!({
            "execution": {"judeg": {"engine":"fixture", "model":null, "effort":null}}
        }))
        .is_err()
    );
}
