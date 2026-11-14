//! Deterministic execution-contract fixture; never runs an engine.
use conary_core::plugin::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::current_exe()?
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let request: Request = serde_json::from_reader(std::io::stdin())?;
    let mut session = "judge-session".to_string();
    let mut unidentified = false;
    let mut execution = None;
    let answer = match &request.task {
        Task::Describe => Answer::Describe {
            versions: vec![if mode == "legacy" {
                CONTRACT_V2.into()
            } else {
                CONTRACT.into()
            }],
            kinds: vec![Kind::Review, Kind::Fix, Kind::Judge],
            v2_kinds: vec![Kind::Job],
            models: ["fixture-model", "alternate-model"]
                .into_iter()
                .map(|id| Model {
                    id: id.into(),
                    efforts: vec!["deliberate".into(), "quick".into()],
                    context_tokens: None,
                    max_output_tokens: None,
                })
                .collect(),
            capabilities: Some(Capabilities {
                sandbox: Sandbox::None,
                network: false,
                devices: vec![],
                mcp: false,
            }),
            meters: vec![],
        },
        _ => {
            let run = request.execution.as_ref().expect("v3 work execution");
            let judging = request.task.kind() == Some(Kind::Judge);
            session = match &run.context {
                Context::Continue { session } | Context::Fork { session } => session.clone(),
                Context::Fresh => match &request.task {
                    Task::Fix { .. } => "writer-session",
                    Task::Review { .. } => "reviewer-session",
                    Task::Job { .. } => "job-session",
                    _ => "judge-session",
                }
                .into(),
            };
            if !judging && mode.starts_with("concurrent") {
                let executable = std::env::current_exe()?;
                let directory = executable.parent().unwrap();
                std::fs::write(directory.join("nonjudge-started"), b"started")?;
                let started = std::time::Instant::now();
                while !directory.join("release-nonjudge").exists() {
                    if started.elapsed() > std::time::Duration::from_secs(5) {
                        return Err("fixture was not released".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
            if !judging && ["exit-nonjudge", "concurrent-failure"].contains(&mode.as_str()) {
                std::process::exit(7);
            }
            let choice = run.choice.as_ref();
            execution = Some(Executed {
                model: Some(
                    choice
                        .and_then(|c| c.model.as_deref())
                        .unwrap_or("fixture-model")
                        .into(),
                ),
                effort: Some(
                    choice
                        .and_then(|c| c.effort.as_deref())
                        .unwrap_or("deliberate")
                        .into(),
                ),
                context: run.context.clone(),
            });
            match mode.as_str() {
                "writer" => session = "writer-session".into(),
                "reviewer" => session = "reviewer-session".into(),
                "wrong-model" => execution.as_mut().unwrap().model = Some("alternate-model".into()),
                "hidden-fork" => {
                    execution.as_mut().unwrap().context = Context::Fork {
                        session: "writer-session".into(),
                    }
                }
                "no-provenance" => execution = None,
                "shared" | "invalid-nonjudge" | "no-provenance-nonjudge" => {
                    session = "shared-session".into();
                    if !judging && mode == "no-provenance-nonjudge" {
                        execution = None;
                    }
                }
                "no-session-nonjudge" if !judging => unidentified = true,
                "blank-session-nonjudge" if !judging => session.clear(),
                "fork-nonjudge" if !judging => {
                    execution.as_mut().unwrap().context = Context::Fork {
                        session: "judge-session".into(),
                    }
                }
                "concurrent" | "concurrent-failure" => session = "judge-session".into(),
                _ => {}
            }
            if !judging && mode == "invalid-nonjudge" {
                Answer::Judge {
                    verdict: Judgment::Pass,
                    why: "wrong answer kind".into(),
                }
            } else {
                match request.task {
                    Task::Judge { .. } => Answer::Judge {
                        verdict: Judgment::Pass,
                        why: "fixture judgment".into(),
                    },
                    Task::Review { .. } => Answer::Review { findings: vec![] },
                    Task::Fix { .. } => Answer::Fix {
                        result: FixResult::Unable {
                            why: "fixture never writes".into(),
                        },
                    },
                    Task::Job { .. } => Answer::Job {
                        summary: "fixture job".into(),
                        next: Next::Done,
                    },
                    _ => return Err("unexpected fixture task".into()),
                }
            }
        }
    };
    let engine = if mode == "other-engine" {
        "other-engine"
    } else {
        "configured-engine"
    };
    serde_json::to_writer(
        std::io::stdout(),
        &Response {
            contract: request.contract,
            by: Identity {
                engine: engine.into(),
                session: if unidentified { None } else { Some(session) },
            },
            execution,
            answer,
        },
    )?;
    Ok(())
}
