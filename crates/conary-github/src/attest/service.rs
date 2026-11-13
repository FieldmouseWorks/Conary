//! Service-key evidence (`docs/DESIGN.md` section 9, "Jobs on remi";
//! `docs/service-attestation.md`): a result a service on the machine
//! produced (a reviewer, a proof, judge or negative job), signed with that
//! service's own Ed25519 key, which no authoring session or sandbox can
//! read. The signed statement rides in the output of the check run that
//! reports the result. It counts only when the machine's own configuration
//! trusts the key, for the producer the statement names and for the check
//! it attests, and the statement says exactly what the verifier asks about:
//! the repository, the revision, the tree, the check, a result its check
//! run agrees with, and its log's hash. Anything else fails closed.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use conary_core::op::{Attest, Outcome, Repo};

use super::{Attested, MOST, Subject, is_commit, negative_result, pass_result};
use crate::api::CheckRun;

/// What every service statement's first line starts with, whatever its
/// version: a check run's output that does is checked as one, and refused
/// if it isn't a v1 that holds.
const FAMILY: &str = "conary service ";
/// A v1 statement's first line.
pub const HEADER: &str = "conary service v1";
/// The one signature scheme a signature line names.
pub const SCHEME: &str = "ed25519";

/// What a service attests: that `producer` ran `check`, as `definition`
/// defines it, on `tree`, for `revision` of `repository`, with `result`,
/// and that its output hashes to `log`. Its bytes as signed are
/// [`Statement::text`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    /// The producer, as the machine's producer keys name it.
    pub producer: String,
    /// `owner/name`.
    pub repository: String,
    pub revision: Revision,
    /// The tree it ran on: the commit's, or for a negative the base's code
    /// with the head's tests. `None` only for a negative with no test to
    /// run.
    pub tree: Option<String>,
    /// The check run it reports as.
    pub check: String,
    /// The SHA-256, in lowercase hex, of the check's definition as the
    /// machine configures it (a flow proof's recipe,
    /// [`crate::flow::recipe_digest`]): what ran, not only its name. `None`
    /// for a check the machine defines no recipe for. Always present in the
    /// predicate, `null` included.
    #[serde(deserialize_with = "Option::deserialize")]
    pub definition: Option<String>,
    /// `pass` for a commit's check; `fail`, `none` or `pass` for a negative,
    /// as CI's negatives say it (`docs/ci-attestation.md`).
    pub result: String,
    pub log: Log,
}

/// What a statement is about: a commit (no `base`), or a PR revision's
/// head on its merge base, for a negative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub base: Option<String>,
    pub head: String,
}

/// The SHA-256 of the run's output, in lowercase hex: for a review, a
/// judgment or a dependency decision, its receipt
/// ([`crate::flow::review_receipt`] and the rest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Log {
    pub sha256: String,
}

impl Statement {
    /// Its exact bytes, which its signature is over: the header, then the
    /// predicate as one line of JSON.
    pub fn text(&self) -> String {
        let predicate = serde_json::to_string(self).expect("a statement is JSON");
        format!("{HEADER}\n{predicate}\n")
    }
}

/// A statement signed with a producer's key, as its check run's output
/// text carries it: the statement, then `ed25519 KEY SIGNATURE`, the
/// public key and the signature in base64url.
pub fn sign(statement: &Statement, key: &SigningKey) -> String {
    let text = statement.text();
    let signature = key.sign(text.as_bytes());
    format!(
        "{text}{SCHEME} {} {}\n",
        URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

/// The producers whose keys the machine trusts, read from its own
/// configuration (`CONARY_PRODUCER_KEYS`, or the flow's
/// `evidence.producers`): never from a repository, a PR, a check run or a
/// statement. None by default, so with no configuration no service
/// evidence counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Producers(Vec<Producer>);

/// One trusted key: whose it is, and the checks it may attest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Producer {
    pub name: String,
    key: VerifyingKey,
    checks: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    key: String,
    checks: Vec<String>,
}

impl Producers {
    /// The producer keys file at `path`; an unreadable or invalid one is an
    /// error, never an empty set.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A producer keys file: a JSON list of `{"name", "key", "checks"}`,
    /// each key an Ed25519 public key in base64url, each check a check
    /// run's name or a prefix of names ending in `*`. A key belongs to one
    /// producer; a producer may hold several keys, while it rotates them.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let entries: Vec<Entry> =
            serde_json::from_slice(bytes).map_err(|e| format!("producer keys don't read: {e}"))?;
        let mut producers: Vec<Producer> = Vec::new();
        for e in entries {
            if e.name.is_empty() || e.name.trim() != e.name || e.name.contains(char::is_control) {
                return Err(format!("{:?} isn't a producer's name", e.name));
            }
            let key = public_key(&e.key).map_err(|why| format!("{}: {why}", e.name))?;
            if key.is_weak() {
                return Err(format!("{}: its key is a weak one", e.name));
            }
            if e.checks.is_empty() {
                return Err(format!("{}: it names no check it may attest", e.name));
            }
            for check in &e.checks {
                let name = check.strip_suffix('*').unwrap_or(check);
                if name.is_empty() || name.contains('*') {
                    return Err(format!("{}: {check:?} isn't a check or a prefix", e.name));
                }
            }
            if let Some(other) = producers.iter().find(|p| p.key == key) {
                return Err(format!("{}: its key is {}'s already", e.name, other.name));
            }
            producers.push(Producer {
                name: e.name,
                key,
                checks: e.checks,
            });
        }
        Ok(Self(producers))
    }

    fn holding(&self, key: &VerifyingKey) -> Option<&Producer> {
        self.0.iter().find(|p| &p.key == key)
    }
}

impl Producer {
    fn attests(&self, check: &str) -> bool {
        self.checks.iter().any(|c| match c.strip_suffix('*') {
            Some(prefix) => check.starts_with(prefix),
            None => c == check,
        })
    }
}

/// The service statement a check run's output carries, if it carries one:
/// only [`check`] counts it, and nothing else reads it.
pub(super) fn carried(run: &CheckRun) -> Option<&str> {
    run.output
        .as_ref()?
        .text
        .as_deref()
        .filter(|t| t.starts_with(FAMILY))
}

/// Checks a check run's service statement as `run`'s result on `subject`
/// in `repo`, by a producer `producers` trusts for it: the attestation and
/// its result, or why it doesn't count.
pub(super) fn check(
    text: &str,
    producers: &Producers,
    repo: &Repo,
    subject: Subject,
    run: &CheckRun,
) -> Result<Attested, String> {
    if text.len() as u64 > MOST {
        return Err(format!("its service statement is past {MOST} bytes"));
    }
    let (statement, key, signature) = split(text)?;
    let s = verified(statement, key, signature, producers)?;
    if s.check != run.name {
        return Err(format!(
            "its statement is about check {:?}, not {:?}",
            s.check, run.name
        ));
    }
    if !s.repository.eq_ignore_ascii_case(&repo.to_string()) {
        return Err(format!("its statement is {}'s", s.repository));
    }
    if !is_digest(&s.log.sha256) {
        return Err("its statement names no log's SHA-256".into());
    }
    let result = match subject {
        Subject::Commit {
            commit,
            tree,
            definition,
        } => {
            if s.revision.base.is_some() || s.revision.head != commit {
                return Err(format!(
                    "its statement is about {}, not {commit}",
                    about(&s.revision)
                ));
            }
            if s.tree.as_deref() != Some(tree) {
                return Err(format!(
                    "it ran on tree {}, not {tree}",
                    s.tree.as_deref().unwrap_or("none")
                ));
            }
            if s.definition.as_deref() != definition {
                return Err(format!(
                    "it ran the check as defined by {}, and the machine defines it by {}",
                    s.definition.as_deref().unwrap_or("nothing"),
                    definition.unwrap_or("nothing")
                ));
            }
            pass_result(&s.result, run)?
        }
        Subject::Negative { base, head } => {
            if let Some(d) = &s.definition {
                return Err(format!(
                    "it ran the negative as defined by {d}, and the machine defines no recipe for it"
                ));
            }
            if (s.revision.base.as_deref(), s.revision.head.as_str()) != (Some(base), head) {
                return Err(format!(
                    "its statement is about {}, not {head} on {base}",
                    about(&s.revision)
                ));
            }
            let result = negative_result(&s.result, run)?;
            // Tests that ran ran on a tree; none to run ran on none.
            match (&s.tree, result) {
                (None, Outcome::None) => {}
                (Some(tree), Outcome::Fail | Outcome::Pass) if is_commit(tree) => {}
                _ => return Err("its statement's tree doesn't fit its result".into()),
            }
            result
        }
    };
    Ok(Attested {
        attest: Attest::Service {
            statement: statement.to_owned(),
            key: key.to_owned(),
            signature: signature.to_owned(),
        },
        result,
        log: Some(s.log.sha256),
    })
}

/// Whether a kept service attestation still counts: its signature verifies
/// with a key the machine trusts now, for the producer it names and the
/// check it attests. What it was checked to be about was checked when it
/// was recorded, and the ledger keeps it as it was.
pub(super) fn holds(statement: &str, key: &str, signature: &str, producers: &Producers) -> bool {
    verified(statement, key, signature, producers).is_ok()
}

/// A check run's output text as its statement, its key and its signature.
fn split(text: &str) -> Result<(&str, &str, &str), String> {
    let (statement, line) = text
        .strip_suffix('\n')
        .and_then(|t| t.rsplit_once('\n'))
        .ok_or("its service statement isn't a statement and then a signature")?;
    let mut parts = line.split(' ');
    let (Some(SCHEME), Some(key), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("its signature line isn't `{SCHEME} KEY SIGNATURE`"));
    };
    // The statement's own bytes end where the signature line starts.
    Ok((&text[..=statement.len()], key, signature))
}

/// A statement whose signature verifies with a key `producers` trust, for
/// the producer it names and the check it attests.
fn verified(
    statement: &str,
    key: &str,
    signature: &str,
    producers: &Producers,
) -> Result<Statement, String> {
    let predicate = statement
        .strip_prefix(HEADER)
        .and_then(|s| s.strip_prefix('\n'))
        .and_then(|s| s.strip_suffix('\n'))
        .filter(|p| !p.contains('\n'))
        .ok_or_else(|| format!("its statement isn't `{HEADER}` and one predicate"))?;
    let public = public_key(key)?;
    let producer = producers
        .holding(&public)
        .ok_or("it's signed with a key the machine doesn't trust")?;
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|s| s.try_into().ok())
        .ok_or("its signature isn't 64 bytes of base64url")?;
    public
        .verify_strict(statement.as_bytes(), &Signature::from_bytes(&signature))
        .map_err(|_| "its signature doesn't verify")?;
    let s: Statement =
        serde_json::from_str(predicate).map_err(|e| format!("its statement doesn't read: {e}"))?;
    if s.producer != producer.name {
        return Err(format!(
            "its key is {}'s, and it says it's {:?}'s",
            producer.name, s.producer
        ));
    }
    if !producer.attests(&s.check) {
        return Err(format!(
            "{} isn't trusted to attest {:?}",
            producer.name, s.check
        ));
    }
    Ok(s)
}

fn public_key(key: &str) -> Result<VerifyingKey, String> {
    let bytes: [u8; 32] = URL_SAFE_NO_PAD
        .decode(key)
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or("its key isn't 32 bytes of base64url")?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| "its key isn't an Ed25519 key".into())
}

fn about(r: &Revision) -> String {
    match &r.base {
        Some(base) => format!("{} on {base}", r.head),
        None => r.head.clone(),
    }
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests;
