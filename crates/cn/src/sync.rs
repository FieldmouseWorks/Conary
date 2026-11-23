//! `cn sync`: brings repositories up to date on one open of the ledger
//! ([`ingest::sync_all`]), and says how each one ended as it ends, the way
//! `Command::Sync`'s help says.

use serde_json::{Value, json};

use conary_core::op::Repo;
use conary_github::api::unavailable;
use conary_github::ingest::{self, Ended, Target};

use super::{Cli, GithubArgs, Result, links, writer};

/// The repositories a sync didn't finish.
#[derive(Debug)]
pub(crate) struct Unsynced {
    /// Each one, and what stopped it.
    each: Vec<(Repo, String)>,
    /// Whether one failed, not just found GitHub unavailable.
    failed: bool,
}

impl Unsynced {
    /// Whether GitHub being unavailable is all that stopped them: running
    /// it again later is the whole fix.
    pub(crate) fn unavailable(&self) -> bool {
        !self.failed
    }
}

impl std::fmt::Display for Unsynced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let each: Vec<String> = self
            .each
            .iter()
            .map(|(repo, why)| format!("{repo}: {why}"))
            .collect();
        f.write_str(&each.join("\n"))
    }
}

impl std::error::Error for Unsynced {}

pub(crate) fn run(cli: &Cli, repos: &[Repo], github: &GithubArgs) -> Result<()> {
    let (api, config) = writer(cli, github)?;
    let targets: Vec<Target> = repos
        .iter()
        .map(|repo| Target {
            repo: repo.clone(),
            links: links(github, repo),
        })
        .collect();
    let mut unsynced = Unsynced {
        each: Vec::new(),
        failed: false,
    };
    let mut tell = |repo: &Repo, ended: &Ended, stopped: bool| {
        let line = match ended {
            Ended::Done(got) => {
                for refused in &got.refused {
                    eprintln!("{repo}: {refused}");
                }
                if cli.human {
                    let recorded: usize = got.ingested.iter().map(|g| g.recorded).sum();
                    println!("{repo}: {} PRs, {recorded} new ops", got.ingested.len());
                    return;
                }
                let prs: Vec<Value> = got
                    .ingested
                    .iter()
                    .map(|g| {
                        json!({
                            "change": g.change.to_string(),
                            "pr": g.pr,
                            "url": repo.pull_url(g.pr),
                            "recorded": g.recorded,
                        })
                    })
                    .collect();
                let refused: Vec<Value> = got
                    .refused
                    .iter()
                    .map(|r| json!({"pr": r.pr, "why": r.to_string()}))
                    .collect();
                json!({"repo": repo.to_string(), "outcome": "done", "prs": prs, "refused": refused})
            }
            Ended::Unavailable(why) | Ended::Failed(why) => {
                let outcome = if matches!(ended, Ended::Unavailable(_)) {
                    "unavailable"
                } else {
                    unsynced.failed = true;
                    "failed"
                };
                unsynced.each.push((repo.clone(), why.clone()));
                if cli.human {
                    println!("{repo}: {outcome}: {why}");
                    return;
                }
                let mut line = json!({"repo": repo.to_string(), "outcome": outcome, "error": why});
                if stopped {
                    line["stopped"] = json!(true);
                }
                line
            }
        };
        println!("{line}");
    };
    let passed = ingest::sync_all(
        &cli.ledger,
        &github.remote.anchor_repo,
        &api,
        &config,
        &targets,
        |repo, ended| tell(repo, &ended, false),
    );
    if let Err(stopped) = passed {
        let why = format!("not synced: the pass stopped: {}", stopped.error);
        let ended = if unavailable(&stopped) {
            Ended::Unavailable(why)
        } else {
            Ended::Failed(why)
        };
        for repo in &stopped.unsynced {
            tell(repo, &ended, true);
        }
    }
    if unsynced.each.is_empty() {
        Ok(())
    } else {
        Err(unsynced.into())
    }
}
