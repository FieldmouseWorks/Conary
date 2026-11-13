//! Copy this executable to start a plugin. See docs/plugins.md.
use conary_core::plugin::*;
use std::io;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let request: Request = serde_json::from_reader(io::stdin())?;
    if ![CONTRACT, CONTRACT_V2, CONTRACT_V1].contains(&request.contract.as_str()) {
        return Err("unsupported contract".into());
    }
    let session = match &request.task {
        Task::Fix { .. } => "example-fixer",
        Task::Judge { .. } => "example-judge",
        _ => "example",
    };
    let contract = request.contract.clone();
    let answer = match request.task {
        Task::Describe => Answer::Describe {
            versions: vec![CONTRACT_V1.into(), CONTRACT_V2.into(), CONTRACT.into()],
            kinds: vec![Kind::Review, Kind::Fix, Kind::Judge, Kind::Proof],
            v2_kinds: vec![Kind::Job],
            // A program: no models, so it may advertise `proof`.
            models: vec![],
            capabilities: Some(Capabilities {
                sandbox: Sandbox::None,
                network: false,
                devices: vec![],
                mcp: false,
            }),
            meters: vec![],
        },
        Task::Job { .. } => Answer::Job {
            summary: "Deterministic example job".into(),
            next: Next::Done,
        },
        // Reserved, in-memory flow demonstration: the host simulates this
        // push. No real repository is written or claimed to be written.
        Task::Review { repo, head, .. }
            if repo == "conary-test/flow-simulation" && head.ends_with('3') =>
        {
            Answer::Review {
                findings: vec![Finding {
                    path: "src/lib.rs".into(),
                    line: 1,
                    severity: Severity::P1,
                    text: "Demonstration finding on the first simulated head".into(),
                }],
            }
        }
        Task::Fix { repo, .. } if repo == "conary-test/flow-simulation" => Answer::Fix {
            result: FixResult::Pushed {
                head: format!("{:0>40}", 4),
            },
        },
        Task::Review { .. } => Answer::Review { findings: vec![] },
        Task::Fix { .. } => Answer::Fix {
            result: FixResult::Unable {
                why: "This example never writes a repository".into(),
            },
        },
        Task::Judge { .. } => Answer::Judge {
            verdict: Judgment::Pass,
            why: "Deterministic example judgment".into(),
        },
        Task::Proof { checks, .. } => Answer::Proof {
            results: checks
                .into_iter()
                .map(|check| CheckResult {
                    verdict: if check == "fixture-fail" {
                        Verdict::Fail
                    } else {
                        Verdict::Pass
                    },
                    check,
                    why: "Deterministic example result, not evidence".into(),
                })
                .collect(),
        },
    };
    serde_json::to_writer(
        io::stdout(),
        &Response {
            contract,
            execution: request.execution.map(|execution| Executed {
                model: None,
                effort: None,
                context: execution.context,
            }),
            by: Identity {
                engine: "conary-example".into(),
                session: Some(session.into()),
            },
            answer,
        },
    )?;
    Ok(())
}
