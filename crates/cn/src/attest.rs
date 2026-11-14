//! `cn attest`: a signing job's side of `docs/ci-attestation.md`. It writes
//! the subject and predicate a Sigstore bundle is made from and, with
//! `--sign`, the statement and the job's own OIDC token as the file the
//! verifier reads. And `cn attest job`, a door's side: whether a job's own
//! token, on its own, may hand it something.

mod ci;

use std::io::Read as _;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde_json::json;

use conary_github::Api;
use conary_github::attest::Signer;
use conary_github::attest::job::{self, Admitted};
use conary_github::attest::make::{self, Claim, Gate, Log, Negative, Request, Statement};

use crate::{Cli, Result};

/// Where GitHub Actions tells a job granted `id-token: write` to ask for its
/// token, and the bearer that asks. Read from the environment only, never
/// a flag, so the bearer is in no command line or help.
const REQUEST_URL: &str = "ACTIONS_ID_TOKEN_REQUEST_URL";
const REQUEST_TOKEN: &str = "ACTIONS_ID_TOKEN_REQUEST_TOKEN";

/// The file the statement and its token are kept in, as the verifier finds
/// it in the run's artifact.
const ATTESTATION: &str = "conary-attestation";

#[derive(Subcommand)]
pub enum AttestCommand {
    /// Summarise caller jobs and sign a tree read with git alone.
    Ci(ci::CiArgs),
    /// Construct base code with head test files, without running code.
    PrepareNegative(ci::RevisionArgs),
    /// A push's gate on a commit.
    Commit {
        /// The commit pushed.
        commit: String,
        #[command(flatten)]
        gate: GateArgs,
        #[command(flatten)]
        out: Out,
    },
    /// A PR's gate on its revision.
    Revision {
        /// The merge base of the head and the base branch.
        base: String,
        /// The PR's head.
        head: String,
        #[command(flatten)]
        gate: GateArgs,
        #[command(flatten)]
        out: Out,
    },
    /// A PR's negative run: its new tests on its base's code.
    Negative {
        /// The merge base the negative tree takes its code from.
        base: String,
        /// The PR's head, whose test files it takes.
        head: String,
        /// fail when a check failed on the negative tree; pass when every
        /// check passed and runs every new test; skipped when every check
        /// built passed but not all were built or some new test none runs;
        /// none when the change needs no negative.
        #[arg(long)]
        result: Negative,
        /// The negative tree. Every result but none has one.
        #[arg(long)]
        tree: Option<String>,
        /// Each new test file.
        #[arg(long = "test", value_name = "PATH")]
        tests: Vec<String>,
        /// Each check that reads the test files.
        #[arg(long = "check", value_name = "CHECK")]
        checks: Vec<String>,
        /// Each of those checks that wasn't built.
        #[arg(long = "skipped", value_name = "CHECK")]
        skipped: Vec<String>,
        /// Each new test file none of the checks runs.
        #[arg(long = "unrun", value_name = "PATH")]
        unrun: Vec<String>,
        #[command(flatten)]
        out: Out,
    },
    /// Check a GitHub Actions job's own OIDC token, read from stdin, as a
    /// door that takes something from CI asks before it takes it
    /// (docs/ci-attestation.md, "A job's own token"): GitHub signed it for
    /// the audience given and no other, it hasn't expired, the signer
    /// workflow asked for it at a commit on its default branch in one of the
    /// repos given, and its job ran on one of GitHub's runners or a fresh
    /// one. Says what the token
    /// says of its run; exits 65 when it doesn't hold.
    Job(JobArgs),
}

#[derive(clap::Args)]
pub struct JobArgs {
    /// The audience the token has to name, alone: what the job hands over,
    /// by its digest.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    audience: String,
    /// A repository, as owner/name, whose runs may hand it over. Repeat for
    /// each; a token of any other is refused, since anyone's repo can call
    /// the signer workflow.
    #[arg(long = "repo", value_name = "OWNER/NAME", required = true)]
    repos: Vec<conary_core::op::Repo>,
    /// The workflow whose jobs may, as `owner/repo/.github/workflows/FILE`:
    /// the machine's, as `cn train step` takes it.
    #[arg(
        long = "signer-workflow",
        env = "CONARY_SIGNER_WORKFLOW",
        value_name = "OWNER/REPO/PATH"
    )]
    signer: Signer,
    /// What the name of each self-hosted runner started fresh for one job
    /// starts with, as `cn train step` takes it. With none, only GitHub's
    /// own runners count.
    #[arg(long = "fresh-runners", env = "CONARY_FRESH_RUNNERS", value_name = "PREFIX", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    fresh: Option<String>,
    /// The GitHub API's root.
    #[arg(long, env = "GITHUB_API_URL", default_value = "https://api.github.com")]
    api: String,
    /// A command that prints a token for the repository it's given, as
    /// every `cn` takes it. GitHub is asked which runner took a self-hosted
    /// job and whether the signer's commit is on its default branch.
    #[arg(long, env = "CONARY_TOKEN_COMMAND", hide_env_values = true)]
    token_command: Option<String>,
}

/// A job's token that doesn't hold, and why: nothing was taken, and asking
/// again refuses again.
#[derive(Debug)]
pub struct Refused(pub(crate) String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the job's token doesn't hold: {}", self.0)
    }
}

impl std::error::Error for Refused {}

#[derive(clap::Args)]
pub struct GateArgs {
    /// pass when the gate succeeded, otherwise fail.
    #[arg(long)]
    result: Gate,
    /// The tree the gate ran on: for a PR, the head merged onto the base
    /// branch's tip.
    #[arg(long)]
    tree: String,
    /// Each check the gate evaluated but didn't build.
    #[arg(long = "skipped", value_name = "CHECK")]
    skipped: Vec<String>,
}

#[derive(clap::Args)]
pub struct Out {
    /// The gate's output, which the predicate names by its SHA-256.
    #[arg(long)]
    log: PathBuf,
    /// The name of the run's artifact that keeps the log.
    #[arg(long = "log-artifact", value_name = "NAME")]
    log_artifact: String,
    /// The directory to write the subject, `predicate.json` and, signed,
    /// `conary-attestation` into.
    #[arg(long)]
    out: PathBuf,
    /// Sign the statement with the job's own OIDC token, as a private repo's
    /// job does: the job needs `id-token: write`.
    #[arg(long)]
    sign: bool,
}

pub fn run(cli: &Cli, command: &AttestCommand) -> Result<()> {
    match command {
        AttestCommand::Ci(args) => return ci::attest(cli, args),
        AttestCommand::PrepareNegative(args) => return ci::prepare(args),
        AttestCommand::Job(args) => return job(cli, args),
        _ => {}
    }
    let request = |out: &Out| -> Result<Option<(String, String)>> {
        if !out.sign {
            return Ok(None);
        }
        let get = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
        match (get(REQUEST_URL), get(REQUEST_TOKEN)) {
            (Some(url), Some(bearer)) => Ok(Some((url, bearer))),
            _ => Err(format!(
                "--sign asks GitHub for this job's token, so it needs {REQUEST_URL} and {REQUEST_TOKEN}: give the job id-token: write"
            )
            .into()),
        }
    };
    let (claim, out) = claim(command)?;
    let request = request(out)?;
    let made = make(
        &claim,
        out,
        request
            .as_ref()
            .map(|(url, bearer)| Request { url, bearer }),
    )?;
    if cli.human {
        println!("subject: {}", made.subject.display());
        println!(
            "predicate: {} ({})",
            made.predicate.display(),
            made.predicate_type
        );
        if let (Some(file), Some(audience)) = (&made.attestation, &made.audience) {
            println!("attestation: {}, signed for {audience}", file.display());
        }
    } else {
        println!(
            "{}",
            json!({
                "subject": made.subject,
                "predicate": made.predicate,
                "predicate_type": made.predicate_type,
                "attestation": made.attestation,
                "audience": made.audience,
            })
        );
    }
    Ok(())
}

/// The claim a command makes, and where it goes.
fn claim(command: &AttestCommand) -> Result<(Claim, &Out)> {
    let log = |out: &Out| -> Result<Log> {
        let bytes = std::fs::read(&out.log)
            .map_err(|e| format!("the log {} doesn't read: {e}", out.log.display()))?;
        Ok(Log::of(&bytes, &out.log_artifact))
    };
    Ok(match command {
        AttestCommand::Ci(_) | AttestCommand::PrepareNegative(_) | AttestCommand::Job(_) => {
            unreachable!()
        }
        AttestCommand::Commit { commit, gate, out } => (
            Claim::Commit {
                commit: commit.clone(),
                result: gate.result,
                tree: gate.tree.clone(),
                skipped: gate.skipped.clone(),
                log: log(out)?,
            },
            out,
        ),
        AttestCommand::Revision {
            base,
            head,
            gate,
            out,
        } => (
            Claim::Revision {
                base: base.clone(),
                head: head.clone(),
                result: gate.result,
                tree: gate.tree.clone(),
                skipped: gate.skipped.clone(),
                log: log(out)?,
            },
            out,
        ),
        AttestCommand::Negative {
            base,
            head,
            result,
            tree,
            tests,
            checks,
            skipped,
            unrun,
            out,
        } => (
            Claim::Negative {
                base: base.clone(),
                head: head.clone(),
                result: *result,
                tree: tree.clone(),
                tests: tests.clone(),
                checks: checks.clone(),
                skipped: skipped.clone(),
                unrun: unrun.clone(),
                log: log(out)?,
            },
            out,
        ),
    })
}

/// `cn attest job`: the token on stdin, never an argument, where any
/// process could read it.
fn job(cli: &Cli, args: &JobArgs) -> Result<()> {
    let mut token = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin().take(MOST_TOKEN), &mut token)?;
    let token = token.trim();
    let tokens = crate::choose_tokens(
        args.token_command.as_deref(),
        std::env::var("GH_TOKEN").ok().as_deref(),
        std::env::var("GITHUB_TOKEN").ok().as_deref(),
    )?;
    let api = Api::new(&args.api, tokens);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let got = job::admitted(
        &api,
        &api,
        token,
        &job::Door {
            audience: &args.audience,
            repos: &args.repos,
            signer: &args.signer,
            fresh: args.fresh.as_deref(),
        },
        i64::try_from(now)?,
    )?
    .map_err(Refused)?;
    if cli.human {
        let Admitted {
            repository,
            visibility,
            event,
            git_ref,
            ..
        } = &got;
        println!("{repository} ({visibility}), {event} of {git_ref}: holds");
    } else {
        println!("{}", serde_json::to_value(&got)?);
    }
    Ok(())
}

/// A job's token is a kilobyte or so; anything far past that isn't one.
const MOST_TOKEN: u64 = 16 * 1024;

/// What `cn attest` wrote.
struct Made {
    subject: PathBuf,
    predicate: PathBuf,
    predicate_type: &'static str,
    attestation: Option<PathBuf>,
    audience: Option<String>,
}

/// Writes the claim's subject and predicate into `out.out` and, given a
/// request, its statement with the token GitHub signs for it. The token is
/// asked for before anything is written, so a refusal leaves nothing behind.
fn make(claim: &Claim, out: &Out, request: Option<Request>) -> Result<Made> {
    let statement = make::statement(claim)?;
    let signed = match &request {
        Some(request) => {
            let text = statement.text();
            let token = make::token(request, &text)?;
            Some((make::file(&text, &token), make::audience(&text)))
        }
        None => None,
    };
    let Statement {
        name,
        subject,
        predicate,
        predicate_type,
    } = statement;
    std::fs::create_dir_all(&out.out)?;
    let write = |file: &str, body: &str| -> Result<PathBuf> {
        let path = Path::new(&out.out).join(file);
        std::fs::write(&path, body)
            .map_err(|e| format!("{} doesn't write: {e}", path.display()))?;
        Ok(path)
    };
    let subject = write(name, &subject)?;
    let predicate = write("predicate.json", &format!("{predicate}\n"))?;
    let (attestation, audience) = match signed {
        Some((file, audience)) => (Some(write(ATTESTATION, &file)?), Some(audience)),
        None => (None, None),
    };
    Ok(Made {
        subject,
        predicate,
        predicate_type,
        attestation,
        audience,
    })
}
