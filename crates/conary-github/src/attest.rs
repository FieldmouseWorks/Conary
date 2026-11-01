//! Attested CI evidence (`docs/DESIGN.md` section 9,
//! `docs/ci-attestation.md`): a private repo's gate result, signed with its
//! job's own GitHub OIDC token, whose audience names the digest of the
//! statement it came with. It counts only once GitHub's signature, the
//! signer workflow, the statement and the check run all agree.

use std::io::{Cursor, Read};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use conary_core::op::{Attest, Repo};

use crate::api::{ApiError, CheckRun, Source};
use crate::train::Forge;

/// The issuer of GitHub Actions' OIDC tokens.
pub const ISSUER: &str = "https://token.actions.githubusercontent.com";
/// Where GitHub publishes the keys it signs them with.
pub const KEYS: &str = "https://token.actions.githubusercontent.com/.well-known/jwks";
/// The one workflow whose jobs' attestations count, at a commit on its
/// repo's default branch: burrow's shared CI. A branch's copy of it, or a
/// PR's run of the copy it changes, could sign anything.
pub const SIGNER_REPO: &str = "FieldmouseWorks/burrow";
pub const SIGNER_WORKFLOW: &str = ".github/workflows/flake-check.yml";
/// A token's audience: this, then its statement's SHA-256 in lowercase hex.
pub const AUDIENCE: &str = "conary-attestation:v1:sha256:";
/// The artifacts a run keeps its attestations in, each holding the one file.
pub const ARTIFACT: &str = "conary-attestation-";
pub const FILE: &str = "conary-attestation";
/// A statement and its token are a few kilobytes; a file far past that
/// isn't one, and isn't read whole.
const MOST: u64 = 64 * 1024;

/// A GitHub Actions job, as GitHub records it. A job is a check run of the
/// same id.
#[derive(Debug, Clone, Deserialize)]
pub struct Job {
    pub run_id: u64,
    /// The runner that took it, as GitHub records it: no workflow can
    /// change which one that was.
    #[serde(default)]
    pub runner_name: Option<String>,
}

/// One of a run's artifacts, as GitHub lists them.
#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub id: u64,
    pub name: String,
    pub expired: bool,
}

/// One of the keys GitHub publishes (a JWK).
#[derive(Debug, Clone, Deserialize)]
pub struct Key {
    pub kid: String,
    pub kty: String,
    /// An RSA key's modulus and exponent, base64url.
    #[serde(default)]
    pub n: String,
    #[serde(default)]
    pub e: String,
}

/// What checking an attestation reads from GitHub Actions.
pub trait Actions {
    /// A job: `None` for a check run that's no job.
    fn job(&self, repo: &str, job: u64) -> Result<Option<Job>, ApiError>;
    /// A run's artifacts.
    fn artifacts(&self, repo: &str, run: u64) -> Result<Vec<Artifact>, ApiError>;
    /// An artifact, as GitHub's zip of it.
    fn artifact(&self, repo: &str, id: u64) -> Result<Vec<u8>, ApiError>;
    /// The keys GitHub signs its OIDC tokens with, as it publishes them now.
    fn keys(&self) -> Result<Vec<Key>, ApiError>;
}

/// The attestation of a candidate's passing check run on its commit,
/// checked, or why none counts. Only GitHub not answering is an error. A
/// self-hosted runner counts as started fresh for its job only with a name
/// that starts with `fresh`, which only the launcher of such runners gives
/// one; with no `fresh`, only GitHub's hosted runners count.
pub fn attested(
    forge: &dyn Forge,
    source: &dyn Source,
    repo: &Repo,
    commit: &str,
    tree: &str,
    run: &CheckRun,
    fresh: Option<&str>,
) -> Result<Result<Attest, String>, ApiError> {
    let full = repo.to_string();
    let Some(job) = forge.job(&full, run.id)? else {
        return Ok(Err("it isn't a GitHub Actions job".into()));
    };
    let run_id = job.run_id;
    let runner = job.runner_name.as_deref();
    let fresh_named = fresh
        .filter(|f| !f.is_empty())
        .is_some_and(|f| runner.is_some_and(|r| r.starts_with(f)));
    let kept: Vec<Artifact> = forge
        .artifacts(&full, run_id)?
        .into_iter()
        .filter(|a| a.name.starts_with(ARTIFACT) && !a.expired)
        .collect();
    if kept.is_empty() {
        return Ok(Err(format!("its run {run_id} kept no attestation")));
    }
    let keys = forge.keys()?;
    let mut why = Vec::new();
    for a in kept {
        let checked = unzip(&forge.artifact(&full, a.id)?)
            .and_then(|file| check(&file, &keys, repo, commit, tree, run));
        match checked {
            // A runner that took a job before this one, or takes one beside
            // it as its user, could have asked for this token in its name
            // (`docs/ci-attestation.md`, rule 5).
            Ok(claimed) if !claimed.hosted && !fresh_named => why.push(format!(
                "{}: it ran on {}, which wasn't started fresh for it",
                a.name,
                runner.unwrap_or("a runner GitHub doesn't name")
            )),
            Ok(claimed) if on_main(forge, source, &claimed.workflow_sha)? => {
                return Ok(Ok(claimed.attest));
            }
            Ok(claimed) => why.push(format!(
                "{}: signed by {SIGNER_WORKFLOW} at {}, which isn't on {SIGNER_REPO}'s default branch",
                a.name, claimed.workflow_sha
            )),
            Err(e) => why.push(format!("{}: {e}", a.name)),
        }
    }
    Ok(Err(why.join("; ")))
}

/// Whether a commit of the signer's repo is on its default branch: the tip,
/// or behind it.
fn on_main(forge: &dyn Forge, source: &dyn Source, sha: &str) -> Result<bool, ApiError> {
    let trunk = forge.default_branch(SIGNER_REPO)?;
    let Some(tip) = forge.branch(SIGNER_REPO, &trunk)? else {
        return Ok(false);
    };
    Ok(tip == sha || source.merge_base(SIGNER_REPO, sha, &tip)? == sha)
}

/// The one file of an attestation's artifact.
fn unzip(zip: &[u8]) -> Result<Vec<u8>, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(zip)).map_err(|e| format!("not a zip: {e}"))?;
    let file = archive
        .by_name(FILE)
        .map_err(|e| format!("no {FILE} in it: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MOST + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{FILE} doesn't unzip: {e}"))?;
    if bytes.len() as u64 > MOST {
        return Err(format!("{FILE} is past {MOST} bytes"));
    }
    Ok(bytes)
}

/// An attestation that holds but for where its signer workflow is and
/// which runner ran its job.
#[derive(Debug)]
struct Claimed {
    attest: Attest,
    workflow_sha: String,
    /// GitHub says its job ran on one of its own runners, a VM for each job.
    hosted: bool,
}

/// The statement in a predicate's line.
#[derive(Debug, Deserialize)]
struct Predicate {
    result: String,
    revision: Option<Value>,
    tree: String,
    skipped: Vec<String>,
}

/// Checks an attestation's file, the statement then its token, as one of
/// `run` on `commit`, whose tree is `tree`, saying it passed: every rule of
/// `docs/ci-attestation.md` but the signer's commit being on its default
/// branch and a self-hosted runner's name, which take asking GitHub.
fn check(
    file: &[u8],
    keys: &[Key],
    repo: &Repo,
    commit: &str,
    tree: &str,
    run: &CheckRun,
) -> Result<Claimed, String> {
    let file = std::str::from_utf8(file).map_err(|_| "it isn't UTF-8")?;
    let (statement, token) = file
        .strip_suffix('\n')
        .and_then(|f| f.rsplit_once('\n'))
        .ok_or("it isn't a statement and then a token")?;
    let statement = format!("{statement}\n");
    let claims = verified(token, keys)?;
    let said = |name: &str| claims.get(name).and_then(Value::as_str).unwrap_or_default();

    if said("iss") != ISSUER {
        return Err(format!("its token's issuer is {:?}", said("iss")));
    }
    let audience = format!("{AUDIENCE}{}", hex(&Sha256::digest(&statement)));
    let audiences = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if audiences != [audience.as_str()] {
        return Err("its token's audience isn't its statement's digest".into());
    }
    if !said("repository").eq_ignore_ascii_case(&repo.to_string()) {
        return Err(format!("its token is {}'s", said("repository")));
    }
    let signer = format!("{SIGNER_REPO}/{SIGNER_WORKFLOW}@");
    let workflow = said("job_workflow_ref");
    // Anyone's text: cut only where a character ends.
    let signed = workflow
        .get(..signer.len())
        .is_some_and(|by| by.eq_ignore_ascii_case(&signer));
    if !signed {
        return Err(format!("its token was asked for by {workflow:?}"));
    }
    let workflow_sha = said("job_workflow_sha");
    if !is_commit(workflow_sha) {
        return Err(format!(
            "its token names no signer commit ({workflow_sha:?})"
        ));
    }
    let check_run = match claims.get("check_run_id") {
        Some(Value::String(id)) => id.parse().ok(),
        Some(Value::Number(id)) => id.as_u64(),
        _ => None,
    };
    if check_run != Some(run.id) {
        return Err(format!(
            "its token is check run {check_run:?}'s, not {}'s",
            run.id
        ));
    }
    if said("sha") != commit {
        return Err(format!("its token is for {}, not {commit}", said("sha")));
    }

    let mut lines = statement.lines();
    let (Some("conary commit v1"), Some(subject), Some(predicate), None) =
        (lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return Err("its statement isn't a commit's subject and a predicate".into());
    };
    if subject != commit {
        return Err(format!("its statement is about {subject}, not {commit}"));
    }
    let predicate: Predicate =
        serde_json::from_str(predicate).map_err(|e| format!("its predicate doesn't read: {e}"))?;
    if predicate.revision.is_some_and(|r| !r.is_null()) {
        return Err("its predicate names a revision, not a push".into());
    }
    if predicate.tree != tree {
        return Err(format!("it ran on tree {}, not {tree}", predicate.tree));
    }
    if predicate.result != "pass" || run.conclusion.as_deref() != Some("success") {
        return Err(format!(
            "it says {} and its check run {}",
            predicate.result,
            run.conclusion.as_deref().unwrap_or("hasn't concluded")
        ));
    }
    if !predicate.skipped.is_empty() {
        return Err(format!(
            "it skipped {}, whose evidence the train doesn't read",
            predicate.skipped.join(", ")
        ));
    }
    Ok(Claimed {
        attest: Attest::GithubToken {
            statement,
            token: token.to_owned(),
        },
        workflow_sha: workflow_sha.to_owned(),
        hosted: said("runner_environment") == "github-hosted",
    })
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: String,
}

/// A token's claims, once its signature verifies against one of `keys`.
fn verified(token: &str, keys: &[Key]) -> Result<Map<String, Value>, String> {
    let mut parts = token.split('.');
    let (Some(header), Some(claims), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("its token isn't a JWT".into());
    };
    let h: Header = serde_json::from_slice(&unbase(header)?)
        .map_err(|e| format!("its token's header doesn't read: {e}"))?;
    if h.alg != "RS256" {
        return Err(format!("its token is signed with {}", h.alg));
    }
    let key = keys
        .iter()
        .find(|k| k.kid == h.kid && k.kty == "RSA")
        .ok_or_else(|| format!("its token's key {:?} isn't one GitHub publishes", h.kid))?;
    let (n, e) = (unbase(&key.n)?, unbase(&key.e)?);
    RsaPublicKeyComponents {
        n: unpadded(&n),
        e: unpadded(&e),
    }
    .verify(
        &RSA_PKCS1_2048_8192_SHA256,
        format!("{header}.{claims}").as_bytes(),
        &unbase(signature)?,
    )
    .map_err(|_| "its token's signature doesn't verify")?;
    serde_json::from_slice(&unbase(claims)?)
        .map_err(|e| format!("its token's claims don't read: {e}"))
}

fn unbase(part: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| "its token isn't base64url".into())
}

/// A big-endian number without its leading zeros, as ring takes a key's.
fn unpadded(n: &[u8]) -> &[u8] {
    let zeros = n.iter().take_while(|b| **b == 0).count();
    &n[zeros..]
}

fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
