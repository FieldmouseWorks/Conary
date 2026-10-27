//! `cn`, Conary's command line: a thin layer over conary-core and
//! conary-github. JSON on stdout by default, for agents; `--human` for
//! people.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use conary_core::Ledger;
use conary_core::ledger::Change;
use conary_core::log;
use conary_core::op::Repo;
use conary_core::why::{self, Why};
use conary_github::anchor::{self, Anchored};
use conary_github::{Api, Config, Ingested, ingest};

#[derive(Parser)]
#[command(
    name = "cn",
    about = "Conary: the ledger of every change and why it was made"
)]
struct Cli {
    /// The ledger's directory.
    #[arg(long, global = true, env = "CONARY_LEDGER", default_value_os_t = default_ledger())]
    ledger: PathBuf,
    /// Print for people instead of JSON.
    #[arg(long, global = true)]
    human: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Remote {
    /// The GitHub API's root.
    #[arg(long, env = "GITHUB_API_URL", default_value = "https://api.github.com")]
    api: String,
    /// The repository, as owner/name, whose ref anchors the ledger: `init`
    /// makes it, and every write checks the log against it first and moves
    /// it on after.
    #[arg(long, env = "CONARY_ANCHOR_REPO")]
    anchor_repo: String,
}

#[derive(clap::Args)]
struct GithubArgs {
    #[command(flatten)]
    remote: Remote,
    /// Logins whose review markers are reviews: the steward's account.
    #[arg(long = "reviewer", env = "CONARY_REVIEWERS", value_delimiter = ',')]
    reviewers: Vec<String>,
    /// Status contexts that are the steward's own, not checks.
    #[arg(
        long = "own-context",
        env = "CONARY_OWN_CONTEXTS",
        value_delimiter = ','
    )]
    own_contexts: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Set the ledger up: its key, and its anchor on GitHub. Run again, it
    /// finishes a set-up that stopped partway; a ledger whose key was lost
    /// is refused until the key is back.
    Init {
        #[command(flatten)]
        remote: Remote,
    },
    /// Record a pull request's current state in the ledger.
    Ingest {
        /// The repository, as owner/name.
        repo: Repo,
        /// The PR's number.
        pr: u64,
        #[command(flatten)]
        github: GithubArgs,
    },
    /// Bring a repository's open and recently closed pull requests up to
    /// date: what to run on each of its PR events.
    Sync {
        /// The repository, as owner/name.
        repo: Repo,
        #[command(flatten)]
        github: GithubArgs,
    },
    /// Record every pull request in a repository.
    Backfill {
        /// The repository, as owner/name.
        repo: Repo,
        #[command(flatten)]
        github: GithubArgs,
    },
    /// List the changes in the ledger.
    Log {
        /// Only this repository's.
        #[arg(long)]
        repo: Option<Repo>,
    },
    /// Check the log against its anchor on GitHub.
    Verify {
        #[command(flatten)]
        remote: Remote,
    },
    /// Why a line is the way it is: the change that landed it.
    Why {
        /// FILE:LINE, in the checkout at -C.
        at: String,
        /// The checkout.
        #[arg(short = 'C', default_value = ".")]
        dir: PathBuf,
    },
}

fn default_ledger() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("conary")
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cn: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Ingest { repo, pr, github } => {
            let (mut a, api, config) = write(cli, github)?;
            let got = ingest::pull(&mut a.store, &api, &config, repo, *pr);
            // What was recorded before a failure is anchored all the same.
            a.publish(&api)?;
            print_ingested(cli, repo, *pr, got?, a.store.ledger());
        }
        Command::Sync { repo, github } | Command::Backfill { repo, github } => {
            let (mut a, api, config) = write(cli, github)?;
            let got = if matches!(cli.command, Command::Sync { .. }) {
                ingest::sync(&mut a.store, &api, &config, repo)
            } else {
                ingest::backfill(&mut a.store, &api, &config, repo)
            };
            a.publish(&api)?;
            let (got, store) = (got?, &a.store);
            if cli.human {
                let recorded: usize = got.iter().map(|g| g.recorded).sum();
                println!("{repo}: {} PRs, {recorded} new ops", got.len());
            } else {
                let all: Vec<Value> = got.iter().map(|g| ingested(g, store.ledger())).collect();
                println!("{}", Value::Array(all));
            }
        }
        Command::Init { remote } => {
            let api = Api::new(&remote.api, token()?);
            let made = anchor::init(&cli.ledger, &key_path(cli), &api, &remote.anchor_repo)?;
            let seq = made.anchor.map_or(0, |a| a.seq);
            if cli.human {
                println!(
                    "{}: the ledger, anchored on {} at op {seq}",
                    cli.ledger.display(),
                    remote.anchor_repo
                );
            } else {
                println!(
                    "{}",
                    json!({"anchored": made.anchor.map(|a| a.to_string()), "ops": seq})
                );
            }
        }
        Command::Verify { remote } => {
            let key = log::load_public_key(&log::public_path(&key_path(cli)))?;
            let api = Api::new(&remote.api, token()?);
            let anchor = anchor::verify(&cli.ledger, &key, &api, &remote.anchor_repo)?;
            let seq = anchor.map_or(0, |a| a.seq);
            if cli.human {
                println!(
                    "the log holds op {seq}, the anchor on {}",
                    remote.anchor_repo
                );
            } else {
                println!(
                    "{}",
                    json!({"anchored": anchor.map(|a| a.to_string()), "ops": seq})
                );
            }
        }
        Command::Log { repo } => {
            let ledger = read(cli)?;
            let changes = ledger
                .changes()
                .filter(|c| repo.as_ref().is_none_or(|r| &c.repo == r));
            if cli.human {
                for c in changes {
                    println!("{}", line(c));
                }
            } else {
                println!("{}", Value::Array(changes.map(summary).collect()));
            }
        }
        Command::Why { at, dir } => {
            let (file, n) = at
                .rsplit_once(':')
                .and_then(|(f, n)| Some((f, n.parse::<u32>().ok()?)))
                .ok_or("give the line as FILE:LINE")?;
            let ledger = read(cli)?;
            print_why(cli, &why::why(&ledger, dir, Path::new(file), n)?);
        }
    }
    Ok(())
}

fn key_path(cli: &Cli) -> PathBuf {
    cli.ledger.join("key")
}

fn token() -> Result<String> {
    Ok(std::env::var("GH_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .map_err(|_| "set GH_TOKEN (or GITHUB_TOKEN) to reach GitHub")?)
}

/// The ledger, open for writing once its log holds the anchor on GitHub.
fn write(cli: &Cli, github: &GithubArgs) -> Result<(Anchored, Api, Config)> {
    let api = Api::new(&github.remote.api, token()?);
    let key = log::load_signing_key(&key_path(cli))?;
    let anchored = anchor::open(&cli.ledger, key, &api, &github.remote.anchor_repo)?;
    let config = Config {
        reviewers: github.reviewers.clone(),
        own_contexts: github.own_contexts.clone(),
    };
    Ok((anchored, api, config))
}

/// The ledger, verified against its public key and the anchor its writer
/// kept beside it. Reading takes no lock and never leaves the box.
fn read(cli: &Cli) -> Result<Ledger> {
    let key = log::load_public_key(&log::public_path(&key_path(cli)))
        .map_err(|e| format!("{e} (has `cn init` set up {}?)", cli.ledger.display()))?;
    let ops = log::read_anchored(&cli.ledger, &key)?;
    Ledger::replay(&ops).map_err(|(id, e)| format!("op {id} in the log doesn't apply: {e}").into())
}

fn url(c: &Change) -> String {
    format!("https://github.com/{}/pull/{}", c.repo, c.pr)
}

fn summary(c: &Change) -> Value {
    json!({
        "change": c.id.to_string(),
        "repo": c.repo.to_string(),
        "pr": c.pr,
        "url": url(c),
        "status": c.status(),
        "author": c.author,
        "summary": c.intent.as_ref().map(|i| &i.summary),
        "task": c.intent.as_ref().and_then(|i| i.task.as_ref()),
        "landed": c.landing.as_ref().map(|l| l.after.to_string()),
    })
}

/// A unit enum as the ledger spells it: `landed`, `ci`, `pass`.
fn word(v: impl serde::Serialize) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        other => format!("{other:?}"),
    }
}

fn line(c: &Change) -> String {
    let status = word(c.status());
    let summary = c.intent.as_ref().map_or("", |i| i.summary.as_str());
    format!(
        "{}#{} {status:<6} {} {summary}",
        c.repo, c.pr, c.author.login
    )
}

fn ingested(got: &Ingested, ledger: &Ledger) -> Value {
    let c = ledger.change(got.change).expect("ingest recorded it");
    json!({"change": c.id.to_string(), "pr": c.pr, "url": url(c), "recorded": got.recorded})
}

fn print_ingested(cli: &Cli, repo: &Repo, pr: u64, got: Ingested, ledger: &Ledger) {
    if cli.human {
        println!(
            "{repo}#{pr}: change {}, {} new ops",
            got.change, got.recorded
        );
    } else {
        println!("{}", ingested(&got, ledger));
    }
}

fn print_why(cli: &Cli, why: &Why) {
    if !cli.human {
        let out = json!({
            "repo": why.repo.to_string(),
            "commit": why.commit.to_string(),
            "trunk": why.landed.map(|(_, l)| l.after.to_string()),
            "change": why.landed.map(|(c, _)| {
                let mut v = serde_json::to_value(c).expect("a change serializes");
                v["url"] = json!(url(c));
                v["status"] = json!(c.status());
                v
            }),
        });
        println!("{out}");
        return;
    }
    let Some((c, l)) = why.landed else {
        println!(
            "{}: no landing in the ledger brought it onto trunk (not landed, not ingested, or trunk moved outside Conary)",
            why.commit
        );
        return;
    };
    println!("{}: {}", l.after, url(c));
    if let Some(i) = &c.intent {
        println!("  {}", i.summary);
        if let Some(task) = &i.task {
            println!("  task: {task}");
        }
    }
    let session = c.author.session.as_deref().unwrap_or("no session");
    println!("  by {} ({session})", c.author.login);
    for e in c.evidence.iter().filter(|e| e.revision == l.revision) {
        println!(
            "  {} {}: {} ({})",
            word(e.kind),
            e.check,
            word(e.result),
            e.source
        );
    }
}
