//! Attestations as a private repo's job makes them, signed with a key made
//! for the tests and published as GitHub's, for the tests that check them.

use std::io::{Cursor, Write};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::rsa::PublicKeyComponents;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{AUDIENCE, ISSUER, Key, Signer, hex};

/// A key made for these tests alone, standing in for GitHub's: PKCS#8,
/// base64. It signs nothing anywhere else.
const KEY: &str = concat!(
    "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDTMbc0u9WcKGedhHXEoQmTRRFH",
    "iF4TWLM8hk9b2I5LL8X4c59c1UbOdmtWmXrJYQ/pJv/EOIZkTMIAineVSdKbh9xLOuCZtGiA7EoZ",
    "Hekc8Ifi2JxYVlf+BxO7eHBP/Ut7qpMNaUg6kPZebQlrr+U0LKfUzPmDT94hz01pVjr0WTP+jLAb",
    "qjc0ccCq7yKY4XhniFClubeas1PN1XMZZki6oRHEJZ+eKygBfIveSiodfzx32os/hJ5A+LBu2j8p",
    "Xaf9NLkOXuUmR+lj4qT+FzgkoOlR/r55McwciXfmjsEQpU81N8FxGM2C4t64iDC9QG2lYr9aoh+S",
    "iSWwCm08D9JfAgMBAAECggEALaU2oBaizevrrR9GZteZXrOUTv4Zuxu2LXQn7cVpx7VNF+xBesp7",
    "0b7FZ0OnxqOMUUuFBw9ieIYgfgZvXxxEASqFgdr5VS8pUQgA4dnVmtQfwncnn24V+YjlCmU2/QrV",
    "SCavPrxNCJ9xKCpv2U78fmDSa0tb+DYIYA1SgVxKRKooa19SwPKIL4/3Q70Nm8eGv/2Xx88p+geK",
    "MRpXCc+Sa8N9bAe9WVHUhpItN+w95R+F/SzxZKc6xUomjgpKoNApmrLU+GuA/DZgV1KG2KKd+1vs",
    "How5h4NiGARS94XQBy4t6qKCnk/6drnlehA5Fnmcy9Lg3omAoMgd52UJ8HNIsQKBgQDtMAZfKnyd",
    "UHIVXKFr7hm/C2fHDHHWRlH01TElPm4Ss9DbhariRN7QFa4EK5EChGg5hgjUDkrMUtFuH/iwzhsJ",
    "mQDr4GLgutJWw56Dt6mAUIYOvPEvdEFb5StCM1DblRyqcauHE7e+o8JDn4mrl4ORD41BqVkCMSqP",
    "XbIkTWUdrwKBgQDj8ehs0wtwqeGP5NlOS+kkIw3mItaLy0HtqrV9RJ/YorKWGIEheWAL0zbcv+Gi",
    "HFS9L9kiKq5bv4/J/9ldpZvW3TawrrQNBx5XO8DqFc/asUUumMUVSCx1QGNaF/fznbTr7AiMzO9m",
    "GVN6igoX5kKwn32yOWhJzv0tcRkDWZ7yUQKBgAayBlwFXQMr1CyaMsxTKlLkRYeu6D++OG4K3ppI",
    "it7oSIsJ2OsOOqPxGLpxaGSDzFVjXThvNtlRtSvAp0qGZuj8crQ131SssWdVmlHpi1Vd8IrV9pD6",
    "XSX08CWsQSoRqX9+OfNUMSmmgcNa6432dvZPVBN4blTLgoINiQvRS3ttAoGADVePfy8rMEEBTuba",
    "QVb4IoFawumvHSeciEjBrr9hi9CaG2b7x/XOxWJrh6hDkLWP0DMIxpMw+XXQ+MXc9zpGUOFJ3jWb",
    "nFeaUwjQVeNI80gFAKE4h/Jdh89pAVD91miYumZqabNlH9jDphyGUrfLl4L+XHw3uno/H3XLPQL1",
    "5sECgYEAtEoQb4kmnt2OnVj+X+na4h08wTSQG83F74ijXMZ7NvGYMNtigN5Sr9XRMztsnrn0HrI0",
    "EMOktRrn/78cB7DODiUAmyxqevExBPNmE/peeoc5CFnzcCBVNVZe7Q0M4TtJPNsFzU3qaYXdAv8u",
    "nzMcEni4hKyE9J/gXbHJYBf3w80=",
);
/// The test key's id, as GitHub's keys are named.
pub(crate) const KID: &str = "test-key";
/// The signer workflow the tests' machine names: no repo's in particular.
pub(crate) const SIGNER: &str = "Acme/ci/.github/workflows/gate.yml";
/// The signer workflow's commit in the tests, which they put on its repo's
/// default branch.
pub(crate) const SIGNER_SHA: &str = "5151515151515151515151515151515151515151";

pub(crate) fn signer() -> Signer {
    SIGNER.parse().unwrap()
}

fn pair() -> RsaKeyPair {
    RsaKeyPair::from_pkcs8(&STANDARD.decode(KEY).unwrap()).unwrap()
}

/// The test key, as GitHub publishes its own.
pub(crate) fn key() -> Key {
    let public = PublicKeyComponents::<Vec<u8>>::from(pair().public());
    Key {
        kid: KID.into(),
        kty: "RSA".into(),
        n: URL_SAFE_NO_PAD.encode(&public.n),
        e: URL_SAFE_NO_PAD.encode(&public.e),
    }
}

/// The statement of a push's gate on `commit`, whose tree is `tree`.
pub(crate) fn statement(commit: &str, tree: &str, result: &str) -> String {
    let predicate = json!({"result": result, "revision": null, "tree": tree, "skipped": [],
                           "log": {"sha256": "0".repeat(64), "artifact": "flake-log-1"}});
    format!("conary commit v1\n{commit}\n{predicate}\n")
}

/// The statement of a gate on `tree` naming `gate` and skipping
/// `skipped`: a push's of `commit` with no `base`, or a PR revision's,
/// `commit` on its merge base `base`.
pub(crate) fn gate_statement(
    base: Option<&str>,
    commit: &str,
    tree: &str,
    gate: &Value,
    skipped: &[&str],
) -> String {
    let revision = base.map(|b| json!({"base": b, "head": commit}));
    let predicate = json!({"result": "pass", "revision": revision, "tree": tree,
                           "skipped": skipped,
                           "log": {"sha256": "0".repeat(64), "artifact": "flake-log-1"},
                           "gate": gate});
    match base {
        None => format!("conary commit v1\n{commit}\n{predicate}\n"),
        Some(b) => format!("conary revision v1\n{b}\n{commit}\n{predicate}\n"),
    }
}

/// The statement of a PR's negative run on `head`, whose merge base is
/// `base`.
pub(crate) fn negative_statement(base: &str, head: &str, result: &str) -> String {
    let tree = (result != "none").then(|| "7".repeat(40));
    let predicate = json!({"result": result, "revision": {"base": base, "head": head},
                           "tree": tree, "tests": ["tests/new.rs"], "checks": ["crates"],
                           "skipped": [],
                           "log": {"sha256": "0".repeat(64), "artifact": "negative-log-1"}});
    format!("conary negative v1\n{base}\n{head}\n{predicate}\n")
}

/// The claims GitHub gives the job of check run `run` in `repo`, pushed
/// at `commit`, asking for `statement`'s audience.
pub(crate) fn claims(repo: &str, commit: &str, run: u64, statement: &str) -> Value {
    json!({
        "iss": ISSUER,
        "aud": format!("{AUDIENCE}{}", hex(&Sha256::digest(statement))),
        "repository": repo,
        "sha": commit,
        "check_run_id": run.to_string(),
        "job_workflow_ref": format!("{SIGNER}@{SIGNER_SHA}"),
        "job_workflow_sha": SIGNER_SHA,
        "event_name": "push",
        "runner_environment": "self-hosted",
    })
}

/// A token over `claims` with `header`, signed with the test key.
pub(crate) fn token_with(header: &Value, claims: &Value) -> String {
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let pair = pair();
    let mut signature = vec![0; pair.public().modulus_len()];
    pair.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
}

/// A token over `claims`, as GitHub signs one.
pub(crate) fn token(claims: &Value) -> String {
    token_with(&json!({"alg": "RS256", "kid": KID, "typ": "JWT"}), claims)
}

/// An attestation's file: its statement, then its token.
pub(crate) fn file(statement: &str, token: &str) -> String {
    format!("{statement}{token}\n")
}

/// GitHub's zip of an artifact holding `bytes` as its file `name`.
pub(crate) fn zipped(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        name,
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(bytes).unwrap();
    zip.finish().unwrap().into_inner()
}

/// The producer the tests' machine trusts, a service on it.
pub(crate) const PRODUCER: &str = "remi-reviewer";

/// The trusted producer's key: made for these tests alone.
pub(crate) fn producer_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[7; 32])
}

/// A key nobody trusts: one the change's own author made.
pub(crate) fn author_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[9; 32])
}

/// A producer keys file trusting each `(name, key, checks)`.
pub(crate) fn producers_file(entries: &[(&str, &ed25519_dalek::SigningKey, &[&str])]) -> Vec<u8> {
    let entries: Vec<Value> = entries
        .iter()
        .map(|(name, key, checks)| {
            json!({"name": name,
                   "key": URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
                   "checks": checks})
        })
        .collect();
    serde_json::to_vec(&entries).unwrap()
}

/// The machine's producers in the tests: [`PRODUCER`], for `checks`.
pub(crate) fn producers(checks: &[&str]) -> super::service::Producers {
    super::service::Producers::parse(&producers_file(&[(PRODUCER, &producer_key(), checks)]))
        .unwrap()
}

/// [`PRODUCER`]'s statement of `check` passing on `commit` of `repo`,
/// whose tree is `tree`, with output hashing to `log`.
pub(crate) fn service_statement(
    repo: &str,
    commit: &str,
    tree: &str,
    check: &str,
    log: &str,
) -> super::service::Statement {
    super::service::Statement {
        producer: PRODUCER.into(),
        repository: repo.into(),
        revision: super::service::Revision {
            base: None,
            head: commit.into(),
        },
        tree: Some(tree.into()),
        check: check.into(),
        definition: None,
        result: "pass".into(),
        log: super::service::Log { sha256: log.into() },
    }
}
