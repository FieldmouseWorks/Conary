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
    let mut execution = None;
    let answer = match &request.task {
        Task::Describe => Answer::Describe {
            versions: vec![if mode == "legacy" {
                CONTRACT_V2.into()
            } else {
                CONTRACT.into()
            }],
            kinds: vec![Kind::Review, Kind::Fix, Kind::Judge],
            v2_kinds: vec![],
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
                _ => {}
            }
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
                _ => return Err("unexpected fixture task".into()),
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
                session: Some(session),
            },
            execution,
            answer,
        },
    )?;
    Ok(())
}
