//! Copy this executable to start a plugin. See docs/plugins.md.
use conary_core::plugin::*;
use std::io;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let request: Request = serde_json::from_reader(io::stdin())?;
    if request.contract != CONTRACT {
        return Err("unsupported contract".into());
    }
    let answer = match request.task {
        Task::Describe => Answer::Describe {
            versions: vec![CONTRACT.into()],
            kinds: vec![Kind::Review, Kind::Fix, Kind::Judge, Kind::Proof],
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
            contract: CONTRACT.into(),
            by: Identity {
                engine: "conary-example".into(),
                session: Some("example".into()),
            },
            answer,
        },
    )?;
    Ok(())
}
