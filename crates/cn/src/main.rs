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
use conary_core::notes::Links;
use conary_core::op::{Authority, Repo};
use conary_core::why::{self, Why};
use conary_core::{reader, rpc};
use conary_github::anchor::{self, Anchored};
use conary_github::{Api, ApiError, Config, Etags, Ingested, Tokens, ingest, notes};

mod train;

#[derive(Parser)]
#[command(
    name = "cn",
    about = "Conary: the ledger of every change and why it was made",
    after_help = "Exits 75 when GitHub couldn't be asked just now (its rate limit, its own errors, the network): run it again later."
)]
struct Cli {
    /// The ledger's directory.
    #[arg(long, global = true, env = "CONARY_LEDGER", default_value_os_t = log::default_dir())]
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
    /// A command that prints a token for the repository it's given (as
    /// OWNER/NAME, its last argument), for when each repository's can
    /// differ: a GitHub App's are one owner's each. It's split on whitespace
    /// and run without a shell. A token it prints must stay good for five
    /// minutes, and it exits 75 when GitHub can't be asked just now. Without
    /// it, GH_TOKEN (or GITHUB_TOKEN) is every repository's.
    // Help never shows its value: a credential can be one of its arguments.
    #[arg(long, env = "CONARY_TOKEN_COMMAND", hide_env_values = true)]
    token_command: Option<String>,
}

#[derive(clap::Args)]
struct GithubArgs {
    #[command(flatten)]
    remote: Remote,
    /// Logins whose review markers are reviews: the steward's App's bot, SLUG[bot].
    #[arg(long = "reviewer", env = "CONARY_REVIEWERS", value_delimiter = ',')]
    reviewers: Vec<String>,
    /// Status contexts that are the steward's own, not checks.
    #[arg(
        long = "own-context",
        env = "CONARY_OWN_CONTEXTS",
        value_delimiter = ','
    )]
    own_contexts: Vec<String>,
    /// Repositories whose notes may link the sessions and threads their
    /// changes came from. Every other repository's notes leave them out,
    /// since some repos' rules keep them out of their history.
    #[arg(
        long = "session-links",
        env = "CONARY_SESSION_LINKS",
        value_delimiter = ','
    )]
    session_links: Vec<Repo>,
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
    /// Record a pull request's current state in the ledger. Every command
    /// that records also brings the repository's notes up to date.
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
    /// Check the log against its anchor on GitHub, all of its history.
    Verify {
        #[command(flatten)]
        remote: Remote,
    },
    /// A repo's train: the changes ready to land, gated together.
    Train {
        #[command(subcommand)]
        command: train::TrainCommand,
    },
    /// Why a line is the way it is: the change that landed it. From the
    /// ledger when it's set up here, otherwise from the notes on the
    /// checkout's origin, which it fetches.
    Why {
        /// FILE:LINE, in the checkout at -C.
        at: String,
        /// The checkout.
        #[arg(short = 'C', default_value = ".")]
        dir: PathBuf,
    },
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The exit status when GitHub couldn't be asked just now (its rate limit,
/// its own errors, the network), BSD's EX_TEMPFAIL: running it again later
/// is the whole fix, so a caller can book that instead of calling it a
/// failure.
const UNAVAILABLE: u8 = 75;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cn: {e}");
            if unavailable(&*e) {
                ExitCode::from(UNAVAILABLE)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// Whether the error comes down to GitHub being unavailable.
fn unavailable(e: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(Some(e), |e| e.source())
        .any(|e| e.downcast_ref::<ApiError>().is_some_and(|a| a.unavailable))
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Ingest { repo, pr, github } => {
            let (mut a, api, config) = write(cli, github)?;
            let got = ingest::pull(&mut a.store, &api, &config, repo, *pr);
            // What was recorded before a failure is anchored all the same.
            a.publish(&api)?;
            write_notes(&api, &a, github, repo)?;
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
            write_notes(&api, &a, github, repo)?;
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
            let api = Api::new(&remote.api, tokens(remote)?);
            let made = anchor::init(
                &cli.ledger,
                &log::key_path(&cli.ledger),
                &api,
                &remote.anchor_repo,
            )?;
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
            let key = log::load_public_key(&log::public_path(&log::key_path(&cli.ledger)))?;
            let api = Api::new(&remote.api, tokens(remote)?);
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
                println!("{}", Value::Array(changes.map(rpc::summary).collect()));
            }
        }
        Command::Train { command } => train::run(cli, command)?,
        Command::Why { at, dir } => {
            let (file, n) = at
                .rsplit_once(':')
                .and_then(|(f, n)| Some((f, n.parse::<u32>().ok()?)))
                .ok_or("give the line as FILE:LINE")?;
            let file = Path::new(file);
            let why = if log::public_path(&log::key_path(&cli.ledger)).exists() {
                why::why(&read(cli)?, dir, file, n)?
            } else {
                why::why_from_notes(dir, file, n)?
            };
            print_why(cli, &why);
        }
    }
    Ok(())
}

fn tokens(remote: &Remote) -> Result<Tokens> {
    choose_tokens(
        remote.token_command.as_deref(),
        std::env::var("GH_TOKEN").ok().as_deref(),
        std::env::var("GITHUB_TOKEN").ok().as_deref(),
    )
}

/// The token command's, or else GH_TOKEN's, or else GITHUB_TOKEN's. An empty
/// one is none, as gh reads them.
fn choose_tokens(
    command: Option<&str>,
    gh_token: Option<&str>,
    github_token: Option<&str>,
) -> Result<Tokens> {
    fn set(v: Option<&str>) -> Option<&str> {
        v.filter(|v| !v.trim().is_empty())
    }
    if let Some(command) = set(command) {
        return Ok(Tokens::command(command)?);
    }
    let token = set(gh_token)
        .or(set(github_token))
        .ok_or("set GH_TOKEN (or GITHUB_TOKEN, or a token command) to reach GitHub")?;
    Ok(Tokens::given(token))
}

/// The ledger, open for writing once its log holds the anchor on GitHub.
fn write(cli: &Cli, github: &GithubArgs) -> Result<(Anchored, Api, Config)> {
    let api =
        Api::new(&github.remote.api, tokens(&github.remote)?).with_etags(Etags::open(&cli.ledger)?);
    let key = log::load_signing_key(&log::key_path(&cli.ledger))?;
    let anchored = anchor::open(&cli.ledger, key, &api, &github.remote.anchor_repo)?;
    let config = Config {
        reviewers: github.reviewers.clone(),
        own_contexts: github.own_contexts.clone(),
    };
    Ok((anchored, api, config))
}

/// Brings `repo`'s notes up to date with what the ledger now holds.
fn write_notes(api: &Api, a: &Anchored, github: &GithubArgs, repo: &Repo) -> Result<()> {
    let links = if github.session_links.contains(repo) {
        Links::Kept
    } else {
        Links::Left
    };
    notes::publish(api, a.store.ledger(), repo, links)?;
    Ok(())
}

/// The ledger, verified against its public key and the anchor its writer
/// kept beside it. Reading takes no lock and never leaves the box.
fn read(cli: &Cli) -> Result<Ledger> {
    let key = log::load_public_key(&log::public_path(&log::key_path(&cli.ledger)))
        .map_err(|e| format!("{e} (has `cn init` set up {}?)", cli.ledger.display()))?;
    Ok(reader::read(&cli.ledger, &key)?)
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
    json!({"change": c.id.to_string(), "pr": c.pr, "url": c.url(), "recorded": got.recorded})
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
        println!("{}", serde_json::to_value(why).expect("a why serializes"));
        return;
    }
    let Some(n) = &why.landed else {
        println!(
            "{}: no landing in the {} brought it onto trunk (not landed, not recorded, or trunk moved outside Conary)",
            why.commit,
            word(why.source)
        );
        return;
    };
    println!("{}: {}", n.landing.after, n.url);
    if let Some(summary) = &n.summary {
        println!("  {summary}");
    }
    if let Some(task) = &n.task {
        println!("  task: {task}");
    }
    let session = n.author.session.as_deref().unwrap_or("no session");
    println!("  by {} ({session})", n.author.login);
    match &n.landing.by {
        Authority::Status { login } => {
            println!("  landed on {login}'s settled status (unattested)");
        }
    }
    for e in &n.evidence {
        println!(
            "  {} {}: {} ({})",
            word(e.kind),
            e.check,
            word(e.result),
            e.source
        );
    }
    if why.source == why::Source::Notes {
        println!("  (from the notes: unsigned, as the ledger said when they were written)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_github::IngestError;
    use conary_github::anchor::AnchorError;

    #[test]
    fn help_never_shows_the_token_command() {
        use clap::CommandFactory;
        let cli = Cli::command();
        let args: Vec<_> = cli
            .get_subcommands()
            .flat_map(|c| c.get_arguments())
            .filter(|a| a.get_id() == "token_command")
            .collect();
        assert!(!args.is_empty());
        assert!(args.iter().all(|a| a.is_hide_env_values_set()));
    }

    #[test]
    fn the_token_command_then_gh_token_then_github_token_and_an_empty_one_is_none() {
        let token = |command, gh, github| {
            choose_tokens(command, gh, github)
                .map(|t| t.of("o/r").unwrap())
                .map_err(|e| e.to_string())
        };
        assert_eq!(
            token(Some("printf c-%s"), Some("g"), None),
            Ok("c-o/r".into())
        );
        assert_eq!(token(Some(" "), Some("g"), Some("h")), Ok("g".into()));
        assert_eq!(token(None, Some(""), Some("h")), Ok("h".into()));
        assert_eq!(token(None, Some("g"), Some("h")), Ok("g".into()));
        for (gh, github) in [(None, None), (Some(""), Some(" "))] {
            assert!(
                token(None, gh, github)
                    .unwrap_err()
                    .contains("set GH_TOKEN")
            );
        }
    }

    #[test]
    fn github_unavailable_is_told_through_whatever_wraps_it() {
        let boxed = |e: Box<dyn std::error::Error>| unavailable(&*e);
        assert!(boxed(ApiError::unavailable("limit").into()));
        assert!(boxed(
            IngestError::Api(ApiError::unavailable("limit")).into()
        ));
        assert!(boxed(
            AnchorError::Api(ApiError::unavailable("limit")).into()
        ));
        assert!(!boxed(IngestError::Api(ApiError::new("refused")).into()));
        assert!(!boxed(AnchorError::Lost("gone".into()).into()));
        assert!(!boxed("no token".into()));
    }
}
