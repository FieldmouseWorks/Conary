//! A required CI result that skipped checks (`docs/DESIGN.md` section 4.7):
//! it counts once each check it skipped is a required machine proof a
//! service attests on the same candidate, and waits on anything less.

use super::*;
use crate::attest::service::sign;
use crate::attest::testing::{author_key, producer_key, producers, service_statement};
use conary_core::op::Attest;

const GATE: &str = "check / check";
const AGENTS: &str = "agents";

/// Settings requiring the CI gate and, as `proofs` say, the machine's
/// proofs, and trusting the test producer for `agents` and `vm`.
fn requiring(proofs: &[&str]) -> Settings {
    let mut s = settings();
    s.require = std::iter::once(GATE)
        .chain(proofs.iter().copied())
        .map(String::from)
        .collect();
    s.trust.producers = producers(&[AGENTS, "vm"]);
    s
}

/// PR #3 in a candidate, gating: its id, commit and tree.
fn gating_one(store: &mut Store, hub: &Hub) -> (CandidateId, String, String) {
    join_all(store, hub, &[3]);
    let (id, _) = gating(&step_once(store, hub));
    let c = store.ledger().candidate(id).unwrap();
    (id, c.commit.to_string(), c.tree.to_string())
}

/// The CI gate's passing run on `commit`, run 41, attested by its job as
/// having skipped `skipped`.
fn gate(hub: &Hub, commit: &str, tree: &str, skipped: &[&str]) {
    hub.add_check(
        commit,
        json!({"id": 41, "name": GATE, "status": "completed", "conclusion": "success",
               "html_url": "https://github.com/FieldmouseWorks/remi/actions/runs/41"}),
    );
    let statement = testing::statement(commit, tree, "pass").replace(
        r#""skipped":[]"#,
        &format!(r#""skipped":{}"#, json!(skipped)),
    );
    let claims = testing::claims(REMI, commit, 41, &statement);
    hub.attest(41, &testing::file(&statement, &testing::token(&claims)));
}

/// A passing run of `check` on `commit`, run `id`, its output `text`.
fn proof(hub: &Hub, commit: &str, id: u64, check: &str, text: Option<&str>) {
    hub.add_check(
        commit,
        json!({"id": id, "name": check, "status": "completed", "conclusion": "success",
               "html_url": format!("https://remi.example/proof/{id}"),
               "output": {"text": text}}),
    );
}

/// The test producer's statement of `check` passing on `commit` and
/// `tree`, signed with `key`.
fn signed(commit: &str, tree: &str, check: &str, key: &ed25519_dalek::SigningKey) -> String {
    let s = service_statement(&repo().to_string(), commit, tree, check, &"c".repeat(64));
    sign(&s, key)
}

fn uncovered(waiting: &[String], skipped: &str) -> bool {
    waiting.iter().any(|w| {
        w == &format!(
            "{GATE}: it skipped {skipped}, which no required machine proof of this candidate attests"
        )
    })
}

#[test]
fn a_gate_that_skipped_a_check_counts_once_a_required_machine_proof_covers_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    let (id, commit, tree) = gating_one(&mut store, &hub);
    gate(&hub, &commit, &tree, &[AGENTS]);
    let text = signed(&commit, &tree, AGENTS, &producer_key());
    proof(&hub, &commit, 42, AGENTS, Some(&text));
    let stepped = step(
        &mut store,
        &hub,
        &hub,
        &config(),
        &repo(),
        &requiring(&[AGENTS]),
    )
    .unwrap();
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
    assert_eq!(stepped.landed, [3]);
    // Both passes are kept with what attested them, the gate's skip
    // included.
    let evidence = store.ledger().candidate(id).unwrap().evidence.clone();
    let attest = |check: &str| {
        evidence
            .iter()
            .find(|e| e.check == check)
            .and_then(|e| e.attest.clone())
    };
    assert!(
        matches!(attest(GATE), Some(Attest::GithubToken { statement, .. })
            if statement.contains(r#""skipped":["agents"]"#)),
        "{evidence:?}"
    );
    assert!(matches!(attest(AGENTS), Some(Attest::Service { .. })));
}

#[test]
fn a_gate_that_skipped_a_check_waits_on_anything_less_than_a_required_machine_proof() {
    let other_tree = "7".repeat(40);
    type Make = Box<dyn Fn(&Hub, &str, &str)>;
    let cases: Vec<(&str, &[&str], &[&str], Make)> = vec![
        (
            "no proof at all",
            &[AGENTS],
            &[],
            Box::new(|_: &Hub, _: &str, _: &str| {}),
        ),
        (
            "a trusted proof the train doesn't require",
            &[AGENTS],
            &[],
            Box::new(|hub: &Hub, commit: &str, tree: &str| {
                let text = signed(commit, tree, AGENTS, &producer_key());
                proof(hub, commit, 42, AGENTS, Some(&text));
            }),
        ),
        (
            "a required check of its name that CI attests, not a service",
            &[AGENTS],
            &[AGENTS],
            Box::new(|hub: &Hub, commit: &str, _: &str| {
                proof(hub, commit, 42, AGENTS, None);
                hub.attest(42, &hub.attestation(commit, 42));
            }),
        ),
        (
            "a required proof signed for another tree",
            &[AGENTS],
            &[AGENTS],
            Box::new(move |hub: &Hub, commit: &str, _: &str| {
                let text = signed(commit, &other_tree, AGENTS, &producer_key());
                proof(hub, commit, 42, AGENTS, Some(&text));
            }),
        ),
        (
            "a required proof signed with a key the machine doesn't trust",
            &[AGENTS],
            &[AGENTS],
            Box::new(|hub: &Hub, commit: &str, tree: &str| {
                let text = signed(commit, tree, AGENTS, &author_key());
                proof(hub, commit, 42, AGENTS, Some(&text));
            }),
        ),
        (
            "a required proof of another check",
            &[AGENTS],
            &["vm"],
            Box::new(|hub: &Hub, commit: &str, tree: &str| {
                let text = signed(commit, tree, "vm", &producer_key());
                proof(hub, commit, 42, "vm", Some(&text));
            }),
        ),
    ];
    for (why, skipped, required, make) in cases {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github(&[3]);
        let (id, commit, tree) = gating_one(&mut store, &hub);
        gate(&hub, &commit, &tree, skipped);
        make(&hub, &commit, &tree);
        let stepped = step(
            &mut store,
            &hub,
            &hub,
            &config(),
            &repo(),
            &requiring(required),
        )
        .unwrap();
        assert_eq!(gating(&stepped).0, id, "{why}");
        let waiting = waiting(&stepped);
        assert!(uncovered(&waiting, AGENTS), "{why}: {waiting:?}");
        assert!(stepped.landed.is_empty(), "{why}");
    }
}

#[test]
fn every_check_a_gate_skipped_needs_its_own_proof() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    let (id, commit, tree) = gating_one(&mut store, &hub);
    gate(&hub, &commit, &tree, &[AGENTS, "vm"]);
    let text = signed(&commit, &tree, AGENTS, &producer_key());
    proof(&hub, &commit, 42, AGENTS, Some(&text));
    let settings = requiring(&[AGENTS]);
    let stepped = step(&mut store, &hub, &hub, &config(), &repo(), &settings).unwrap();
    assert_eq!(gating(&stepped).0, id);
    assert_eq!(waiting(&stepped).len(), 1, "{:?}", waiting(&stepped));
    assert!(uncovered(&waiting(&stepped), "vm"));
    // Its proof too, and it counts.
    let text = signed(&commit, &tree, "vm", &producer_key());
    proof(&hub, &commit, 43, "vm", Some(&text));
    let stepped = step(
        &mut store,
        &hub,
        &hub,
        &config(),
        &repo(),
        &requiring(&[AGENTS, "vm"]),
    )
    .unwrap();
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
}

#[test]
fn a_kept_gate_that_skipped_a_check_still_needs_the_proof() {
    for proved in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github(&[3]);
        let (id, commit, tree) = gating_one(&mut store, &hub);
        gate(&hub, &commit, &tree, &[AGENTS]);
        hub.add_check(
            &commit,
            json!({"id": 44, "name": "slow", "status": "in_progress", "conclusion": null,
                   "html_url": null}),
        );
        let settings = requiring(&[AGENTS]);
        let stepped = step(&mut store, &hub, &hub, &config(), &repo(), &settings).unwrap();
        assert!(uncovered(&waiting(&stepped), AGENTS), "{proved}");
        // Only what the ledger kept of the gate's attestation is left to
        // count, and it still says what the gate skipped.
        hub.forget_artifacts(Some(41));
        hub.remove_check(&commit, "slow");
        hub.add_check(
            &commit,
            json!({"id": 44, "name": "slow", "status": "completed", "conclusion": "success",
                   "html_url": null}),
        );
        if proved {
            let text = signed(&commit, &tree, AGENTS, &producer_key());
            proof(&hub, &commit, 42, AGENTS, Some(&text));
        }
        let stepped = step(&mut store, &hub, &hub, &config(), &repo(), &settings).unwrap();
        if proved {
            assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
        } else {
            assert_eq!(gating(&stepped).0, id);
            assert!(
                uncovered(&waiting(&stepped), AGENTS),
                "{:?}",
                waiting(&stepped)
            );
        }
    }
}
