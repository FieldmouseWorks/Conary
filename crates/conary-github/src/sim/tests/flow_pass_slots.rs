//! One admission view is shared by full passes, dry runs and targeted kicks.
use super::flow_pass_case::{Case, REPO, example};
use super::head;
use crate::api::Source;
use crate::flow::jobs::Purpose;
use crate::flow::*;
use serde_json::json;

pub(super) fn two(slots: u32) -> Case {
    let mut c = Case::new();
    c.config.settings.proof_slots = Some(slots);
    c.config
        .settings
        .proofs
        .insert("machine".into(), Recipe::Run(vec!["check".into()]));
    c.config.plugins.proof = Some(example());
    c.clean_review();
    c.hub
        .post_status(&head(3), json!({"context":"machine","state":"pending"}));
    another(&c, 5);
    c
}
/// PR `n` beside PR 3, opened at the same time, reviewed clean on a green
/// head, its machine check pending.
pub(super) fn another(c: &Case, n: u64) {
    c.hub.pr(REPO, n, &head(n));
    c.hub.edit_pull(REPO, n, |p| {
        p["user"]["login"] = json!("owner");
        p["created_at"] = json!("2099-01-01T00:00:00Z");
    });
    let mut snapshot = c.hub.snapshot(REPO, 3).unwrap();
    snapshot.revision = Revision::from(&c.hub.pull(REPO, n).unwrap());
    c.hub.flow_snapshot(REPO, n, snapshot);
    c.good_ci(&head(n));
    c.hub.flow_review(REPO, n, Verdict::Clean, 0);
    c.hub
        .post_status(&head(n), json!({"context":"machine","state":"pending"}));
}
pub(super) fn kick(c: &Case, pr: u64, dry: bool, now: u64) -> Vec<Report> {
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
/// Both PRs' preparations launched by a real pass at `now`, and answered
/// ready, needing a slot or not. Nothing else is left launched.
pub(super) fn prepared(c: &Case, now: u64, slot: bool) {
    let reports = c.pass(now, false);
    for report in &reports {
        assert_eq!(report.next.as_ref().unwrap().step, Step::Prepare);
    }
    c.prepared(slot, None);
    assert!(c.deferred.0.borrow().is_empty());
}
pub(super) fn step(reports: &[Report], at: usize) -> (Stage, Step) {
    let next = reports[at].next.as_ref().unwrap();
    (next.stage, next.step.clone())
}
pub(super) fn work(
    c: &Case,
    pr: u64,
    purpose: fn(&Purpose) -> bool,
) -> Vec<crate::flow::jobs::Work> {
    Data::read(c.dir.path(), &REPO.parse().unwrap(), pr)
        .unwrap()
        .jobs
        .into_iter()
        .filter(|w| purpose(&w.purpose))
        .collect()
}
pub(super) const WAITING: (Stage, Step) = (Stage::ProofSlot, Step::Wait { until: None });

#[test]
fn full_and_dry_passes_enforce_repository_proof_slots() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    let dry = c.pass(now + 1, true);
    assert_eq!(step(&dry, 0).1, Step::Prove);
    assert_eq!(step(&dry, 1), WAITING);
    let real = c.pass(now + 1, false);
    assert_eq!(step(&real, 0), step(&dry, 0));
    assert_eq!(step(&real, 1), step(&dry, 1));
    assert_eq!(c.deferred.0.borrow().len(), 1);
}
/// Records a pass saved before proofs were prepared name plans the flow no
/// longer makes. They still load, so neither PR's error reserves the slot.
#[test]
fn decisions_saved_before_preparation_still_load_and_hold_no_capacity() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    let repo = REPO.parse().unwrap();
    let data = Data::open(c.dir.path(), &repo).unwrap();
    for (pr, legacy) in [
        (3, json!({"Run": {"machine": {"Run": ["check"]}}})),
        (5, json!({"Evaluate": {"machine": {"Run": ["check"]}}})),
    ] {
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(data.path(pr)).unwrap()).unwrap();
        value["decision"]["proof"] = legacy;
        std::fs::write(data.path(pr), value.to_string()).unwrap();
    }
    drop(data);
    let real = c.pass(now + 1, false);
    assert!(real.iter().all(|r| r.error.is_none()), "{real:?}");
    assert_eq!(step(&real, 0).1, Step::Prove);
    assert_eq!(step(&real, 1), WAITING);
    assert_eq!(c.deferred.0.borrow().len(), 1);
}
#[test]
fn targeted_kicks_count_workers_on_excluded_prs() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    c.pass(now + 1, false);
    for dry in [true, false] {
        let report = kick(&c, 5, dry, now + 2);
        assert_eq!(report.len(), 1);
        assert_eq!(step(&report, 0), WAITING);
    }
    assert_eq!(c.deferred.0.borrow().len(), 1);
}
#[test]
fn a_live_holder_is_not_counted_again_as_a_queued_waiter() {
    let c = two(2);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    c.pass(now + 1, false);
    assert_eq!(c.deferred.0.borrow().len(), 2);
}
#[test]
fn priority_is_used_before_admission_but_output_is_stable() {
    let c = two(1);
    c.hub.label(REPO, 5, &["priority: high"]);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    let report = c.pass(now + 1, true);
    assert_eq!(report.iter().map(|r| r.pr).collect::<Vec<_>>(), vec![3, 5]);
    assert_eq!(step(&report, 0), WAITING);
    assert_eq!(step(&report, 1).1, Step::Prove);
}
#[test]
fn targeted_kicks_preserve_an_earlier_queued_prs_priority() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    // PR 3 has never been decided on its prepared head: its turn is read
    // from its record before PR 5 is admitted.
    let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
    let mut record = data.load(3).unwrap();
    record.decision = None;
    data.save(3, &record).unwrap();
    drop(data);
    let report = kick(&c, 5, true, now + 1);
    assert_eq!(step(&report, 0), WAITING);
    assert!(c.deferred.0.borrow().is_empty());
}

#[test]
fn a_cancellation_request_keeps_its_slot_until_the_worker_answers() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    c.pass(now + 1, false);
    let proof = work(&c, 3, |p| matches!(p, Purpose::Proof));
    std::fs::write(proof[0].path.with_extension("cancel"), b"obsolete\n").unwrap();
    let report = kick(&c, 5, true, now + 2);
    assert_eq!(step(&report, 0), WAITING);
}

#[test]
fn a_first_targeted_kick_prepares_without_waiting_on_unvisited_prs() {
    // A preparation holds no slot, so no PR's turn comes before it.
    let c = two(1);
    let report = kick(&c, 5, true, crate::flow::jobs::now());
    assert_eq!(step(&report, 0), (Stage::Proof, Step::Prepare));
    assert!(c.deferred.0.borrow().is_empty());
}

#[test]
fn targeted_kicks_do_not_reserve_slots_for_a_stale_queued_decision() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
    let mut record = data.load(3).unwrap();
    record.decision = c.pass(now + 1, true)[0].next.clone();
    assert_eq!(record.decision.as_ref().unwrap().step, Step::Prove);
    data.save(3, &record).unwrap();
    drop(data);
    c.hub
        .post_status(&head(3), json!({"context":"machine","state":"success"}));
    let report = kick(&c, 5, true, now + 2);
    assert_eq!(step(&report, 0).1, Step::Prove);
}

#[test]
fn while_a_prepare_runs_another_prs_proof_takes_the_only_slot() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    let reports = c.pass(now, false);
    assert_eq!(step(&reports, 0), (Stage::Proof, Step::Prepare));
    assert_eq!(step(&reports, 1), (Stage::Proof, Step::Prepare));
    // PR 5's preparation answers first; PR 3's still runs.
    c.prepared(true, Some(5));
    let reports = c.pass(now + 1, false);
    assert_eq!(reports[0].next.as_ref().unwrap().stage, Stage::Proof);
    assert!(matches!(
        reports[0].next.as_ref().unwrap().step,
        Step::Wait { .. }
    ));
    assert_eq!(step(&reports, 1), (Stage::Proof, Step::Prove));
    assert_eq!(work(&c, 5, |p| matches!(p, Purpose::Proof)).len(), 1);
    assert!(work(&c, 3, |p| matches!(p, Purpose::Proof)).is_empty());
    // Then PR 3 is ready and waits its turn behind PR 5's proof.
    c.prepared(true, Some(3));
    for dry in [true, false] {
        assert_eq!(step(&c.pass(now + 2, dry), 0), WAITING);
    }
    assert!(work(&c, 3, |p| matches!(p, Purpose::Proof)).is_empty());
}

#[test]
fn a_prepare_answer_cannot_post_or_count_a_verdict() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    // What GitHub shows for the proof's check: its runs, and its statuses
    // other than the pending one the case starts with.
    let checks = |c: &Case| {
        c.hub.check_runs(REPO, &head(3)).unwrap().len()
            + c.hub
                .statuses(REPO, &head(3))
                .unwrap()
                .iter()
                .filter(|s| s.context == "machine" && s.state != "pending")
                .count()
    };
    let before = checks(&c);
    c.pass(now, false);
    c.prepared(true, Some(3));
    let report = c.pass(now + 1, false);
    // Readiness only: the proof still has to run and its check is still
    // pending. Nothing went up for it, and nothing grants landing.
    assert_eq!(step(&report, 0).1, Step::Prove);
    assert_eq!(checks(&c), before);
    assert_eq!(c.hub.count(crate::sim::Call::Land), 0);
    // Even an answer shaped like a passing proof is no verdict: it answers
    // another kind of work, so it doesn't read, and the PR waits on it,
    // naming it, for as long as it lasts.
    let c = two(1);
    c.pass(now, false);
    let prepare = work(&c, 3, |p| matches!(p, Purpose::Prepare)).remove(0);
    let completion = crate::flow::jobs::Completion {
        at: now,
        result: Some(Ok(conary_core::plugin::Response {
            contract: conary_core::plugin::CONTRACT.into(),
            by: conary_core::plugin::Identity {
                engine: "fixture".into(),
                session: None,
            },
            execution: None,
            answer: conary_core::plugin::Answer::Proof {
                head: Some(head(3)),
                results: vec![conary_core::plugin::CheckResult {
                    check: "machine".into(),
                    verdict: conary_core::plugin::Verdict::Pass,
                    why: "forged".into(),
                }],
            },
        })),
    };
    std::fs::write(
        prepare.path.with_extension("answer"),
        serde_json::to_vec(&completion).unwrap(),
    )
    .unwrap();
    for at in [now + 1, now + 900] {
        let next = c.pass(at, false)[0].next.clone().unwrap();
        assert_eq!(next.step, Step::Wait { until: None });
        assert_eq!(next.stage, Stage::None);
        assert_eq!(
            next.degraded.iter().map(|d| &d.what).collect::<Vec<_>>(),
            [&crate::flow::Degraded::Answer {
                job: prepare.job.id.clone(),
                kind: conary_core::plugin::Kind::Prepare,
            }]
        );
        assert_eq!(checks(&c), before);
        assert!(work(&c, 3, |p| matches!(p, Purpose::Proof)).is_empty());
    }
}

#[test]
fn a_changed_head_or_recipe_cancels_its_prepare() {
    for change in ["head", "recipe"] {
        let mut c = two(1);
        let now = crate::flow::jobs::now();
        c.pass(now, false);
        let prepare = work(&c, 3, |p| matches!(p, Purpose::Prepare)).remove(0);
        match change {
            "head" => {
                c.hub.push(REPO, 3, &head(4));
                c.hub.refresh_flow(REPO, 3);
            }
            _ => {
                c.config
                    .settings
                    .proofs
                    .insert("machine".into(), Recipe::Run(vec!["check again".into()]));
            }
        }
        let report = c.pass(now + 1, false);
        assert_eq!(
            report[0].next.as_ref().unwrap().step,
            Step::StopJob(prepare.job.id.clone()),
            "{change}"
        );
        assert_eq!(
            std::fs::read(prepare.path.with_extension("cancel")).unwrap(),
            b"obsolete\n"
        );
    }
}

#[test]
fn a_job_recipe_starts_no_proof_worker() {
    let mut c = two(1);
    c.config
        .settings
        .proofs
        .insert("machine".into(), Recipe::Job("exercise the change".into()));
    let now = crate::flow::jobs::now();
    for (at, dry) in [(now, true), (now, false), (now + 1, false)] {
        for report in c.pass(at, dry) {
            let next = report.next.unwrap();
            assert_eq!(next.step, Step::Wait { until: None });
            assert!(next.description.contains("model-run check, not a proof"));
        }
    }
    assert!(c.deferred.0.borrow().is_empty());
    for pr in [3, 5] {
        assert!(work(&c, pr, |_| true).is_empty());
    }
}

#[test]
fn dry_runs_reserve_virtual_slots_only_for_proof() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    // Preparing holds no slot, dry or real: both PRs prepare at once.
    let dry = c.pass(now, true);
    assert_eq!(step(&dry, 0).1, Step::Prepare);
    assert_eq!(step(&dry, 1).1, Step::Prepare);
    // A proof needing a slot reserves one, virtually, in a dry run.
    prepared(&c, now, true);
    let dry = c.pass(now + 1, true);
    assert_eq!(step(&dry, 0).1, Step::Prove);
    assert_eq!(step(&dry, 1), WAITING);
    // A preparation decided first in a dry run leaves the only slot to the
    // proof after it.
    let c = two(1);
    c.pass(now, false);
    c.prepared(true, Some(5));
    let failed = work(&c, 3, |p| matches!(p, Purpose::Prepare)).remove(0);
    let completion = crate::flow::jobs::Completion {
        at: now,
        result: Some(Err("the cache was unreachable".into())),
    };
    std::fs::write(
        failed.path.with_extension("answer"),
        serde_json::to_vec(&completion).unwrap(),
    )
    .unwrap();
    let dry = c.pass(now + 900, true);
    assert_eq!(step(&dry, 0), (Stage::Proof, Step::Prepare));
    assert_eq!(step(&dry, 1), (Stage::Proof, Step::Prove));
    // One its preparation says needs none reserves nothing.
    let c = two(1);
    prepared(&c, now, false);
    for dry in [true, false] {
        let reports = c.pass(now + 1, dry);
        assert_eq!(step(&reports, 0), (Stage::Proof, Step::Prove));
        assert_eq!(step(&reports, 1), (Stage::Proof, Step::Prove));
    }
    for pr in [3, 5] {
        let proofs = work(&c, pr, |p| matches!(p, Purpose::Proof));
        assert_eq!(proofs.len(), 1);
        assert!(proofs[0].slotless);
    }
}
