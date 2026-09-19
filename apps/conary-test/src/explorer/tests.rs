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
        publication: None,
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
    accept_absent_removal: bool,
    refuse_install: bool,
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
            accept_absent_removal: false,
            refuse_install: false,
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
            Action::Install(_) | Action::Update(_) if self.refuse_install => {
                exit_code = 1;
            }
            Action::Install(f) | Action::Update(f) => {
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
                    exit_code = if self.accept_absent_removal { 0 } else { 1 };
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
            assert!(
                report
                    .evaluations
                    .iter()
                    .any(|e| e.classification == Classification::EnvironmentFailure)
            );
        }
        if case == 2 {
            assert!(report.cleanup.starts_with("failed"));
        }
        if case == 3 {
            assert_eq!(report.operations, vec![Action::Inspect]);
            assert!(report.stop_reason.contains("unknown execution outcome"));
            assert!(
                report
                    .evaluations
                    .iter()
                    .any(|e| e.classification == Classification::Inconclusive)
            );
        }
        if case == 4 {
            assert!(report.evaluations.iter().any(|e| e.passed.is_none()));
        }
    }
}

#[tokio::test]
async fn unexplained_nonzero_exit_is_inconclusive_and_cannot_confirm_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let replay = Replay {
        version: VERSION,
        identity: identity(),
        operations: vec![Action::Install(Fixture::AppV1)],
        failure_signatures: vec![],
    };
    let mut fake = Fake {
        refuse_install: true,
        ..Default::default()
    };
    let report = controller::run(
        &mut fake,
        None,
        &mut Campaign::new(Limits::default()).unwrap(),
        Episode {
            mode: Mode::Replay,
            identity: identity(),
            replay: Some(&replay),
            output: &tmp.path().join("refusal"),
            cancel: &AtomicBool::new(false),
        },
    )
    .await
    .unwrap();
    assert_eq!(fake.dispatched, 1);
    assert_eq!(report.reproduces_recorded_predicate, Some(false));
    assert_eq!(report.cleanup, "removed");
    assert!(
        report
            .evaluations
            .iter()
            .any(|e| e.criterion == "operation.refusal_or_success"
                && e.classification == Classification::Inconclusive
                && e.passed.is_none())
    );
    assert!(
        !report
            .evaluations
            .iter()
            .any(|e| e.classification == Classification::ProductFailure)
    );
    let events = std::fs::read_to_string(tmp.path().join("refusal/events.jsonl")).unwrap();
    assert!(events.contains("execution_receipt") && events.contains("required_final_checks"));
}

#[tokio::test]
async fn cancellation_during_selection_still_checks_and_cleans_without_dispatch() {
    struct Cancel<'a>(&'a AtomicBool);
    #[async_trait]
    impl Selector for Cancel<'_> {
        fn identity(&self) -> &'static str {
            "cancel-test"
        }
        async fn select(&mut self, _: &DecisionRequest) -> Result<Decision> {
            self.0.store(true, Ordering::SeqCst);
            anyhow::bail!("selector interrupted")
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let mut fake = Fake::default();
    let report = controller::run(
        &mut fake,
        Some(&mut Cancel(&cancel)),
        &mut Campaign::new(Limits::default()).unwrap(),
        Episode {
            mode: Mode::Exploration,
            identity: identity(),
            replay: None,
            output: &tmp.path().join("cancel"),
            cancel: &cancel,
        },
    )
    .await
    .unwrap();
    assert_eq!(fake.dispatched, 0);
    assert_eq!(report.stop_reason, "cancelled");
    assert_eq!(report.cleanup, "removed");
    assert!(report.reset_verified);
    assert_eq!(report.evaluations.len(), 6);
    assert!(report.evaluations.iter().all(|e| e.passed == Some(true)));
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
    let replay_path = std::env::var_os("CONARY_EXPLORER_REPLAY_INPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| tmp.path().join("original/replay.json"));
    let replay = Replay::load(&replay_path).unwrap();
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
        scratch_uuid: "11111111-1111-1111-1111-111111111111".into(),
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

#[tokio::test]
async fn deadline_and_replay_mismatch_remain_visible_without_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fake = Fake::default();
    let mut selector = Seeded::new(1);
    let mut campaign = Campaign::new(Limits {
        seconds: 1,
        ..Default::default()
    })
    .unwrap();
    let report = controller::run(
        &mut fake,
        Some(&mut selector),
        &mut campaign,
        Episode {
            mode: Mode::Exploration,
            identity: identity(),
            replay: None,
            output: &tmp.path().join("deadline"),
            cancel: &AtomicBool::new(false),
        },
    )
    .await
    .unwrap();
    assert_eq!(fake.dispatched, 0);
    assert!(report.stop_reason.contains("wall-time budget"));
    assert!(report.evaluations.iter().all(|e| e.passed == Some(true)));
    let mut replay = Replay {
        version: VERSION,
        identity: identity(),
        operations: vec![Action::Stop],
        failure_signatures: vec!["not-reproduced".into()],
    };
    let mut campaign = Campaign::new(Limits::default()).unwrap();
    let report = controller::run(
        &mut fake,
        None,
        &mut campaign,
        Episode {
            mode: Mode::Replay,
            identity: identity(),
            replay: Some(&replay),
            output: &tmp.path().join("predicate-mismatch"),
            cancel: &AtomicBool::new(false),
        },
    )
    .await
    .unwrap();
    assert_eq!(report.reproduces_recorded_predicate, Some(false));
    replay.identity.source_revision = "d".repeat(40);
    let report = controller::run(
        &mut fake,
        None,
        &mut campaign,
        Episode {
            mode: Mode::Replay,
            identity: identity(),
            replay: Some(&replay),
            output: &tmp.path().join("identity-mismatch"),
            cancel: &AtomicBool::new(false),
        },
    )
    .await
    .unwrap();
    assert!(!report.reset_verified);
    assert_eq!(fake.dispatched, 0);
}

#[test]
fn saved_fixture_bundle_contains_exact_bytes_and_fails_on_substitution() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("fixtures");
    let hashes = fixtures::build(&source).unwrap();
    let output = tmp.path().join("bundle");
    let mut sink = evidence::Evidence::create(&output, 1024 * 1024).unwrap();
    sink.include_fixtures(&source, &hashes).unwrap();
    assert_eq!(hashes, fixtures::hashes(&output.join("fixtures")).unwrap());
    std::fs::write(source.join("app-v2.ccs"), b"different").unwrap();
    let mut another = evidence::Evidence::create(&tmp.path().join("another"), 1024 * 1024).unwrap();
    assert!(another.include_fixtures(&source, &hashes).is_err());
}

#[tokio::test]
async fn unexpected_acceptance_of_a_required_refusal_is_a_product_failure() {
    let tmp = tempfile::tempdir().unwrap();
    for invalid_acceptance in [false, true] {
        let mut fake = Fake {
            accept_absent_removal: invalid_acceptance,
            ..Default::default()
        };
        let mut selected = Scripted(vec![Action::Remove(Package::App), Action::Stop]);
        let mut campaign = Campaign::new(Limits::default()).unwrap();
        let report = controller::run(
            &mut fake,
            Some(&mut selected),
            &mut campaign,
            Episode {
                mode: Mode::Calibration,
                identity: identity(),
                replay: None,
                output: &tmp.path().join(format!("refusal-{invalid_acceptance}")),
                cancel: &AtomicBool::new(false),
            },
        )
        .await
        .unwrap();
        let outcome = report
            .evaluations
            .iter()
            .find(|e| e.criterion == "operation.refusal_or_success")
            .unwrap();
        assert_eq!(outcome.passed, Some(!invalid_acceptance));
        assert_eq!(
            outcome.classification,
            if invalid_acceptance {
                Classification::ProductFailure
            } else {
                Classification::ExpectedRefusal
            }
        );
    }
}

#[tokio::test]
async fn seeded_controller_reobserves_between_concrete_operations() {
    let tmp = tempfile::tempdir().unwrap();
    let output = std::env::var_os("CONARY_EXPLORER_SEEDED_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| tmp.path().join("seeded"));
    let mut fake = Fake::default();
    let mut selected = Seeded::new(7);
    let mut campaign = Campaign::new(Limits {
        actions: 12,
        ..Default::default()
    })
    .unwrap();
    let report = controller::run(
        &mut fake,
        Some(&mut selected),
        &mut campaign,
        Episode {
            mode: Mode::Exploration,
            identity: identity(),
            replay: None,
            output: &output,
            cancel: &AtomicBool::new(false),
        },
    )
    .await
    .unwrap();
    assert_eq!(report.mode, Mode::Exploration);
    assert_eq!(report.selector_configuration["seed"], 7);
    assert_eq!(report.attempted_actions_total, 12);
    assert!(fake.dispatched > 1);
    let events = std::fs::read_to_string(output.join("events.jsonl")).unwrap();
    let requests = events
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|v| v["event"] == "decision_request")
        .collect::<Vec<_>>();
    assert!(
        requests
            .windows(2)
            .any(|w| w[0]["data"]["candidates"] != w[1]["data"]["candidates"]
                && w[0]["data"]["observation"]["facts"] != w[1]["data"]["observation"]["facts"])
    );
    std::fs::write(
        output.join("MOCK-ONLY.txt"),
        "Seeded controller test double; not real Conary integration or a live model trial.\n",
    )
    .unwrap();
}

#[test]
fn runtime_process_guard_requires_exact_mount_authority_without_privilege_growth() {
    let valid = "CapEff:\t0000000008200006\nCapPrm:\t0000000008200006\nCapBnd:\t0000000008200006\nNoNewPrivs:\t1\nSeccomp:\t2\n";
    assert!(super::sandbox::verify_process_restrictions(valid).is_ok());
    for invalid in [
        valid.replace("8200006", "8200007"),
        valid.replace("8200006", "8200002"),
        valid.replace("8200006", "0200000"),
        valid.replace("8200006", "0000000"),
        valid.replace("NoNewPrivs:\t1", "NoNewPrivs:\t0"),
        valid.replace("Seccomp:\t2", "Seccomp:\t0"),
        String::new(),
    ] {
        assert!(super::sandbox::verify_process_restrictions(&invalid).is_err());
    }
}

#[test]
fn scratch_guard_rejects_wrong_identity_unbounded_storage_and_unsafe_mounts() {
    let valid = serde_json::json!({"filesystems": [{"target": sandbox::SCRATCH_MOUNT,
        "fstype": "ext4", "uuid": "registered", "size": 234594304,
        "options": "rw,nosuid,nodev,relatime"}]});
    let check = |value: &serde_json::Value| {
        sandbox::verify_scratch_mount(&serde_json::to_vec(value).unwrap(), "registered")
    };
    assert!(check(&valid).is_ok());
    for (field, value) in [
        ("fstype", serde_json::json!("tmpfs")),
        ("uuid", serde_json::json!("another")),
        ("target", serde_json::json!("/")),
        ("size", serde_json::json!(1024 * 1024 * 1024)),
        ("options", serde_json::json!("rw,relatime")),
    ] {
        let mut invalid = valid.clone();
        invalid["filesystems"][0][field] = value;
        assert!(check(&invalid).is_err());
    }
}

#[test]
fn selected_state_observer_checks_snapshot_bytes_and_exposes_pending_publication() {
    use conary_core::{db, filesystem::CasStore, generation::root_manifest::*};
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path().join("runtime");
    let db_path = runtime.join("conary.db");
    db::init(&db_path).unwrap();
    let mut facts = empty();
    selected_state::observe(&runtime, &mut facts).unwrap();
    assert!(facts.publication.is_none());
    let root = temp.path().join("fixture-root");
    let payload = root.join(Package::App.path().trim_start_matches('/'));
    std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
    std::fs::write(&payload, Fixture::AppV1.payload()).unwrap();
    let cas = CasStore::new(runtime.join("objects")).unwrap();
    let captured = scan_selected_root(&root, &cas).unwrap();
    let conn = db::open(&db_path).unwrap();
    let snapshot = SelectedRootSnapshot::capture(&conn, &captured).unwrap();
    let publication = db::models::GenerationPublication::create_pending(
        &conn,
        None,
        None,
        db_path.to_str().unwrap(),
        runtime.to_str().unwrap(),
        "unit selected-root fixture",
        &Default::default(),
    )
    .unwrap();
    publication
        .bind_selected_root_snapshot(&conn, snapshot.id())
        .unwrap();
    selected_state::observe(&runtime, &mut facts).unwrap();
    let expected = hex::encode(Sha256::digest(Fixture::AppV1.payload()));
    assert_eq!(facts.payloads[&Package::App], expected);
    assert_eq!(facts.publication.unwrap().status, "pending");
    // An intact database/manifest must not hide changed stored bytes.
    let object = conary_core::filesystem::object_path(&runtime.join("objects"), &expected).unwrap();
    std::fs::write(object, "x".repeat(Fixture::AppV1.payload().len())).unwrap();
    assert!(selected_state::observe(&runtime, &mut empty()).is_err());
}
