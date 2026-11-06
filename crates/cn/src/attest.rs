//! `cn attest`: a signing job's side of `docs/ci-attestation.md`. It writes
//! the subject and predicate a Sigstore bundle is made from and, with
//! `--sign`, the statement and the job's own OIDC token as the file the
//! verifier reads.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde_json::json;

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
}

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
