//! Service-key producer evidence (`docs/service-attestation.md`) through the
//! real pass: a review and a negative a trusted service signed grant
//! permission beside the CI gate's attestation, and nothing altered,
//! unknown or author-controlled does.
use super::flow_pass_case::{Case, REPO};
use super::{TRUNK, head};
use crate::api::Source;
use crate::attest::service::{Statement, sign};
use crate::attest::testing::{self, PRODUCER, author_key, producer_key, producers_file};
use crate::branches::Branches;
use crate::flow::*;
use crate::sim::Call;
use ed25519_dalek::SigningKey;
use serde_json::json;

const REVIEW: &str = "conary/review/reviewer";

/// The case, with the machine trusting [`PRODUCER`] for reviews and
/// negatives, as `trusted` says, in a producer keys file of its own.
fn case(trusted: &[u8]) -> (Case, tempfile::TempDir) {
    let mut c = Case::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("producers.json");
    std::fs::write(&path, trusted).unwrap();
    c.config.evidence.as_mut().unwrap().producers = Some(path);
    (c, dir)
}

fn trusted() -> Vec<u8> {
    producers_file(&[(PRODUCER, &producer_key(), &["conary/review/*", "negative"])])
}

/// A check run on `head` reporting `text`, as a service posts one.
fn post(c: &Case, head: &str, name: &str, conclusion: &str, text: &str) {
    let id = c.hub.run_id();
    c.hub.add_check(
        head,
        json!({"id": id, "name": name, "status": "completed", "conclusion": conclusion,
               "started_at": "2026-11-01T00:00:00Z", "output": {"text": text}}),
    );
}

/// A clean review, published, and the service's statement of it on the
/// head, changed by `change` and signed with `key`; changed afterwards,
/// as text, by `tamper`.
fn review(
    c: &Case,
    key: &SigningKey,
    change: impl FnOnce(&mut Statement),
    tamper: impl FnOnce(String) -> String,
) {
    c.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 0);
    let records = c.hub.reviews(REPO, 3).unwrap();
    let body = records.last().unwrap().body.clone().unwrap();
    let receipt = review_receipt(&head(3), &body);
    let tree = Branches::tree(&c.hub, REPO, &head(3)).unwrap();
    let mut s =
        testing::service_statement(REPO, &head(3), &tree, REVIEW, &receipt_digest(&receipt));
    change(&mut s);
    post(c, &head(3), REVIEW, "success", &tamper(sign(&s, key)));
}

/// The service's negative of the head on its base, its CI one forgotten:
/// its new tests did `result` on the old code.
fn negative(c: &Case, result: &str) {
    for run in c
        .hub
        .checks_on(&head(3))
        .iter()
        .filter(|r| r.name == "negative")
    {
        c.hub.forget_artifacts(Some(run.id));
    }
    let base = c.hub.merge_base(REPO, TRUNK, &head(3)).unwrap();
    let mut s =
        testing::service_statement(REPO, &head(3), &"7".repeat(40), "negative", &"0".repeat(64));
    s.revision.base = Some(base);
    s.result = result.into();
    let conclusion = if result == "pass" {
        "failure"
    } else {
        "success"
    };
    post(
        c,
        &head(3),
        "negative",
        conclusion,
        &sign(&s, &producer_key()),
    );
}

fn landed(c: &Case) -> bool {
    c.hub.pull(REPO, 3).unwrap().is_merged()
}

fn pending(c: &Case, why: &str) {
    let settled = c.hub.statuses(REPO, &head(3)).unwrap();
    let settled = settled.iter().find(|s| s.context == SETTLED).unwrap();
    assert_eq!(settled.state, "pending", "{why}");
    assert_eq!(c.hub.count(Call::Land), 0, "{why}");
    assert!(!landed(c), "{why}");
}

#[test]
fn a_trusted_services_review_and_negative_grant_permission_beside_ci() {
    let (c, _keys) = case(&trusted());
    review(&c, &producer_key(), |_| {}, |t| t);
    negative(&c, "fail");
    let reports = c.pass(jobs::now(), false);
    assert_eq!(reports[0].error, None);
    assert!(landed(&c), "{:?}", reports[0].next);
}

#[test]
fn altered_unknown_or_author_controlled_service_evidence_cannot_grant_permission() {
    let other = "4".repeat(40);
    type Change = Box<dyn FnOnce(&mut Statement)>;
    type Tamper = Box<dyn FnOnce(String) -> String>;
    let keep: fn() -> Tamper = || Box::new(|t| t);
    let cases: Vec<((&str, &str), SigningKey, Change, Tamper)> = vec![
        (
            ("a result altered after signing", "signature doesn't verify"),
            producer_key(),
            Box::new(|s| s.result = "fail".into()),
            Box::new(|t| t.replacen(r#""result":"fail""#, r#""result":"pass""#, 1)),
        ),
        (
            ("a log of another review", "does not bind this exact result"),
            producer_key(),
            Box::new(|s| s.log.sha256 = receipt_digest(b"another review")),
            keep(),
        ),
        (
            ("another revision", "its statement is about 4444"),
            producer_key(),
            Box::new({
                let other = other.clone();
                move |s| s.revision.head = other
            }),
            keep(),
        ),
        (
            ("another tree", "it ran on tree 4444"),
            producer_key(),
            Box::new({
                let other = other.clone();
                move |s| s.tree = Some(other)
            }),
            keep(),
        ),
        (
            ("an unknown key", "doesn't trust"),
            SigningKey::from_bytes(&[11; 32]),
            Box::new(|_| {}),
            keep(),
        ),
        (
            (
                "the author's own key, naming the trusted producer",
                "doesn't trust",
            ),
            author_key(),
            Box::new(|s| s.producer = PRODUCER.into()),
            keep(),
        ),
        (
            (
                "the author naming itself the producer",
                "its key is remi-reviewer's",
            ),
            producer_key(),
            Box::new(|s| s.producer = "owner".into()),
            keep(),
        ),
    ];
    for ((why, says), key, change, tamper) in cases {
        let (c, _keys) = case(&trusted());
        review(&c, &key, change, tamper);
        negative(&c, "fail");
        let reports = c.pass(jobs::now(), false);
        let next = reports[0].next.as_ref().unwrap();
        assert_eq!(next.step, Step::Wait { until: None }, "{why}");
        assert!(
            next.description.contains(says),
            "{why}: {}",
            next.description
        );
        pending(&c, why);
    }
}

#[test]
fn service_evidence_counts_only_under_the_machines_own_producer_keys() {
    // A producer the machine trusts for proofs alone signs no review.
    let proofs_only =
        producers_file(&[(PRODUCER, &producer_key(), &["conary/proof/*", "negative"])]);
    // An author's key the machine doesn't list counts for nothing, even
    // beside the trusted one.
    let unlisted = trusted();
    for (why, file, key) in [
        ("out of its scope", proofs_only, producer_key()),
        ("unlisted", unlisted, author_key()),
    ] {
        let (c, _keys) = case(&file);
        review(&c, &key, |_| {}, |t| t);
        negative(&c, "fail");
        c.pass(jobs::now(), false);
        pending(&c, why);
    }
    // No producer keys, or a file that doesn't read: no service counts.
    let (mut c, _keys) = case(&trusted());
    c.config.evidence.as_mut().unwrap().producers = None;
    review(&c, &producer_key(), |_| {}, |t| t);
    negative(&c, "fail");
    c.pass(jobs::now(), false);
    pending(&c, "none configured");
    for broken in [&b"not json"[..], b"[]"] {
        let (c, _keys) = case(broken);
        review(&c, &producer_key(), |_| {}, |t| t);
        negative(&c, "fail");
        c.pass(jobs::now(), false);
        pending(&c, &String::from_utf8_lossy(broken));
    }
}

#[test]
fn a_services_negative_must_say_the_new_tests_failed_on_the_old_code() {
    let (c, _keys) = case(&trusted());
    review(&c, &producer_key(), |_| {}, |t| t);
    negative(&c, "pass");
    c.pass(jobs::now(), false);
    pending(&c, "a passing negative");
}
