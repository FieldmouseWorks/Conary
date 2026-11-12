//! Making an attestation, as a signing job does (`docs/ci-attestation.md`):
//! its statement, a subject and then its predicate on one line, and the job's
//! own GitHub OIDC token, asked for with the audience that names the
//! statement's digest. The file is the statement, then the token as its last
//! line, which is what [`super::attested`] reads back.

use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use ureq::tls::{RootCerts, TlsConfig};

use super::{AUDIENCE, hex, is_commit};
use crate::api::{ApiError, passing_status, read, unreachable};

/// A gate's result: `pass` when it succeeded, otherwise `fail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    Pass,
    Fail,
}

/// What a negative run's new tests did on the old code
/// (`docs/ci-attestation.md`, "Negative runs").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Negative {
    /// A check failed on the negative tree: what rule 2 asks for.
    Fail,
    /// Every check passed, and each new test is run by one of them.
    Pass,
    /// Every check built passed, but some that read the tests weren't built
    /// or some new tests none of them runs.
    Skipped,
    /// The change needs no negative.
    None,
}

impl std::str::FromStr for Gate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "pass" => Ok(Self::Pass),
            "fail" => Ok(Self::Fail),
            _ => Err(format!("a gate's result is pass or fail, not {s:?}")),
        }
    }
}

impl std::str::FromStr for Negative {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "fail" => Ok(Self::Fail),
            "pass" => Ok(Self::Pass),
            "skipped" => Ok(Self::Skipped),
            "none" => Ok(Self::None),
            _ => Err(format!(
                "a negative's result is fail, pass, skipped or none, not {s:?}"
            )),
        }
    }
}

/// The gate's output, as the run keeps it: its SHA-256 and the name of the
/// artifact it's in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Log {
    pub sha256: String,
    pub artifact: String,
}

impl Log {
    /// The log `bytes`, kept as `artifact`.
    pub fn of(bytes: &[u8], artifact: &str) -> Self {
        Self {
            sha256: hex(&Sha256::digest(bytes)),
            artifact: artifact.to_owned(),
        }
    }
}

/// What a job attests, and about what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// A push's gate on `commit`, run on `tree`.
    Commit {
        commit: String,
        result: Gate,
        tree: String,
        skipped: Vec<String>,
        log: Log,
    },
    /// A deferred check's gate for the dispatch `nonce`, on `commit`, run on
    /// `tree`.
    Deferred {
        commit: String,
        nonce: String,
        result: Gate,
        tree: String,
        skipped: Vec<String>,
        log: Log,
    },
    /// A PR's gate on its revision, `head` on its merge base `base`, run on
    /// `tree`, the head merged onto the base branch's tip as the job found it.
    Revision {
        base: String,
        head: String,
        result: Gate,
        tree: String,
        skipped: Vec<String>,
        log: Log,
    },
    /// A PR's negative run: its new tests on its base's code. `tree` is the
    /// negative tree, and none when it needs no negative.
    Negative {
        base: String,
        head: String,
        result: Negative,
        tree: Option<String>,
        tests: Vec<String>,
        checks: Vec<String>,
        skipped: Vec<String>,
        unrun: Vec<String>,
        log: Log,
    },
}

#[derive(Serialize)]
struct Revision<'a> {
    base: &'a str,
    head: &'a str,
}

#[derive(Serialize)]
struct GatePredicate<'a> {
    result: Gate,
    revision: Option<Revision<'a>>,
    tree: &'a str,
    skipped: &'a [String],
    log: &'a Log,
}

#[derive(Serialize)]
struct NegativePredicate<'a> {
    result: Negative,
    revision: Revision<'a>,
    tree: Option<&'a str>,
    tests: &'a [String],
    checks: &'a [String],
    skipped: &'a [String],
    unrun: &'a [String],
    log: &'a Log,
}

/// What a claim attests, in the parts a job keeps: the subject, named as
/// the doc names its file, and the predicate, one line of JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The subject's name: `conary-commit`, `conary-revision` or
    /// `conary-negative`.
    pub name: &'static str,
    /// The subject's text, each line ending in `\n`.
    pub subject: String,
    /// The predicate, as one line of JSON with no `\n`.
    pub predicate: String,
    /// The predicate's type, for a Sigstore bundle of it.
    pub predicate_type: &'static str,
}

impl Statement {
    /// The statement a token signs: the subject, then the predicate and
    /// `\n`.
    pub fn text(&self) -> String {
        format!("{}{}\n", self.subject, self.predicate)
    }
}

/// The predicate type of a gate's result.
pub const GATE_TYPE: &str =
    "https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#v1";
/// The predicate type of a negative run's.
pub const NEGATIVE_TYPE: &str =
    "https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#negative-v1";

/// The claim's statement. Refused when a commit or tree isn't a lowercase
/// object id, or a negative's tree doesn't match its result: `none` has no
/// tree and nothing in its lists, and every other result has a tree.
pub fn statement(claim: &Claim) -> Result<Statement, String> {
    let ids = |ids: &[&str]| {
        ids.iter().find(|id| !is_commit(id)).map_or(Ok(()), |id| {
            Err(format!("{id:?} isn't a lowercase object id"))
        })
    };
    let (name, subject, predicate, predicate_type) = match claim {
        Claim::Commit {
            commit,
            result,
            tree,
            skipped,
            log,
        } => {
            ids(&[commit, tree])?;
            let predicate = GatePredicate {
                result: *result,
                revision: None,
                tree,
                skipped,
                log,
            };
            (
                "conary-commit",
                format!("conary commit v1\n{commit}\n"),
                json(&predicate),
                GATE_TYPE,
            )
        }
        Claim::Deferred {
            commit,
            nonce,
            result,
            tree,
            skipped,
            log,
        } => {
            ids(&[commit, tree])?;
            if nonce.is_empty() || nonce.contains(char::is_whitespace) {
                return Err(format!("{nonce:?} isn't a dispatch nonce"));
            }
            let predicate = GatePredicate {
                result: *result,
                revision: None,
                tree,
                skipped,
                log,
            };
            (
                "conary-deferred",
                format!("conary deferred v1\n{commit}\n{nonce}\n"),
                json(&predicate),
                GATE_TYPE,
            )
        }
        Claim::Revision {
            base,
            head,
            result,
            tree,
            skipped,
            log,
        } => {
            ids(&[base, head, tree])?;
            let predicate = GatePredicate {
                result: *result,
                revision: Some(Revision { base, head }),
                tree,
                skipped,
                log,
            };
            (
                "conary-revision",
                format!("conary revision v1\n{base}\n{head}\n"),
                json(&predicate),
                GATE_TYPE,
            )
        }
        Claim::Negative {
            base,
            head,
            result,
            tree,
            tests,
            checks,
            skipped,
            unrun,
            log,
        } => {
            ids(&[base, head])?;
            match (result, tree) {
                (Negative::None, None)
                    if [tests, checks, skipped, unrun].iter().all(|l| l.is_empty()) => {}
                (Negative::None, _) => {
                    return Err(
                        "a change that needs no negative has no tree and lists nothing".into(),
                    );
                }
                (_, Some(tree)) => ids(&[tree])?,
                (_, None) => return Err("a negative run names the tree it ran on".into()),
            }
            let predicate = NegativePredicate {
                result: *result,
                revision: Revision { base, head },
                tree: tree.as_deref(),
                tests,
                checks,
                skipped,
                unrun,
                log,
            };
            (
                "conary-negative",
                format!("conary negative v1\n{base}\n{head}\n"),
                json(&predicate),
                NEGATIVE_TYPE,
            )
        }
    };
    Ok(Statement {
        name,
        subject,
        predicate,
        predicate_type,
    })
}

fn json(predicate: &impl Serialize) -> String {
    serde_json::to_string(predicate).expect("a predicate is plain data")
}

/// The audience a token for `statement` is asked for with.
pub fn audience(statement: &str) -> String {
    format!("{AUDIENCE}{}", hex(&Sha256::digest(statement)))
}

/// The attestation's file: the statement, then the token as its last line.
pub fn file(statement: &str, token: &str) -> String {
    format!("{statement}{token}\n")
}

/// Where a job asks GitHub for its OIDC token, as GitHub Actions gives it to
/// a job granted `id-token: write`: the URL and the bearer token that asks.
/// The bearer is a credential: it's sent, never shown.
pub struct Request<'a> {
    pub url: &'a str,
    pub bearer: &'a str,
}

#[derive(serde::Deserialize)]
struct Answer {
    value: String,
}

/// How many times a token is asked for before GitHub's answer stands: its
/// token service answers a 503 now and then.
const TRIES: u32 = 5;

/// The job's OIDC token for `statement`, asked with the audience that names
/// its digest, and again after a wait each time GitHub says try later.
pub fn token(request: &Request, statement: &str) -> Result<String, ApiError> {
    token_waiting(request, statement, Duration::from_secs(1))
}

pub(super) fn token_waiting(
    request: &Request,
    statement: &str,
    first: Duration,
) -> Result<String, ApiError> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .proxy(ureq::Proxy::try_from_env())
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .user_agent("conary")
            .http_status_as_error(false)
            .build(),
    );
    // Nothing in the audience needs escaping in a URL.
    let url = format!("{}&audience={}", request.url, audience(statement));
    let mut wait = first;
    let mut tries = 1;
    loop {
        let got = agent
            .get(&url)
            .header("Authorization", &format!("bearer {}", request.bearer))
            .call()
            .map_err(|e| unreachable(request.url, e))
            .and_then(|mut answer| {
                let status = answer.status().as_u16();
                if status == 200 {
                    // Cut off while it's read is the network, asked again;
                    // a whole answer that isn't a token stays wrong.
                    let body = read(request.url, &mut answer)?;
                    let read: Answer = serde_json::from_slice(&body)
                        .map_err(|e| ApiError::new(format!("its OIDC token doesn't read: {e}")))?;
                    return Ok(read.value);
                }
                let what = format!("asking for its OIDC token: HTTP {status}");
                Err(if passing_status(status) {
                    ApiError::unavailable(what)
                } else {
                    ApiError::new(what)
                })
            });
        match got {
            Err(e) if e.unavailable && tries < TRIES => {
                std::thread::sleep(wait);
                wait *= 2;
                tries += 1;
            }
            got => return got,
        }
    }
}
