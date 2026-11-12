//! `cn attest`: a signing job's side of `docs/ci-attestation.md`. It writes
//! the subject and predicate a Sigstore bundle is made from and, with
//! `--sign`, the statement and the job's own OIDC token as the file the
//! verifier reads.

mod ci;

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
        #[arg(long, required_unless_present = "from", conflicts_with = "from")]
        result: Option<Negative>,
        /// The negative tree. Every result but none has one.
        #[arg(long, conflicts_with = "from")]
        tree: Option<String>,
        /// Each new test file.
        #[arg(long = "test", value_name = "PATH", conflicts_with = "from")]
        tests: Vec<String>,
        /// Each check that reads the test files.
        #[arg(long = "check", value_name = "CHECK", conflicts_with = "from")]
        checks: Vec<String>,
        /// Each of those checks that wasn't built.
        #[arg(long = "skipped", value_name = "CHECK", conflicts_with = "from")]
        skipped: Vec<String>,
        /// Each new test file none of the checks runs.
        #[arg(long = "unrun", value_name = "PATH", conflicts_with = "from")]
        unrun: Vec<String>,
        /// Take all of the above from burrow's `negative.json` (`result`,
        /// `tree`, `tests`, `checks`, `skipped` and `unrun`, nothing else)
        /// instead of from flags.
        #[arg(long, value_name = "FILE")]
        from: Option<PathBuf>,
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
    #[arg(
        long,
        required_unless_present = "leg_logs",
        conflicts_with = "leg_logs"
    )]
    log: Option<PathBuf>,
    /// One leg's log of a gate split across legs, in leg order. The gate's
    /// output is them one after another, each under a `leg K of N:` line
    /// (written to `combined.log` in `--out`), and the predicate names that.
    #[arg(long = "leg-log", value_name = "FILE")]
    leg_logs: Vec<PathBuf>,
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

impl Out {
    /// The gate's output the predicate names: the one log, or the legs' put
    /// together.
    fn log_bytes(&self) -> Result<Vec<u8>> {
        let read = |path: &PathBuf| -> Result<Vec<u8>> {
            Ok(std::fs::read(path)
                .map_err(|e| format!("the log {} doesn't read: {e}", path.display()))?)
        };
        match &self.log {
            Some(path) => read(path),
            None => Ok(combined(
                &self.leg_logs.iter().map(read).collect::<Result<Vec<_>>>()?,
            )),
        }
    }
}

pub fn run(cli: &Cli, command: &AttestCommand) -> Result<()> {
    match command {
        AttestCommand::Ci(args) => return ci::attest(cli, args),
        AttestCommand::PrepareNegative(args) => return ci::prepare(args),
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
    let (claim, out, log_bytes) = claim(command)?;
    let request = request(out)?;
    let made = make(
        &claim,
        out,
        &log_bytes,
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
                "log": made.log,
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
fn claim(command: &AttestCommand) -> Result<(Claim, &Out, Vec<u8>)> {
    let out = match command {
        AttestCommand::Ci(_) | AttestCommand::PrepareNegative(_) => unreachable!(),
        AttestCommand::Commit { out, .. }
        | AttestCommand::Revision { out, .. }
        | AttestCommand::Negative { out, .. } => out,
    };
    let log_bytes = out.log_bytes()?;
    let log = Log::of(&log_bytes, &out.log_artifact);
    let claim = match command {
        AttestCommand::Ci(_) | AttestCommand::PrepareNegative(_) => unreachable!(),
        AttestCommand::Commit { commit, gate, .. } => Claim::Commit {
            commit: commit.clone(),
            result: gate.result,
            tree: gate.tree.clone(),
            skipped: gate.skipped.clone(),
            log,
        },
        AttestCommand::Revision {
            base, head, gate, ..
        } => Claim::Revision {
            base: base.clone(),
            head: head.clone(),
            result: gate.result,
            tree: gate.tree.clone(),
            skipped: gate.skipped.clone(),
            log,
        },
        AttestCommand::Negative {
            base,
            head,
            result,
            tree,
            tests,
            checks,
            skipped,
            unrun,
            from,
            ..
        } => {
            let found = match (from, result) {
                (Some(path), _) => Found::read(path)?,
                (None, Some(result)) => Found {
                    result: *result,
                    tree: tree.clone(),
                    tests: tests.clone(),
                    checks: checks.clone(),
                    skipped: skipped.clone(),
                    unrun: unrun.clone(),
                },
                (None, None) => unreachable!("clap requires --result without --from"),
            };
            Claim::Negative {
                base: base.clone(),
                head: head.clone(),
                result: found.result,
                tree: found.tree,
                tests: found.tests,
                checks: found.checks,
                skipped: found.skipped,
                unrun: found.unrun,
                log,
            }
        }
    };
    Ok((claim, out, log_bytes))
}

/// What a negative run found, as burrow's `negative.json` keeps it. The
/// subject and predicate are made from it here, never by the caller.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Found {
    #[serde(deserialize_with = "negative_result")]
    result: Negative,
    // An explicit deserializer requires the key even when its value is null.
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    tree: Option<String>,
    tests: Vec<String>,
    checks: Vec<String>,
    skipped: Vec<String>,
    unrun: Vec<String>,
}

impl Found {
    fn read(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("{} doesn't read: {e}", path.display()))?;
        // A derived struct also accepts a positional array. Enter through
        // deserialize_map so only the six named members can reach Found.
        struct Object;
        impl<'de> serde::de::Visitor<'de> for Object {
            type Value = Found;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a negative run's result object")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                map: M,
            ) -> std::result::Result<Found, M::Error> {
                serde::Deserialize::deserialize(serde::de::value::MapAccessDeserializer::new(map))
            }
        }
        let read = || -> serde_json::Result<Self> {
            let mut input = serde_json::Deserializer::from_slice(&bytes);
            let found = serde::Deserializer::deserialize_map(&mut input, Object)?;
            input.end()?;
            Ok(found)
        };
        Ok(read().map_err(|e| format!("{} isn't a negative run's result: {e}", path.display()))?)
    }
}

// Deserialize a string before parsing the enum: serde's enum deserializer
// would also accept an object such as {"none": null}.
fn negative_result<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Negative, D::Error> {
    let result: String = serde::Deserialize::deserialize(input)?;
    result.parse().map_err(serde::de::Error::custom)
}

/// The legs' logs as one gate output: each under its `leg K of N:` line, one
/// after another.
fn combined(legs: &[Vec<u8>]) -> Vec<u8> {
    let mut all = Vec::new();
    for (k, log) in legs.iter().enumerate() {
        all.extend(format!("leg {} of {}:\n", k + 1, legs.len()).bytes());
        all.extend(log);
    }
    all
}

/// What `cn attest` wrote.
struct Made {
    /// The legs' logs together, when there were legs.
    log: Option<PathBuf>,
    subject: PathBuf,
    predicate: PathBuf,
    predicate_type: &'static str,
    attestation: Option<PathBuf>,
    audience: Option<String>,
}

/// Writes the claim's subject and predicate into `out.out` and, given a
/// request, its statement with the token GitHub signs for it. The token is
/// asked for before anything is written, so a refusal leaves nothing behind.
fn make(claim: &Claim, out: &Out, log_bytes: &[u8], request: Option<Request>) -> Result<Made> {
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
    // Write the snapshot used for the predicate, even if signing or its
    // retries allowed the source logs to change or disappear.
    let log = if out.leg_logs.is_empty() {
        None
    } else {
        std::fs::write(out.out.join("combined.log"), log_bytes)?;
        Some(out.out.join("combined.log"))
    };
    let subject = write(name, &subject)?;
    let predicate = write("predicate.json", &format!("{predicate}\n"))?;
    let (attestation, audience) = match signed {
        Some((file, audience)) => (Some(write(ATTESTATION, &file)?), Some(audience)),
        None => (None, None),
    };
    Ok(Made {
        log,
        subject,
        predicate,
        predicate_type,
        attestation,
        audience,
    })
}

#[cfg(test)]
mod tests;
