// apps/conary-test/src/explorer/tests.rs
#![cfg(test)]

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use contract::*;
use controller::{Campaign, Environment, Episode};
use evidence::{Identity, Replay};
use selector::{Scripted, Seeded, Selector};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
};

mod jev;

fn empty() -> Facts {
    Facts {
        packages: BTreeMap::new(),
        owners: BTreeMap::new(),
        payloads: BTreeMap::new(),
        complete: true,
    }
}
fn observation() -> Observation {
    Observation {
        version: VERSION,
        environment: "unit-fixture-not-a-real-container".into(),
        epoch: 1,
        revision: 0,
        facts: empty(),
    }
}
fn identity() -> Identity {
    Identity {
        source_revision: "a".repeat(40),
        conary_sha256: "b".repeat(64),
        image: format!("sha256:{}", "c".repeat(64)),
        fixtures: BTreeMap::new(),
    }
}

/// Test double only. Actual Conary proof requires the approved VM adapter.
struct Fake {
    current: Observation,
    dispatched: usize,
    observed: usize,
    fail_guard: bool,
    fail_cleanup: bool,
    fail_execute: bool,
    incomplete: bool,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            current: observation(),
            dispatched: 0,
            observed: 0,
            fail_guard: false,
            fail_cleanup: false,
            fail_execute: false,
            incomplete: false,
        }
    }
}
#[async_trait]
impl Environment for Fake {
    async fn reset(&mut self) -> Result<Observation> {
        self.current = observation();
        self.current.epoch += 1;
        Ok(self.current.clone())
    }
    async fn verify(&self) -> Result<()> {
        anyhow::ensure!(!self.fail_guard, "injected guard failure");
        Ok(())
    }
    async fn observe(&mut self) -> Result<Observation> {
        self.observed += 1;
        let mut result = self.current.clone();
        result.facts.complete = !self.incomplete;
        Ok(result)
    }
    async fn execute(&mut self, operation_id: &str, action: &Action) -> Result<Receipt> {
        self.dispatched += 1;
        anyhow::ensure!(!self.fail_execute, "injected unknown execution outcome");
        let mut exit_code = 0;
        match action {
            Action::Install(f) => {
                self.current
                    .facts
                    .packages
                    .insert(f.package(), f.version().into());
                self.current
                    .facts
                    .owners
                    .insert(f.package(), f.package().name().into());
                self.current
                    .facts
                    .payloads
                    .insert(f.package(), hex::encode(Sha256::digest(f.payload())));
            }
            Action::Remove(p) => {
                if self.current.facts.packages.remove(p).is_none() {
                    exit_code = 1;
                }
                self.current.facts.owners.remove(p);
                self.current.facts.payloads.remove(p);
            }
            _ => {}
        }
        self.current.revision += 1;
        Ok(Receipt {
            operation_id: operation_id.into(),
            action: action.clone(),
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
    async fn close(&mut self) -> Result<()> {
        anyhow::ensure!(!self.fail_cleanup, "injected cleanup failure");
        Ok(())
    }
}

#[test]
fn guards_reject_unknown_stale_rebound_injection_and_forged_actions() {
    let observed = observation();
    let request = DecisionRequest::new(observed.clone(), 8).unwrap();
    let valid = Decision {
        candidate_id: request.candidates[0].id.clone(),
        binding: request.binding.clone(),
    };
    assert!(request.authorize(&valid, &observed).is_ok());
    for id in ["unknown", "../root", "c1; touch /tmp/unsafe", "$(whoami)"] {
        assert!(
            request
                .authorize(
                    &Decision {
                        candidate_id: id.into(),
                        ..valid.clone()
                    },
                    &observed
                )
                .is_err()
        );
    }
    for changed in [
        Observation {
            environment: "host".into(),
            ..observed.clone()
        },
        Observation {
            epoch: 99,
            ..observed.clone()
        },
        Observation {
            revision: 1,
            ..observed.clone()
        },
    ] {
        assert!(request.authorize(&valid, &changed).is_err());
    }
    let mut rebound = request.clone();
    rebound.candidates[0].action = Action::Remove(Package::App);
    assert!(rebound.authorize(&valid, &observed).is_err());
    assert!(
        DecisionRequest::new(observed.clone(), 0)
            .unwrap()
            .authorize(&valid, &observed)
            .is_err()
    );
    assert!(
        serde_json::from_str::<Decision>(r#"{"candidate_id":"c0","binding":"x","command":"rm"}"#)
            .is_err()
    );
    assert!(serde_json::from_str::<Action>(r#"{"kind":"install","fixture":"/tmp/evil"}"#).is_err());
}

#[tokio::test]
async fn observations_change_seeded_choices_and_preserve_negative_candidates() {
    let mut observed = observation();
    let before = DecisionRequest::new(observed.clone(), 10).unwrap();
    observed.facts.packages.insert(Package::App, "1.0.0".into());
    observed.revision += 1;
    let after = DecisionRequest::new(observed, 9).unwrap();
    assert_ne!(before.candidates, after.candidates);
    assert!(
        before
            .candidates
            .iter()
            .any(|c| c.action == Action::Install(Fixture::AppV1))
    );
    assert!(
        !after
            .candidates
            .iter()
            .any(|c| c.action == Action::Install(Fixture::AppV1))
    );
    assert!(
        after
            .candidates
            .iter()
            .any(|c| c.action == Action::NegativeControl)
    );
    for seed in 0..32 {
        let mut selector = Seeded::new(seed);
        let decision = selector.select(&after).await.unwrap();
        assert!(after.authorize(&decision, &after.observation).is_ok());
    }
}

#[test]
fn independent_checker_requires_bytes_ownership_and_complete_evidence() {
    let mut oracle = checker::Oracle::default();
    oracle.0.insert(Package::App, Fixture::AppV1);
    let mut facts = empty();
    facts.packages.insert(Package::App, "1.0.0".into());
    facts.owners.insert(Package::App, "redshirt-app".into());
    facts.payloads.insert(
        Package::App,
        hex::encode(Sha256::digest(b"redshirt app v1\n")),
    );
    assert!(
        oracle
            .evaluate(&facts, false)
            .iter()
            .all(|e| e.passed == Some(true))
    );
    assert!(oracle.evaluate(&facts, true).iter().any(|e| e.classification == Classification::NegativeControl && e.passed == Some(false)));
    facts.payloads.insert(Package::App, "broken".into());
    assert!(
        oracle
            .evaluate(&facts, false)
            .iter()
            .any(|e| e.classification == Classification::ProductFailure)
    );
    facts.complete = false;
    assert!(
        oracle
            .evaluate(&facts, false)
            .iter()
            .all(|e| e.classification == Classification::Inconclusive && e.passed.is_none())
    );
}

#[tokio::test]
async fn stop_guard_failure_unknown_outcome_and_cleanup_always_collect_checks() {
    let tmp = tempfile::tempdir().unwrap();
    for case in 0..5 {
        let mut fake = Fake {
            fail_guard: case == 1,
            fail_cleanup: case == 2,
            fail_execute: case == 3,
            incomplete: case == 4,
            ..Default::default()
        };
        let mut selector = Scripted(vec![if case == 3 {
            Action::Inspect
        } else {
            Action::Stop
        }]);
        let mut campaign = Campaign::new(Limits::default()).unwrap();
        let report = controller::run(
            &mut fake,
            Some(&mut selector),
            &mut campaign,
            Episode {
                mode: Mode::Calibration,
                identity: identity(),
                replay: None,
                output: &tmp.path().join(format!("case-{case}")),
                cancel: &AtomicBool::new(false),
            },
        )
        .await
        .unwrap();
        assert!(fake.observed >= 1);
        assert!(!report.evaluations.is_empty());
        if case == 1 {
            assert_eq!(fake.dispatched, 0);
        }
        if case == 2 {
            assert!(report.cleanup.starts_with("failed"));
        }
        if case == 3 {
            assert_eq!(report.operations, vec![Action::Inspect]);
            assert!(report.stop_reason.contains("unknown execution outcome"));
        }
        if case == 4 {
            assert!(report.evaluations.iter().any(|e| e.passed.is_none()));
        }
    }
}

#[tokio::test]
async fn concrete_replay_and_bounded_reduction_preserve_negative_control() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let mut selector = Scripted(selector::calibration());
    let mut campaign = Campaign::new(Limits::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let original = controller::run(
        &mut fake,
        Some(&mut selector),
        &mut campaign,
        Episode {
            mode: Mode::Calibration,
            identity: identity(),
            replay: None,
            output: &tmp.path().join("original"),
            cancel: &cancel,
        },
    )
    .await
    .unwrap();
    assert!(original.reset_verified);
    assert_eq!(original.cleanup, "removed");
    assert_eq!(original.signatures().len(), 1);
    let replay = Replay::load(&tmp.path().join("original/replay.json")).unwrap();
    let repeated = controller::run(
        &mut fake,
        None,
        &mut campaign,
        Episode {
            mode: Mode::Replay,
            identity: identity(),
            replay: Some(&replay),
            output: &tmp.path().join("replay"),
            cancel: &cancel,
        },
    )
    .await
    .unwrap();
    assert_eq!(repeated.signatures(), original.signatures());
    assert_eq!(repeated.selector, "none-recorded-operations");
    let reduced = reducer::reduce(
        &mut fake,
        &mut campaign,
        &replay,
        &tmp.path().join("reduction"),
        &cancel,
    )
    .await
    .unwrap();
    assert!(reduced.attempts.len() <= 6);
    assert!(campaign.attempted <= 64);
    assert_eq!(
        reduced.reduced.failure_signatures,
        replay.failure_signatures
    );
    assert!(reduced.reduced.operations.len() < replay.operations.len());
    if let Ok(destination) = std::env::var("CONARY_EXPLORER_TEST_EVIDENCE") {
        // Explicit opt-in test artifact export; always labelled as a fake environment.
        let destination = std::path::Path::new(&destination);
        std::fs::create_dir(destination).unwrap();
        fn copy(from: &std::path::Path, to: &std::path::Path) {
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let path = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    std::fs::create_dir(&path).unwrap();
                    copy(&entry.path(), &path);
                } else {
                    std::fs::copy(entry.path(), path).unwrap();
                }
            }
        }
        copy(tmp.path(), destination);
        std::fs::write(destination.join("MOCK-ONLY.txt"), "Controller test double. Not real Conary integration, #917 reproduction, or a live Jev trial.\n").unwrap();
    }
}

#[tokio::test]
async fn cancellation_and_action_limits_never_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(true);
    for exhausted in [false, true] {
        let mut fake = Fake::default();
        let mut selected = Seeded::new(0);
        let mut campaign = Campaign::new(Limits::default()).unwrap();
        if exhausted {
            cancel.store(false, Ordering::SeqCst);
            campaign.attempted = 64;
        }
        let report = controller::run(
            &mut fake,
            Some(&mut selected),
            &mut campaign,
            Episode {
                mode: Mode::Exploration,
                identity: identity(),
                replay: None,
                output: &tmp.path().join(format!("{exhausted}")),
                cancel: &cancel,
            },
        )
        .await
        .unwrap();
        assert_eq!(fake.dispatched, 0);
        assert!(fake.observed > 0);
        assert_eq!(report.cleanup, "removed");
    }
}

#[test]
fn guest_registration_and_evidence_fail_closed() {
    let value = sandbox::GuestApproval {
        version: 1,
        approved_disposable: true,
        boot_id: "00000000-0000-0000-0000-000000000000".into(),
        source_revision: "a".repeat(40),
        conary_sha256: "b".repeat(64),
        image: format!("sha256:{}", "c".repeat(64)),
    };
    assert!(value.validate().is_ok());
    assert!(value.verify_here().is_err());
    assert!(
        sandbox::GuestApproval {
            image: "latest".into(),
            ..value
        }
        .validate()
        .is_err()
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut sink = evidence::Evidence::create(&tmp.path().join("out"), 65536).unwrap();
    assert!(sink.event("too_large", &"x".repeat(65536)).is_err());
    assert!(evidence::Evidence::create(&tmp.path().join("out"), 65536).is_err());
}

#[test]
fn fixture_builder_produces_pinned_verified_current_packages() {
    let tmp = tempfile::tempdir().unwrap();
    let directory = tmp.path().join("fixtures");
    let hashes = fixtures::build(&directory).unwrap();
    assert_eq!(hashes.len(), 4);
    let policy = conary_core::ccs::TrustPolicy::from_file(&directory.join("policy.toml")).unwrap();
    for name in ["app-v1.ccs", "app-v2.ccs", "companion.ccs"] {
        let verified =
            conary_core::ccs::verify::verify_package(&directory.join(name), &policy).unwrap();
        assert!(verified.files_checked() >= 1);
    }
    std::fs::write(directory.join("app-v1.ccs"), b"tampered").unwrap();
    assert_ne!(hashes, fixtures::hashes(&directory).unwrap());
}
