//! `cn`, Conary's command line: a thin layer over conary-core and
//! conary-github. JSON on stdout by default, for agents; `--human` for
//! people.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use conary_core::ledger::Change;
use conary_core::log;
use conary_core::notes::Links;
use conary_core::op::{Authority, Repo};
use conary_core::why::{self, Why};
use conary_core::{ChangeId, Ledger, Policy};
use conary_core::{reader, rpc};
use conary_github::anchor::{self, Anchored};
use conary_github::class;
use conary_github::undo::{self, How, Stopped, Undid, Undone, Unrecorded};
use conary_github::{Api, ApiError, Config, Etags, Ingested, Tokens, ingest, notes};

mod train;

#[derive(Parser)]
#[command(
    name = "cn",
    about = "Conary: the ledger of every change and why it was made",
    after_help = "Exits 75 when GitHub couldn't be asked just now (its rate limit, its own errors, the network): run it again later. Exits 65 when the ledger refused what was asked, so nothing was done: asked again, it refuses again until the ledger changes."
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
    #[command(flatten)]
    Query(Query),
    /// Answer a query as the ledger stood at a time: from the ops its log
    /// held then, the whole log verified as ever.
    At {
        /// In milliseconds since the Unix epoch.
        time: i64,
        #[command(subcommand)]
        query: AtQuery,
    },
    /// Check the log against its anchor on GitHub, all of its history.
    Verify {
        #[command(flatten)]
        remote: Remote,
    },
    /// Open the PR that reverts a landed change's landing, as trunk is now,
    /// and record it as the change's undo; or, by session or time, one PR
    /// per repo reverting each change newest first. A revert that conflicts
    /// with what came after opens all the same, for its conflict to be
    /// resolved; one that conflicts with the others' is left out, for an
    /// undo of its own. Run again, it finds those PRs rather than open more.
    #[command(group(clap::ArgGroup::new("what").required(true).args(["change", "actor", "since"])))]
    Undo {
        /// The change: its id, or its PR as OWNER/NAME#N.
        change: Option<String>,
        /// Every change this agent session landed, in every repo.
        #[arg(long)]
        actor: Option<String>,
        /// Everything that landed on `--repo`'s trunk after this time, in
        /// milliseconds since the Unix epoch.
        #[arg(long, requires = "repo")]
        since: Option<i64>,
        /// The repo `--since` reverts, as OWNER/NAME.
        #[arg(long, requires = "since")]
        repo: Option<Repo>,
        #[command(flatten)]
        github: GithubArgs,
    },
    /// A PR's class at a head, and the evidence it needs: what the class
    /// policy says of the files that revision changes, recorded once per
    /// revision and policy. Refused when the PR's head on GitHub is another.
    Class {
        /// The repository, as owner/name.
        repo: Repo,
        /// The PR's number.
        pr: u64,
        /// The head to class.
        #[arg(long)]
        head: String,
        /// The class policy, a JSON file (docs/DESIGN.md section 4.4).
        #[arg(long, env = "CONARY_POLICY")]
        policy: PathBuf,
        #[command(flatten)]
        github: GithubArgs,
    },
    /// Check a class policy file (docs/DESIGN.md section 4.4): read as `cn
    /// class` reads it, a key this cn doesn't know, a bad pattern or a repo
    /// named twice refused. Says the digest a `classify` op names it by. A
    /// machine checks its file when it builds it, so a cn too old for it
    /// fails the build rather than every class it's asked.
    Policy {
        /// The class policy, a JSON file.
        #[arg(env = "CONARY_POLICY")]
        policy: PathBuf,
    },
    /// A repo's train: the changes ready to land, gated together.
    Train {
        #[command(subcommand)]
        command: train::TrainCommand,
    },
}

/// What `cn` reads without writing, which `at` asks too.
#[derive(Subcommand)]
enum Query {
    /// List the changes in the ledger.
    Log {
        /// Only this repository's.
        #[arg(long)]
        repo: Option<Repo>,
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

/// What `at` asks: any query.
#[derive(Subcommand)]
enum AtQuery {
    #[command(flatten)]
    Query(Query),
    /// A repo's train.
    Train {
        #[command(subcommand)]
        command: train::TrainQuery,
    },
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The exit status when GitHub couldn't be asked just now (its rate limit,
/// its own errors, the network), BSD's EX_TEMPFAIL: running it again later
/// is the whole fix, so a caller can book that instead of calling it a
/// failure.
const UNAVAILABLE: u8 = 75;

/// The exit status when the ledger refused what was asked (no such change,
/// one that hasn't landed, a session that landed nothing), BSD's
/// EX_DATAERR: nothing was done, and running it again refuses again until
/// the ledger changes. Every other failure may have stopped partway.
const REFUSED: u8 = 65;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cn: {e}");
            if unavailable(&*e) {
                ExitCode::from(UNAVAILABLE)
            } else if refused(&*e) {
                ExitCode::from(REFUSED)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// Whether the ledger refused what was asked, before anything was done.
fn refused(e: &(dyn std::error::Error + 'static)) -> bool {
    e.downcast_ref::<Stopped>().is_some_and(Stopped::refused) || e.is::<Unnamed>()
}

/// A change named by something the ledger has no change for.
#[derive(Debug)]
struct Unnamed(String);

impl std::fmt::Display for Unnamed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unnamed {}

/// Whether the error comes down to GitHub being unavailable.
fn unavailable(e: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(Some(e), |e| e.source())
        .any(|e| e.downcast_ref::<ApiError>().is_some_and(|a| a.unavailable))
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Policy { policy } => {
            let digest = Policy::load(policy)?.digest().to_owned();
            if cli.human {
                println!("{}: a class policy, {digest}", policy.display());
            } else {
                println!("{}", serde_json::json!({ "digest": digest }));
            }
        }
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
        Command::Class {
            repo,
            pr,
            head,
            policy,
            github,
        } => {
            let policy = Policy::load(policy)?;
            let (mut a, api, config) = write(cli, github)?;
            let got = class::class(&mut a.store, &api, &config, &policy, repo, *pr, head);
            a.publish(&api)?;
            write_notes(&api, &a, github, repo)?;
            let got = got?;
            let said = &got.said;
            if cli.human {
                let because = said
                    .because
                    .as_deref()
                    .map_or(String::new(), |b| format!(" ({b})"));
                let docs = if said.docs {
                    ", documentation only"
                } else {
                    ""
                };
                let review = if said.review() {
                    "a review round"
                } else {
                    "no review round"
                };
                println!(
                    "{repo}#{pr} at {head}: {}{because}{docs}; needs {review}",
                    said.class
                );
            } else {
                println!(
                    "{}",
                    json!({"pr": pr, "change": got.change.to_string(),
                           "revision": got.revision.to_string(), "class": said.class,
                           "because": said.because, "docs": said.docs,
                           "review": said.review(), "recorded": got.recorded})
                );
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
        Command::Query(q) => query(cli, q, None)?,
        Command::At { time, query: q } => match q {
            AtQuery::Query(q) => query(cli, q, Some(*time))?,
            AtQuery::Train { command } => train::query(cli, command, Some(*time))?,
        },
        Command::Undo {
            change,
            actor,
            since,
            repo,
            github,
        } => {
            let (mut a, api, config) = write(cli, github)?;
            let got = match (change, actor, since.zip(repo.as_ref())) {
                (Some(change), ..) => {
                    let id = named(a.store.ledger(), change)?;
                    undo::undo(&mut a.store, &api, &api, &config, id).map(|u| Undid {
                        undos: vec![u],
                        gone: Vec::new(),
                    })
                }
                (_, Some(session), _) => {
                    undo::undo_actor(&mut a.store, &api, &api, &config, session)
                }
                (_, _, Some((since, repo))) => {
                    undo::undo_since(&mut a.store, &api, &api, &config, repo, since)
                }
                _ => unreachable!("clap asks for one of them"),
            };
            // What it opened is told first, the undos a run that stopped
            // had by then included: they're PRs on GitHub, whatever stops
            // it after them.
            let (done, unrecorded) = match &got {
                Ok(done) => (done, None),
                Err(stopped) => (&*stopped.done, stopped.unrecorded.as_ref()),
            };
            for u in &done.undos {
                print_undone(cli, u, a.store.ledger());
            }
            if let Some(u) = unrecorded {
                print_unrecorded(cli, u);
            }
            print_gone(cli, &done.gone);
            a.publish(&api)?;
            // Whatever was recorded before a failure is published, and so
            // are its repos' notes.
            let named_id = change
                .as_ref()
                .and_then(|c| named(a.store.ledger(), c).ok());
            let repos = noted(a.store.ledger(), done, named_id, repo.as_ref());
            let notes = repos
                .iter()
                .try_for_each(|r| write_notes(&api, &a, github, r));
            // The run's own failure is what it exits with: notes it
            // couldn't write after it are the next run's to write.
            got?;
            notes?;
        }
        Command::Train { command } => train::run(cli, command)?,
    }
    Ok(())
}

/// A query's answer, from the ledger as it is or as it stood `at` a time.
fn query(cli: &Cli, q: &Query, at: Option<i64>) -> Result<()> {
    match q {
        Query::Log { repo } => {
            let ledger = read(cli, at)?;
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
        Query::Why { at: line, dir } => {
            let (file, n) = line
                .rsplit_once(':')
                .and_then(|(f, n)| Some((f, n.parse::<u32>().ok()?)))
                .ok_or("give the line as FILE:LINE")?;
            let file = Path::new(file);
            let why = if log::public_path(&log::key_path(&cli.ledger)).exists() {
                why::why(&read(cli, at)?, dir, file, n)?
            } else if at.is_none() {
                why::why_from_notes(dir, file, n)?
            } else {
                return Err(
                    "at a time, why needs the ledger here: the notes hold what's landed now".into(),
                );
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
/// kept beside it: as it is, or as it stood `at` a time. Reading takes no
/// lock and never leaves the box.
fn read(cli: &Cli, at: Option<i64>) -> Result<Ledger> {
    let key = log::load_public_key(&log::public_path(&log::key_path(&cli.ledger)))
        .map_err(|e| format!("{e} (has `cn init` set up {}?)", cli.ledger.display()))?;
    Ok(match at {
        None => reader::read(&cli.ledger, &key)?,
        Some(t) => reader::read_at(&cli.ledger, &key, t)?,
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
    let mut line = format!(
        "{}#{} {status:<8} {} {summary}",
        c.repo, c.pr, c.author.login
    );
    if let Some(undo) = &c.undo {
        let ids: Vec<String> = undo.reverts.iter().map(|id| id.to_string()).collect();
        line += &format!(" (reverts {})", ids.join(", "));
    }
    if let Some(by) = c.reverted_by {
        line += &format!(" (reverted by {by})");
    }
    line
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

/// A change named by its id, or by its PR as OWNER/NAME#N.
fn named(ledger: &Ledger, said: &str) -> std::result::Result<ChangeId, Unnamed> {
    let Some((repo, pr)) = said.split_once('#') else {
        return said
            .parse()
            .map_err(|e| Unnamed(format!("{said:?} as a change id: {e}")));
    };
    let repo: Repo = repo
        .parse()
        .map_err(|e| Unnamed(format!("{repo:?} as OWNER/NAME: {e}")))?;
    let pr: u64 = pr
        .parse()
        .map_err(|e| Unnamed(format!("{pr:?} as a PR: {e}")))?;
    Ok(ledger
        .by_pr(&repo, pr)
        .ok_or_else(|| Unnamed(format!("{repo}#{pr} isn't in the ledger")))?
        .id)
}

fn print_undone(cli: &Cli, got: &Undone, ledger: &Ledger) {
    let u = ledger.change(got.change).expect("undo recorded it");
    let reverts: Vec<String> = got.reverts.iter().map(|id| id.to_string()).collect();
    if cli.human {
        let how = match got.how {
            How::Opened { conflicts: false } => "opened",
            How::Opened { conflicts: true } => {
                "opened, conflicting with trunk: what came after builds on it"
            }
            How::Found => "open already",
        };
        println!("{}: reverts {}, {how}", u.url(), reverts.join(", "));
        if !got.left.is_empty() {
            let left: Vec<String> = got.left.iter().map(|id| id.to_string()).collect();
            println!(
                "  left out, conflicting with the others: {}",
                left.join(", ")
            );
        }
        return;
    }
    let (opened, conflicts) = match got.how {
        How::Opened { conflicts } => (true, Some(conflicts)),
        How::Found => (false, None),
    };
    println!(
        "{}",
        json!({"change": u.id.to_string(), "pr": u.pr, "url": u.url(), "reverts": reverts,
               "left": got.left.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
               "opened": opened, "conflicts": conflicts, "recorded": got.recorded})
    );
}

/// The repos an undo writes the notes of: those of what it found or
/// opened, and of what was asked, that the ledger keeps. One it keeps
/// nothing of has no notes, and isn't asked about.
fn noted(
    ledger: &Ledger,
    done: &Undid,
    change: Option<ChangeId>,
    repo: Option<&Repo>,
) -> Vec<Repo> {
    let mut repos: Vec<Repo> = done
        .undos
        .iter()
        .map(|u| u.change)
        .chain(change)
        .filter_map(|id| ledger.change(id).map(|c| c.repo.clone()))
        .chain(repo.cloned())
        .filter(|r| ledger.changes().any(|c| &c.repo == r))
        .collect();
    repos.sort();
    repos.dedup();
    repos
}

fn print_unrecorded(cli: &Cli, u: &Unrecorded) {
    if cli.human {
        println!(
            "{}: opened, and stopped before it was recorded: run it again to record it",
            u.url()
        );
    } else {
        println!("{}", json!({"unrecorded": u.url(), "pr": u.pr}));
    }
}

fn print_gone(cli: &Cli, gone: &[ChangeId]) {
    if gone.is_empty() {
        return;
    }
    let gone: Vec<String> = gone.iter().map(ChangeId::to_string).collect();
    if cli.human {
        println!(
            "trunk already lacks what these brought: {}",
            gone.join(", ")
        );
    } else {
        println!("{}", json!({"gone": gone}));
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
    use conary_github::undo::UndoError;

    // Every query, and only a query, is asked at a time; asked so, it
    // reads the ledger as it stood then.
    #[test]
    fn at_asks_any_query_and_only_a_query_as_the_ledger_stood_then() {
        use conary_core::op::{Actor, ActorKind, Body};
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().to_str().unwrap();
        let parse =
            |args: &[&str]| Cli::try_parse_from([&["cn", "--ledger", ledger], args].concat());
        for args in [
            &["at", "200", "log", "--repo", "o/r"][..],
            &["at", "200", "why", "f:1", "-C", "/"],
            &["at", "200", "train", "show", "o/r"],
            &["log"],
            &["why", "f:1"],
            &["train", "show", "o/r"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        for args in [
            &["at", "200", "undo", "o/r#1"][..],
            &["at", "200", "sync", "o/r"],
            &["at", "200", "train", "step", "o/r"],
            &["at", "200", "at", "100", "log"],
            &["at", "yesterday", "log"],
            &["at", "200"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }

        let key = log::load_or_create_key(&log::key_path(dir.path())).unwrap();
        let lock = log::Lock::take(dir.path()).unwrap();
        log::save_anchor(&lock, None, "c0").unwrap();
        let mut store = conary_core::Store::open(lock, key, None).unwrap();
        for (pr, at) in [(1, 100), (2, 300)] {
            let author = Actor {
                kind: ActorKind::Person,
                login: "someone".into(),
                session: None,
            };
            let change = ChangeId::new().unwrap();
            let repo = "o/r".parse().unwrap();
            store
                .record_at(
                    Body::Observe {
                        change,
                        repo,
                        pr,
                        author,
                    },
                    at,
                )
                .unwrap();
        }
        let cli = parse(&["at", "200", "log"]).unwrap();
        let prs = |at| -> Vec<u64> { read(&cli, at).unwrap().changes().map(|c| c.pr).collect() };
        assert_eq!(prs(Some(200)), [1]);
        assert_eq!(prs(None), [1, 2]);
    }

    // A policy file is checked as `cn class` reads it: one this cn doesn't
    // know a key of is refused, so a machine's build of it fails.
    #[test]
    fn policy_checks_a_file_as_class_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let check = |body: &str| {
            let file = dir.path().join("policy.json");
            std::fs::write(&file, body).unwrap();
            run(&Cli::try_parse_from(["cn", "policy", file.to_str().unwrap()]).unwrap())
        };
        assert!(check(r#"{"r2": ["^secrets/"], "hidden": ["^docs/private/$"]}"#).is_ok());
        for bad in [r#"{"hiddn": []}"#, r#"{"r2": ["("]}"#, "not json"] {
            assert!(check(bad).is_err(), "{bad}");
        }
    }

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

    // Which runners were started fresh is the machine's to say, for every
    // cn it runs: a cn that doesn't count by it yet ignores it, where it
    // would refuse a flag it doesn't know.
    #[test]
    fn the_fresh_runners_are_a_machine_setting() {
        use clap::CommandFactory;
        let cli = Cli::command();
        let step = cli
            .find_subcommand("train")
            .and_then(|t| t.find_subcommand("step"))
            .expect("cn train step");
        let fresh = step
            .get_arguments()
            .find(|a| a.get_long() == Some("fresh-runners"))
            .expect("--fresh-runners");
        assert_eq!(
            fresh.get_env().and_then(|e| e.to_str()),
            Some("CONARY_FRESH_RUNNERS")
        );
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
    fn notes_are_written_only_for_repos_the_ledger_keeps() {
        use conary_core::op::{Actor, ActorKind, Body};
        let dir = tempfile::tempdir().unwrap();
        let lock = log::Lock::take(dir.path()).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let mut store = conary_core::Store::open(lock, key, None).unwrap();
        let kept: Repo = "o/kept".parse().unwrap();
        let change = ChangeId::new().unwrap();
        store
            .record(Body::Observe {
                change,
                repo: kept.clone(),
                pr: 5,
                author: Actor {
                    kind: ActorKind::Agent,
                    login: "a".into(),
                    session: None,
                },
            })
            .unwrap();
        let stranger: Repo = "o/stranger".parse().unwrap();
        let done = Undid::default();
        let ledger = store.ledger();
        assert_eq!(
            noted(ledger, &done, Some(change), Some(&kept)),
            std::slice::from_ref(&kept)
        );
        assert!(noted(ledger, &done, None, Some(&stranger)).is_empty());
        assert_eq!(noted(ledger, &done, Some(change), Some(&stranger)), [kept]);
    }

    #[test]
    fn a_change_is_named_by_its_id_or_its_pr() {
        let ledger = Ledger::default();
        let id = ChangeId::new().unwrap();
        assert_eq!(named(&ledger, &id.to_string()).unwrap(), id);
        let refused = |said: &str| named(&ledger, said).unwrap_err().to_string();
        assert!(refused("o/r#5").contains("o/r#5 isn't in the ledger"));
        assert!(refused("o#5").contains("as OWNER/NAME"));
        assert!(refused("o/r#five").contains("as a PR"));
        assert!(refused("five").contains("as a change id"));
    }

    #[test]
    fn a_refusal_exits_apart_from_a_run_that_stopped() {
        let boxed = |e: Box<dyn std::error::Error>| (refused(&*e), unavailable(&*e));
        let id = ChangeId::new().unwrap();
        assert_eq!(
            boxed(Stopped::from(UndoError::NotLanded(id)).into()),
            (true, false)
        );
        assert_eq!(
            boxed(Unnamed("o/r#5 isn't in the ledger".into()).into()),
            (true, false)
        );
        assert_eq!(
            boxed(Stopped::from(UndoError::Api(ApiError::new("Server Error"))).into()),
            (false, false)
        );
        assert_eq!(
            boxed(Stopped::from(UndoError::Api(ApiError::unavailable("limit"))).into()),
            (false, true)
        );
        assert_eq!(boxed("no token".into()), (false, false));
        // Refused in a repo after an undo opened in another: it stopped.
        let mut stopped = Stopped::from(UndoError::NoTrunk("main".into()));
        stopped.done.undos.push(Undone {
            change: id,
            pr: 7,
            reverts: vec![id],
            left: Vec::new(),
            how: How::Opened { conflicts: false },
            recorded: 1,
        });
        assert_eq!(boxed(stopped.into()), (false, false));
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
