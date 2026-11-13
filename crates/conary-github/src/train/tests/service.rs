//! Service-key evidence in the train (`docs/service-attestation.md`): a
//! required check a trusted service signed counts for the candidate's own
//! commit and tree, an R2 one's included, and a negative a trusted service
//! signed lets a PR join. Another tree, commit, key or log doesn't.

use super::*;
use crate::attest::service::{Statement, sign};
use crate::attest::testing::{author_key, producer_key, producers, service_statement};
use conary_core::op::{Attest, CheckKind, Outcome};

const PROOF: &str = "proof";
const NEGATIVE: &str = "check / negative";

/// Settings whose one required check is the proof a service runs, and
/// which trust the test producer for it and for negatives.
fn trusting() -> Settings {
    let mut s = settings();
    s.require = vec![PROOF.into()];
    s.trust.producers = producers(&[PROOF, NEGATIVE]);
    s
}

/// R2 PR #3 in a candidate of its own, gating on a trunk that moved after
/// its head was made, so the candidate's tree isn't the head's: the
/// candidate's id, commit and tree.
fn gating_r2(store: &mut Store, hub: &Hub) -> (CandidateId, String, String) {
    r2(hub, 3);
    let moved = hub.add(TRUNK, "elsewhere");
    hub.set_branch(REMI, "main", &moved);
    join_all(store, hub, &[3]);
    let (id, _) = gating(&step_once(store, hub));
    let c = store.ledger().candidate(id).unwrap();
    assert_ne!(c.tree.to_string(), hub.tree_at(&head(3)));
    (id, c.commit.to_string(), c.tree.to_string())
}

/// The proof's passing check run on `commit`, its output `text`.
fn proof(hub: &Hub, commit: &str, text: &str) {
    hub.put_checks(
        commit,
        json!([{"id": 31, "name": PROOF, "status": "completed", "conclusion": "success",
                "html_url": "https://remi.example/proof/31",
                "started_at": "2026-11-05T05:00:00Z", "output": {"text": text}}]),
    );
}

fn statement(commit: &str, tree: &str) -> Statement {
    service_statement(&repo().to_string(), commit, tree, PROOF, &"c".repeat(64))
}

#[test]
fn an_r2_candidate_lands_on_a_trusted_services_pass_for_its_own_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    let (id, commit, tree) = gating_r2(&mut store, &hub);
    let text = sign(&statement(&commit, &tree), &producer_key());
    proof(&hub, &commit, &text);
    let stepped = step(&mut store, &hub, &hub, &config(), &repo(), &trusting()).unwrap();
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
    assert_eq!(stepped.landed, [3]);
    let evidence = store.ledger().candidate(id).unwrap().evidence.clone();
    assert!(
        matches!(&evidence.last().unwrap().attest,
            Some(Attest::Service { statement, .. }) if text.starts_with(statement.as_str())),
        "{evidence:?}"
    );
}

#[test]
fn a_services_pass_for_another_tree_commit_key_or_log_leaves_the_gate_waiting() {
    let tampered = |text: String| text.replacen(&"c".repeat(64), &"d".repeat(64), 1);
    type Make = Box<dyn Fn(&Hub, &str, &str) -> String>;
    let cases: Vec<(&str, &str, Make)> = vec![
        (
            "the PR's own pass, on its head",
            "is about",
            Box::new(|hub, _, _| {
                sign(
                    &statement(&head(3), &hub.tree_at(&head(3))),
                    &producer_key(),
                )
            }),
        ),
        (
            "the candidate's commit, the head's tree",
            "it ran on tree",
            Box::new(|hub, commit, _| {
                sign(&statement(commit, &hub.tree_at(&head(3))), &producer_key())
            }),
        ),
        (
            "an untrusted key",
            "doesn't trust",
            Box::new(|_, commit, tree| sign(&statement(commit, tree), &author_key())),
        ),
        (
            "a log altered after signing",
            "doesn't verify",
            Box::new(move |_, commit, tree| {
                tampered(sign(&statement(commit, tree), &producer_key()))
            }),
        ),
    ];
    for (why, says, make) in cases {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github(&[3]);
        let (id, commit, tree) = gating_r2(&mut store, &hub);
        proof(&hub, &commit, &make(&hub, &commit, &tree));
        let stepped = step(&mut store, &hub, &hub, &config(), &repo(), &trusting()).unwrap();
        assert_eq!(gating(&stepped).0, id, "{why}");
        let waiting = waiting(&stepped);
        assert!(
            waiting.len() == 1 && waiting[0].starts_with("proof: ") && waiting[0].contains(says),
            "{why}: {waiting:?}"
        );
        assert!(stepped.landed.is_empty(), "{why}");
    }
}

#[test]
fn a_kept_service_pass_counts_only_while_the_machine_trusts_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    let (id, commit, tree) = gating_r2(&mut store, &hub);
    proof(
        &hub,
        &commit,
        &sign(&statement(&commit, &tree), &producer_key()),
    );
    // Something else still runs, so the proof's pass is kept, checked.
    hub.add_check(
        &commit,
        json!({"id": 32, "name": "slow", "status": "in_progress", "conclusion": null,
               "html_url": null}),
    );
    let stepped = step(&mut store, &hub, &hub, &config(), &repo(), &trusting()).unwrap();
    assert_eq!(waiting(&stepped), ["slow"]);
    // The machine stops trusting the key: what the ledger kept no longer
    // counts, and neither does the run's own word.
    let mut rotated = trusting();
    rotated.trust.producers = producers(&[NEGATIVE]);
    let stepped = step(&mut store, &hub, &hub, &config(), &repo(), &rotated).unwrap();
    assert_eq!(gating(&stepped).0, id);
    let waiting = waiting(&stepped);
    assert!(
        waiting
            .iter()
            .any(|w| w.starts_with("proof: ") && w.contains("isn't trusted to attest")),
        "{waiting:?}"
    );
}

#[test]
fn a_kept_service_pass_cannot_attest_another_check_with_the_same_link() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    let (id, commit, tree) = gating_r2(&mut store, &hub);
    proof(
        &hub,
        &commit,
        &sign(&statement(&commit, &tree), &producer_key()),
    );
    hub.add_check(
        &commit,
        json!({"id": 32, "name": "slow", "status": "in_progress", "conclusion": null}),
    );
    assert_eq!(
        waiting(&step(&mut store, &hub, &hub, &config(), &repo(), &trusting()).unwrap()),
        ["slow"]
    );
    // Check links are supplied by their publisher, so two checks can share
    // one. The proof's checked receipt grants nothing to the other check.
    hub.add_check(
        &commit,
        json!({"id": 33, "name": "another", "status": "completed", "conclusion": "success",
               "html_url": "https://remi.example/proof/31"}),
    );
    let mut settings = trusting();
    settings.require.push("another".into());
    let stepped = step(&mut store, &hub, &hub, &config(), &repo(), &settings).unwrap();
    assert_eq!(gating(&stepped).0, id);
    assert!(
        waiting(&stepped).iter().any(|w| w.starts_with("another: ")),
        "{:?}",
        waiting(&stepped)
    );
    assert!(stepped.landed.is_empty());
}

/// PR #3's negative, as the service signed it with `key`: its new tests
/// did `result` on its base's code.
fn negative(hub: &Hub, key: &ed25519_dalek::SigningKey, result: &str) {
    let mut s = service_statement(
        &repo().to_string(),
        &head(3),
        &"7".repeat(40),
        NEGATIVE,
        &"0".repeat(64),
    );
    s.revision.base = Some(hub.merge_base(REMI, TRUNK, &head(3)).unwrap());
    s.result = result.into();
    let conclusion = if result == "pass" {
        "failure"
    } else {
        "success"
    };
    hub.put_checks(
        &head(3),
        json!([{"id": 33, "name": NEGATIVE, "status": "completed", "conclusion": conclusion,
                "html_url": "https://remi.example/negative/33",
                "started_at": "2026-11-05T05:00:00Z",
                "output": {"text": sign(&s, key)}}]),
    );
}

fn join_negative(store: &mut Store, hub: &Hub) -> Joined {
    let n = Negatives {
        check: NEGATIVE.into(),
        trust: trusting().trust,
    };
    join(
        store,
        hub,
        hub,
        &config(),
        &repo(),
        3,
        &head(3),
        &[],
        &policy(),
        Some(&n),
    )
    .unwrap()
}

#[test]
fn a_pr_joins_on_a_trusted_services_negative_and_on_nothing_less() {
    for (key, result, joins) in [
        (producer_key(), "fail", true),
        (producer_key(), "none", true),
        (producer_key(), "pass", false),
        (author_key(), "fail", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github(&[3]);
        if result == "none" {
            // Nothing to run ran on no tree.
            let mut s =
                service_statement(&repo().to_string(), &head(3), "", NEGATIVE, &"0".repeat(64));
            s.revision.base = Some(hub.merge_base(REMI, TRUNK, &head(3)).unwrap());
            s.tree = None;
            s.result = "none".into();
            hub.put_checks(
                &head(3),
                json!([{"id": 33, "name": NEGATIVE, "status": "completed", "conclusion": "success",
                        "html_url": "https://remi.example/negative/33",
                        "output": {"text": sign(&s, &key)}}]),
            );
        } else {
            negative(&hub, &key, result);
        }
        let joined = join_negative(&mut store, &hub);
        assert_eq!(joined == Joined::Joined, joins, "{result}: {joined:?}");
        let kept: Vec<_> = store
            .ledger()
            .by_pr(&repo(), 3)
            .unwrap()
            .evidence
            .iter()
            .filter(|e| e.kind == CheckKind::Negative)
            .map(|e| (e.result, matches!(e.attest, Some(Attest::Service { .. }))))
            .collect();
        let recorded = key == producer_key();
        let expected: Vec<(Outcome, bool)> = if recorded {
            vec![(
                match result {
                    "fail" => Outcome::Fail,
                    "none" => Outcome::None,
                    _ => Outcome::Pass,
                },
                true,
            )]
        } else {
            vec![]
        };
        assert_eq!(kept, expected, "{result}");
    }
}
