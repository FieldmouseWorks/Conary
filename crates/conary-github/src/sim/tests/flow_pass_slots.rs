//! One admission view is shared by full passes, dry runs and targeted kicks.
use super::flow_pass_case::{Case, REPO, example};
use super::head;
use crate::api::Source;
use crate::flow::*;
use serde_json::json;

fn two(slots: u32) -> Case {
    let mut c = Case::new();
    c.config.settings.proof_slots = Some(slots);
    c.config
        .settings
        .proofs
        .insert("machine".into(), Recipe::Run(vec!["check".into()]));
    c.config.plugins.proof = Some(example());
    c.clean_review();
    c.hub.pr(REPO, 5, &head(5));
    c.hub.edit_pull(REPO, 5, |p| {
        p["user"]["login"] = json!("owner");
        p["created_at"] = json!("2099-01-01T00:00:00Z");
    });
    let mut snapshot = c.hub.snapshot(REPO, 3).unwrap();
    snapshot.revision = Revision::from(&c.hub.pull(REPO, 5).unwrap());
    c.hub.flow_snapshot(REPO, 5, snapshot);
    c.good_ci(&head(5));
    c.hub.flow_review(REPO, 5, "reviewer", Verdict::Clean, 0);
    let r = c.hub.reviews(REPO, 5).unwrap().pop().unwrap();
    c.sign_check(
        &head(5),
        "conary/review/reviewer",
        Some(&review_receipt(&head(5), r.body.as_deref().unwrap())),
    );
    for n in [3, 5] {
        c.hub
            .post_status(&head(n), json!({"context":"machine","state":"pending"}));
    }
    c
}
fn kick(c: &Case, pr: u64, dry: bool, now: u64) -> Vec<Report> {
    Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now,
        dry_run: dry,
        ledger: None,
    }
    .run(&c.hub, &REPO.parse().unwrap(), Some(pr), None)
    .unwrap()
}
#[test]
fn full_and_dry_passes_enforce_repository_proof_slots() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    let dry = c.pass(now, true);
    assert_eq!(dry[0].next.as_ref().unwrap().step, Step::Prove);
    assert_eq!(dry[1].next.as_ref().unwrap().stage, Stage::ProofSlot);
    let real = c.pass(now, false);
    assert_eq!(
        real[0].next.as_ref().unwrap().step,
        dry[0].next.as_ref().unwrap().step
    );
    assert_eq!(
        real[1].next.as_ref().unwrap().stage,
        dry[1].next.as_ref().unwrap().stage
    );
    assert_eq!(c.deferred.0.borrow().len(), 1);
}
#[test]
fn targeted_kicks_count_workers_on_excluded_prs() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    c.pass(now, false);
    for dry in [true, false] {
        let report = kick(&c, 5, dry, now + 1);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::ProofSlot);
    }
    assert_eq!(c.deferred.0.borrow().len(), 1);
}
#[test]
fn a_live_holder_is_not_counted_again_as_a_queued_waiter() {
    let c = two(2);
    c.pass(crate::flow::jobs::now(), false);
    assert_eq!(c.deferred.0.borrow().len(), 2);
}
#[test]
fn first_priority_is_used_before_admission_but_output_is_stable() {
    let c = two(1);
    c.hub.label(REPO, 5, &["first"]);
    let report = c.pass(crate::flow::jobs::now(), true);
    assert_eq!(report.iter().map(|r| r.pr).collect::<Vec<_>>(), vec![3, 5]);
    assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::ProofSlot);
    assert_eq!(report[1].next.as_ref().unwrap().step, Step::Prove);
}
#[test]
fn targeted_kicks_preserve_an_earlier_queued_prs_priority() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    let dry = c.pass(now, true);
    let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
    let mut record = Record {
        decision: dry[0].next.clone(),
        ..Record::default()
    };
    record.memory.head = head(3);
    data.save(3, &record).unwrap();
    drop(data);
    let report = kick(&c, 5, true, now);
    assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::ProofSlot);
    assert!(c.deferred.0.borrow().is_empty());
}

#[test]
fn a_cancellation_request_keeps_its_slot_until_the_worker_answers() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    c.pass(now, false);
    let record = c.record();
    let work = record
        .jobs
        .iter()
        .find(|w| matches!(w.purpose, crate::flow::jobs::Purpose::Proof))
        .unwrap();
    std::fs::write(work.path.with_extension("cancel"), b"obsolete\n").unwrap();
    let report = kick(&c, 5, true, now + 1);
    assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::ProofSlot);
}

#[test]
fn all_full_proof_workers_take_a_slot_including_build_evaluation() {
    for recipe in [
        Recipe::Build(".#check".into()),
        Recipe::Run(vec!["check".into()]),
        Recipe::Job("proof".into()),
    ] {
        let mut c = two(1);
        c.config.settings.proofs.insert("machine".into(), recipe);
        let now = crate::flow::jobs::now();
        for dry in [true, false] {
            let report = c.pass(now, dry);
            assert_eq!(report[0].next.as_ref().unwrap().step, Step::Prove);
            assert_eq!(report[1].next.as_ref().unwrap().stage, Stage::ProofSlot);
        }
        assert_eq!(c.deferred.0.borrow().len(), 1);
        assert_eq!(
            kick(&c, 5, true, now + 1)[0].next.as_ref().unwrap().stage,
            Stage::ProofSlot
        );
    }
}
#[test]
fn a_first_targeted_kick_preserves_the_priority_of_unvisited_prs() {
    let c = two(1);
    assert_eq!(
        kick(&c, 5, true, crate::flow::jobs::now())[0]
            .next
            .as_ref()
            .unwrap()
            .stage,
        Stage::ProofSlot
    );
}

#[test]
fn targeted_kicks_do_not_reserve_slots_for_a_stale_queued_decision() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
    let record = Record {
        decision: c.pass(now, true)[0].next.clone(),
        ..Record::default()
    };
    data.save(3, &record).unwrap();
    drop(data);
    c.hub
        .post_status(&head(3), json!({"context":"machine","state":"success"}));
    let report = kick(&c, 5, true, now + 1);
    assert_eq!(report[0].next.as_ref().unwrap().step, Step::Prove);
}
