//! Copy this executable to start a plugin. See docs/plugins.md.
use conary_core::plugin::*;
use std::io;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let request: Request = serde_json::from_reader(io::stdin())?;
    if version(&request.contract).is_none() {
        return Err("unsupported contract".into());
    }
    let session = match &request.task {
        Task::Fix { .. } => "example-fixer",
        Task::Judge { .. } => "example-judge",
        _ => "example",
    };
    let contract = request.contract.clone();
    let v4 = contract == CONTRACT;
    let answer = match request.task {
        Task::Describe => Answer::Describe {
            versions: CONTRACTS.map(String::from).to_vec(),
            kinds: vec![Kind::Review, Kind::Fix, Kind::Judge, Kind::Proof],
            v2_kinds: vec![Kind::Job],
            v4_kinds: vec![Kind::Prepare],
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
        Task::Fix { repo, problems, .. } if repo == "conary-test/flow-simulation" => Answer::Fix {
            result: FixResult::Pushed {
                head: format!("{:0>40}", 4),
            },
            answers: v4.then(|| answers(&problems, true)),
        },
        Task::Review { .. } => Answer::Review { findings: vec![] },
        // It never writes, so it answers each finding without a change.
        Task::Fix { problems, .. }
            if v4
                && problems
                    .iter()
                    .any(|p| matches!(p, Problem::Finding { .. })) =>
        {
            Answer::Fix {
                result: FixResult::Unchanged,
                answers: Some(answers(&problems, false)),
            }
        }
        Task::Fix { problems, .. } => Answer::Fix {
            result: FixResult::Unable {
                why: "This example never writes a repository".into(),
            },
            answers: v4.then(|| answers(&problems, false)),
        },
        Task::Judge { .. } => Answer::Judge {
            verdict: Judgment::Pass,
            why: "Deterministic example judgment".into(),
        },
        Task::Prepare { .. } => Answer::Prepare {
            ready: true,
            slot: false,
            why: "The example's checks need nothing prepared".into(),
        },
        Task::Proof { head, checks, .. } => Answer::Proof {
            head: v4.then_some(head),
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

/// One answer for each finding problem, in order.
fn answers(problems: &[Problem], fixed: bool) -> Vec<FindingAnswer> {
    problems
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Problem::Finding { .. }))
        .map(|(problem, _)| FindingAnswer {
            problem,
            fixed,
            reply: if fixed {
                "Fixed in the example's simulated push".into()
            } else {
                "The example never changes code".into()
            },
        })
        .collect()
}
