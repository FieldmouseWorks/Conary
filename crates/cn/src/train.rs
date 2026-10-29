//! `cn train`: a repo's train (`docs/DESIGN.md` section 4.7), for the
//! steward to run as its App until conaryd runs the landing flow. It joins
//! the changes the steward settled, builds and splits the candidates, reads
//! their gates, and lands the green ones.

use clap::Subcommand;
use serde_json::{Value, json};

use conary_core::ledger::{Candidate, Ledger};
use conary_core::op::{Left, Repo};
use conary_core::{CandidateId, ChangeId};
use conary_github::train::{self, Joined, Now, Settings, Stepped};

use crate::{Cli, GithubArgs, Result, read, word, write, write_notes};

#[derive(Subcommand)]
pub enum TrainCommand {
    /// Put a PR in its repo's train: everything but the gate says its head
    /// can land. Refused when the PR's head on GitHub is another; held when
    /// it left as it is (red alone, a conflict, a refused merge) and nothing
    /// has moved since; left out when it targets another branch than the
    /// default one.
    Join {
        /// The repository, as owner/name.
        repo: Repo,
        /// The PR's number.
        pr: u64,
        /// The head that was settled.
        #[arg(long)]
        head: String,
        /// It touches a one-way path (R2): it rides in a candidate of its
        /// own, which lands only while trunk is still the candidate's base.
        #[arg(long)]
        alone: bool,
        #[command(flatten)]
        github: GithubArgs,
    },
    /// Take a PR out of its repo's train.
    Leave {
        /// The repository, as owner/name.
        repo: Repo,
        /// The PR's number.
        pr: u64,
        /// unready (it can't land but for the gate any more), conflict (its
        /// head won't merge) or refused (GitHub refused its merge).
        #[arg(long, value_parser = spelled::<Left>)]
        why: Left,
        #[command(flatten)]
        github: GithubArgs,
    },
    /// One step of a repo's train, to run on each of its events: finish the
    /// candidates whose landings all came through, build the next if none
    /// is in flight, read the gate of the one that is and say its verdict,
    /// land a green one's PRs in order, and delete the branches of every
    /// other. Says what it did and where the train is: idle, gating (on its
    /// train branch) or landing (waiting on GitHub to show a merge).
    Step {
        /// The repository, as owner/name.
        repo: Repo,
        /// The trunk branch candidates are built on.
        #[arg(long, default_value = "main")]
        trunk: String,
        /// The most changes a candidate holds.
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u64).range(1..))]
        size: u64,
        /// A check the gate waits for before it can be green, even before it
        /// reports: a proof job's. Repeat for each.
        #[arg(long = "require", value_name = "CHECK")]
        require: Vec<String>,
        #[command(flatten)]
        github: GithubArgs,
    },
    /// Drop a candidate whose gate won't finish, the stall watchdog's last
    /// word: its changes wait again, as they joined.
    Drop {
        /// The repository, as owner/name.
        repo: Repo,
        candidate: CandidateId,
        #[command(flatten)]
        github: GithubArgs,
    },
    /// The repo's train as the ledger holds it.
    Show {
        /// The repository, as owner/name.
        repo: Repo,
    },
}

/// A word as the ledger spells it: `unready`, `green`.
fn spelled<T: serde::de::DeserializeOwned>(s: &str) -> std::result::Result<T, String> {
    serde_json::from_value(Value::String(s.to_owned())).map_err(|e| e.to_string())
}

pub fn run(cli: &Cli, command: &TrainCommand) -> Result<()> {
    let (repo, github) = match command {
        TrainCommand::Show { repo } => {
            let ledger = read(cli)?;
            print(cli, show(&ledger, repo), || human_show(&ledger, repo));
            return Ok(());
        }
        TrainCommand::Join { repo, github, .. }
        | TrainCommand::Leave { repo, github, .. }
        | TrainCommand::Step { repo, github, .. }
        | TrainCommand::Drop { repo, github, .. } => (repo, github),
    };
    let (mut a, api, config) = write(cli, github)?;
    let store = &mut a.store;
    let done: Result<(Value, String)> = match command {
        TrainCommand::Join {
            pr, head, alone, ..
        } => train::join(store, &api, &config, repo, *pr, head, *alone)
            .map(|joined| match joined {
                Joined::Joined | Joined::Already => {
                    let new = joined == Joined::Joined;
                    let said = if new { "joined" } else { "already in" };
                    (
                        json!({"pr": pr, "joined": new}),
                        format!("{repo}#{pr}: {said} the train at {head}"),
                    )
                }
                Joined::Held(why) => (
                    json!({"pr": pr, "joined": false, "held": word(why)}),
                    format!("{repo}#{pr}: held out of the train: {why}"),
                ),
                Joined::Elsewhere(base) => (
                    json!({"pr": pr, "joined": false, "targets": base}),
                    format!(
                        "{repo}#{pr}: not in the train: it targets {base}, not the default branch"
                    ),
                ),
            })
            .map_err(Into::into),
        TrainCommand::Leave { pr, why, .. } => train::leave(store, repo, *pr, *why)
            .map(|left| {
                let said = if left { "left" } else { "wasn't in" };
                (
                    json!({"pr": pr, "left": left}),
                    format!("{repo}#{pr}: {said} the train"),
                )
            })
            .map_err(Into::into),
        TrainCommand::Step {
            trunk,
            size,
            require,
            ..
        } => {
            let settings = Settings {
                trunk: trunk.clone(),
                size: usize::try_from(*size)?,
                require: require.clone(),
            };
            train::step(store, &api, &api, &config, repo, &settings)
                .map(|s| (stepped(&s), human_stepped(repo, &s)))
                .map_err(Into::into)
        }
        TrainCommand::Drop { candidate, .. } => train::drop_candidate(store, repo, *candidate)
            .map(|()| {
                (
                    json!({"candidate": candidate.to_string(), "verdict": "dropped"}),
                    format!("{repo}: candidate {candidate} dropped"),
                )
            })
            .map_err(Into::into),
        TrainCommand::Show { .. } => unreachable!("shown above"),
    };
    // What was recorded before a failure is anchored all the same.
    a.publish(&api)?;
    write_notes(&api, &a, github, repo)?;
    let (json, human) = done?;
    print(cli, json, || human);
    Ok(())
}

fn print(cli: &Cli, json: Value, human: impl FnOnce() -> String) {
    if cli.human {
        println!("{}", human());
    } else {
        println!("{json}");
    }
}

fn stepped(s: &Stepped) -> Value {
    serde_json::to_value(s).expect("a step serializes")
}

fn human_stepped(repo: &Repo, s: &Stepped) -> String {
    let mut lines = Vec::new();
    for c in &s.conflicts {
        let onto = if c.onto.is_empty() {
            String::new()
        } else {
            format!(" and {}", list(&c.onto))
        };
        lines.push(format!(
            "{repo}#{}: won't merge onto {}{onto}, left the train",
            c.pr, c.base
        ));
    }
    for (id, verdict) in &s.verdicts {
        lines.push(format!("{repo}: candidate {id} {}", word(verdict)));
    }
    for r in &s.red {
        let checks: Vec<&str> = r.failed.iter().map(|f| f.check.as_str()).collect();
        lines.push(format!(
            "{repo}#{}: red alone ({}), left the train",
            r.pr,
            checks.join(", ")
        ));
    }
    for pr in &s.landed {
        lines.push(format!("{repo}#{pr}: merged"));
    }
    for r in &s.refused {
        lines.push(format!(
            "{repo}#{}: GitHub refused its merge ({}), left the train",
            r.pr, r.why
        ));
    }
    for id in &s.finished {
        lines.push(format!("{repo}: candidate {id} landed whole"));
    }
    lines.push(match &s.now {
        Now::Idle => format!("{repo}: nothing in the train"),
        Now::Gating {
            candidate,
            branch,
            prs,
            waiting,
            ..
        } => {
            let on = if waiting.is_empty() {
                "no check yet".to_owned()
            } else {
                waiting.join(", ")
            };
            format!(
                "{repo}: candidate {candidate} gating on {branch}: {}, waiting on {on}",
                list(prs)
            )
        }
        Now::Landing {
            candidate,
            landings,
        } => {
            let prs: Vec<u64> = landings.iter().map(|l| l.pr).collect();
            let first = list(&prs[..prs.len().min(1)]);
            format!(
                "{repo}: candidate {candidate} green, landing {}: waiting on GitHub to show {first} merged",
                list(&prs)
            )
        }
    });
    lines.join("\n")
}

fn list(prs: &[u64]) -> String {
    prs.iter()
        .map(|pr| format!("#{pr}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn pr_of(ledger: &Ledger, change: ChangeId) -> u64 {
    ledger.change(change).map_or(0, |c| c.pr)
}

fn candidate(ledger: &Ledger, c: &Candidate) -> Value {
    json!({
        "candidate": c.id.to_string(),
        "state": serde_json::to_value(&c.state).expect("a state serializes"),
        "prs": c.changes.iter().map(|m| pr_of(ledger, m.change)).collect::<Vec<_>>(),
        "landed": c.landed.iter().map(|id| pr_of(ledger, *id)).collect::<Vec<_>>(),
        "base": c.base.to_string(),
        "commit": c.commit.to_string(),
        "of": c.of.map(|id| id.to_string()),
        "alone": c.alone,
        "same_tree": c.same_tree(),
    })
}

fn show(ledger: &Ledger, repo: &Repo) -> Value {
    let Some(t) = ledger.train(repo) else {
        return json!({"joined": [], "in_flight": null, "candidates": []});
    };
    json!({
        "joined": t.joined().iter().map(|m| json!({
            "pr": pr_of(ledger, m.change),
            "change": m.change.to_string(),
            "revision": m.revision.to_string(),
            "alone": t.alone(m.change),
        })).collect::<Vec<_>>(),
        "in_flight": t.in_flight().map(|id| id.to_string()),
        "candidates": t.candidates().iter()
            .filter_map(|id| ledger.candidate(*id))
            .map(|c| candidate(ledger, c))
            .collect::<Vec<_>>(),
    })
}

fn human_show(ledger: &Ledger, repo: &Repo) -> String {
    let Some(t) = ledger.train(repo) else {
        return format!("{repo}: no train");
    };
    let joined: Vec<u64> = t.joined().iter().map(|m| pr_of(ledger, m.change)).collect();
    let mut lines = vec![format!("{repo}: in the train {}", list(&joined))];
    for c in t.candidates().iter().filter_map(|id| ledger.candidate(*id)) {
        let prs: Vec<u64> = c.changes.iter().map(|m| pr_of(ledger, m.change)).collect();
        let state = serde_json::to_value(&c.state).expect("a state serializes");
        let state = state["state"].as_str().unwrap_or_default().to_owned();
        lines.push(format!("  {} {state:<8} {}", c.id, list(&prs)));
    }
    lines.join("\n")
}
