//! Exercise the real executor and runner against Hub over successive passes.
use super::{TRUNK, head};
use crate::api::Source;
use crate::flow::{self, *};
use crate::sim::{Call, Hub};
use serde_json::json;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

const REPO: &str = "conary-test/flow-simulation";
struct Deferred(RefCell<Vec<PathBuf>>);
impl flow::jobs::Launch for Deferred {
    fn launch(
        &self,
        path: &Path,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let invocation: flow::jobs::Invocation = serde_json::from_slice(&std::fs::read(path)?)?;
        if let conary_core::plugin::Task::Review { pr, head, .. } = invocation.task {
            let record: flow::Record = serde_json::from_slice(&std::fs::read(
                path.parent().unwrap().join(format!("{pr}.json")),
            )?)?;
            assert_eq!(
                record.memory.head, head,
                "worker must see the Memory from this decision"
            );
        }
        self.0.borrow_mut().push(path.into());
        Ok(())
    }
}
fn example() -> PathBuf {
    let executable = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("conary-flow-test-plugin");
    assert!(
        executable.is_file(),
        "Cargo must build the shared example fixture"
    );
    executable
}

struct Case {
    hub: Hub,
    dir: tempfile::TempDir,
    config: flow::Config,
    policy: conary_core::Policy,
    deferred: Deferred,
    store: RefCell<conary_core::Store>,
    _ledger: tempfile::TempDir,
}
impl Case {
    fn new() -> Self {
        let hub = Hub::new(REPO, TRUNK);
        hub.pr(REPO, 3, &head(3));
        hub.edit_pull(REPO, 3, |p| {
            p["user"]["login"] = json!("owner");
            p["created_at"] = json!("2099-01-01T00:00:00Z");
        });
        hub.ci(&head(3), &[("check", "success")]);
        hub.flow_snapshot(
            REPO,
            3,
            Snapshot {
                revision: Revision::from(&hub.pull(REPO, 3).unwrap()),
                default_branch: None,
                draft: false,
                complete: true,
                merge_state: "CLEAN".into(),
                open_threads: 0,
                threads: Default::default(),
                resolved_reviews: Default::default(),
                updated_at: 0,
                suites: Default::default(),
                checks_complete: true,
                rebase_removed: vec![],
                timeline_cut_at: None,
            },
        );
        let mut config = flow::Config {
            trusted_authors: vec!["owner".into()],
            ..Default::default()
        };
        config.negative = Some(Negative {
            check: "check / negative".into(),
            signer: crate::attest::testing::SIGNER.into(),
            fresh: Some("fresh-remi-".into()),
        });
        negative_evidence(&hub, &head(3), "none");
        config.settings.settle_delay = 0;
        config.settings.handoff_after = 0;
        config.plugins.review = Some(example());
        config.plugins.fix = Some(example());
        let dir = tempfile::tempdir().unwrap();
        let ledger = tempfile::tempdir().unwrap();
        let store = RefCell::new(
            conary_core::Store::open(
                conary_core::log::Lock::take(ledger.path()).unwrap(),
                ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
                None,
            )
            .unwrap(),
        );
        Self {
            hub,
            dir,
            store,
            _ledger: ledger,
            config,
            policy: conary_core::Policy::parse(b"{}").unwrap(),
            deferred: Deferred(RefCell::new(vec![])),
        }
    }
    fn pass(&self, now: u64, dry: bool) -> Vec<flow::Report> {
        Pass {
            config: &self.config,
            policy: &self.policy,
            root: self.dir.path(),
            launcher: &self.deferred,
            now,
            dry_run: dry,
            ledger: None,
        }
        .run(
            &self.hub,
            &REPO.parse().unwrap(),
            None,
            Some(&mut self.store.borrow_mut()),
        )
        .unwrap()
    }
    fn run_worker(&self) {
        for path in self.deferred.0.borrow_mut().drain(..) {
            flow::jobs::worker(&path).unwrap();
            let invocation: flow::jobs::Invocation =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let answer: flow::jobs::Completion =
                serde_json::from_slice(&std::fs::read(path.with_extension("answer")).unwrap())
                    .unwrap();
            if let conary_core::plugin::Task::Fix { .. } = invocation.task
                && let Some(Ok(conary_core::plugin::Response {
                    answer:
                        conary_core::plugin::Answer::Fix {
                            result: conary_core::plugin::FixResult::Pushed { head },
                        },
                    ..
                })) = answer.result
            {
                self.hub.push(REPO, 3, &head);
                self.hub.refresh_flow(REPO, 3);
                self.hub.ci(&head, &[("check", "success")]);
                negative_evidence(&self.hub, &head, "none");
            }
        }
    }
    fn record(&self) -> flow::Record {
        Data::read(self.dir.path(), &REPO.parse().unwrap(), 3).unwrap()
    }
}
fn lifecycle(train: bool) {
    let mut c = Case::new();
    c.config.settings.train = train;
    let ledger = tempfile::tempdir().unwrap();
    let mut store = Some({
        conary_core::Store::open(
            conary_core::log::Lock::take(ledger.path()).unwrap(),
            ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
            None,
        )
        .unwrap()
    });
    let mut run = |now| {
        Pass {
            config: &c.config,
            policy: &c.policy,
            root: c.dir.path(),
            launcher: &c.deferred,
            now,
            dry_run: false,
            ledger: None,
        }
        .run(&c.hub, &REPO.parse().unwrap(), None, store.as_mut())
        .unwrap()
    };
    let now = flow::jobs::now();
    assert_eq!(
        run(now)[0].next.as_ref().unwrap().step,
        Step::QueueReview("reviewer".into())
    );
    assert_eq!(c.hub.count(Call::Land), 0);
    c.run_worker();
    assert_eq!(
        run(now + 1)[0].next.as_ref().unwrap().step,
        Step::PostReview("reviewer".into())
    );
    assert_eq!(
        run(now + 2)[0].next.as_ref().unwrap().step,
        Step::HandFix(Kind::Findings)
    );
    assert_eq!(c.hub.pull(REPO, 3).unwrap().head.sha, head(3));
    assert!(matches!(
        run(now + 3)[0].next.as_ref().unwrap().step,
        Step::Wait { .. }
    ));
    c.run_worker();
    assert_eq!(c.hub.pull(REPO, 3).unwrap().head.sha, head(4));
    run(now + 4);
    c.run_worker();
    run(now + 5);
    let result = run(now + 6);
    assert_eq!(result[0].error, None);
    assert_eq!(
        result[0].next.as_ref().unwrap().step,
        if train { Step::JoinTrain } else { Step::Merge }
    );
    assert_eq!(c.hub.count(Call::Land), if train { 0 } else { 1 });
    assert_eq!(c.hub.pull(REPO, 3).unwrap().is_merged(), !train);
    if train {
        assert_eq!(
            store
                .as_ref()
                .unwrap()
                .ledger()
                .train(&REPO.parse().unwrap())
                .unwrap()
                .joined()
                .len(),
            1
        );
    }
}

#[test]
fn pass_lifecycle_collects_a_fix_after_its_starting_pass() {
    lifecycle(false);
}
#[test]
fn pass_lifecycle_joins_a_train_after_review_fix_and_green_ci() {
    lifecycle(true);
}

#[test]
fn pass_uses_current_memory_and_keeps_composite_keys() {
    let c = Case::new();
    let now = flow::jobs::now();
    c.pass(now, false);
    assert_eq!(c.record().memory.head, head(3));
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.pass(now + 1, false);
    assert_eq!(c.record().memory.head, head(4));
    assert_eq!(c.record().memory.seen, now + 1);
}
#[test]
fn pass_dry_run_writes_nothing_and_prints_one_complete_line_per_pr() {
    let c = Case::new();
    let results = c.pass(flow::jobs::now(), true);
    let output = results
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(output.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    for field in ["stage", "step", "description", "overdue_at"] {
        assert!(value.get(field).is_some(), "{field}");
    }
    for call in [
        Call::FlowStatus,
        Call::FlowStage,
        Call::FlowReview,
        Call::Land,
        Call::Mark,
        Call::FlowOwner,
    ] {
        assert_eq!(c.hub.count(call), 0, "{call:?}");
    }
    assert_eq!(std::fs::read_dir(c.dir.path()).unwrap().count(), 0);
}
#[test]
fn pass_without_plugin_waits_with_reason() {
    let mut c = Case::new();
    c.config.plugins.review = None;
    let report = c.pass(flow::jobs::now(), false);
    let next = report[0].next.as_ref().unwrap();
    assert_eq!(next.step, Step::Wait { until: None });
    assert_eq!(next.description, "No plugin configured for review");
    assert!(c.record().jobs.is_empty());
}
#[test]
fn pass_refused_settled_write_never_merges_and_third_strike_is_overdue() {
    let c = Case::new();
    c.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 0);
    c.hub.fail(Call::FlowStatus);
    let now = flow::jobs::now();
    c.pass(now, false);
    c.pass(now + 1, false);
    c.pass(now + 1000, false);
    c.pass(now + 1001, false);
    c.pass(now + 2000, false);
    let result = c.pass(now + 2001, false);
    assert!(result[0].next.as_ref().unwrap().overdue_at.unwrap() <= now + 2001);
    assert_eq!(c.hub.count(Call::Land), 0);
    c.hub.heal(Call::FlowStatus);
    c.pass(now + 10000, false);
    // The successful revocation clears the failed Retry on this pass.
    c.pass(now + 10001, false);
    assert_eq!(c.hub.count(Call::Land), 1);
}
#[test]
fn pass_refuses_the_heads_own_plugin_as_reviewer() {
    let c = Case::new();
    c.hub.commit_details(
        &head(3),
        Some("change: fixture\n\nEngine: conary-example\nSession: forged-independent-session\n"),
        Some("owner@example.org"),
        Some("owner@example.org"),
    );
    let now = flow::jobs::now();
    c.pass(now, false);
    c.run_worker();
    let record = c.record();
    let answer: flow::jobs::Completion = serde_json::from_slice(
        &std::fs::read(record.jobs[0].path.with_extension("answer")).unwrap(),
    )
    .unwrap();
    assert!(
        answer
            .result
            .unwrap()
            .unwrap_err()
            .contains("author cannot grade")
    );
    c.pass(now + 1, false);
    assert_eq!(c.hub.count(Call::FlowReview), 0);
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn pass_merges_only_after_observing_settled_permission() {
    let c = Case::new();
    c.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 0);
    flow::Write::status(&c.hub, REPO, &head(3), false, "unsettled").unwrap();
    // The write succeeds, but GitHub's status endpoint has not caught up.
    c.hub.stale(Call::Statuses);
    let result = c.pass(flow::jobs::now(), false);
    assert!(result[0].error.as_ref().unwrap().contains("permission"));
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn pass_train_joins_through_the_existing_train_module() {
    let mut c = Case::new();
    c.config.settings.train = true;
    c.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 0);
    let ledger = tempfile::tempdir().unwrap();
    let lock = conary_core::log::Lock::take(ledger.path()).unwrap();
    let mut store =
        conary_core::Store::open(lock, ed25519_dalek::SigningKey::from_bytes(&[5; 32]), None)
            .unwrap();
    let now = flow::jobs::now();
    let pass = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now,
        dry_run: false,
        ledger: None,
    };
    let results = pass
        .run(&c.hub, &REPO.parse().unwrap(), None, Some(&mut store))
        .unwrap();
    assert_eq!(results[0].next.as_ref().unwrap().step, Step::JoinTrain);
    assert_eq!(results[0].error, None);
    assert_eq!(
        store
            .ledger()
            .train(&REPO.parse().unwrap())
            .unwrap()
            .joined()
            .len(),
        1
    );
    let results = pass
        .run(&c.hub, &REPO.parse().unwrap(), None, Some(&mut store))
        .unwrap();
    assert_eq!(
        results[0].next.as_ref().unwrap().step,
        Step::Wait { until: None }
    );
    assert_eq!(results[0].error, None);
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn pass_isolates_one_prs_read_failure() {
    let c = Case::new();
    c.hub.pr(REPO, 5, &head(5));
    c.hub.fail(Call::FlowSnapshot.pr(5));
    let results = c.pass(flow::jobs::now(), true);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].error, None);
    assert!(results[1].error.is_some());
}

#[test]
fn pass_rejects_a_revision_that_moves_before_execution() {
    let c = Case::new();
    c.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 0);
    c.hub.race(Call::FlowStage, |hub| {
        hub.push(REPO, 3, &head(4));
        hub.refresh_flow(REPO, 3);
    });
    c.pass(flow::jobs::now(), false);
    // Even a push after the revision check is caught by the merge's expected head.
    assert!(!c.hub.pull(REPO, 3).unwrap().is_merged());
}

#[test]
fn pass_worker_has_a_deadline_even_when_it_was_never_launched() {
    let mut c = Case::new();
    c.config.settings.job_timeout = 1;
    let now = flow::jobs::now();
    c.pass(now, false);
    let results = c.pass(now + 2, false);
    assert_eq!(c.hub.count(Call::FlowReview), 0);
    assert!(matches!(
        results[0].next.as_ref().unwrap().step,
        Step::Wait { .. }
    ));
    assert!(c.record().jobs[0].path.with_extension("cancel").exists());
}

#[test]
fn pass_collects_an_independent_judgment() {
    let mut c = Case::new();
    c.config.plugins.judge = Some(example());
    c.config.settings.judge_engines = ["fixture-author".into()].into();
    c.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 0);
    c.hub.commit_details(
        &head(3),
        Some("change: fixture\n\nEngine: fixture-author\nSession: author\n"),
        Some("owner@example.org"),
        Some("owner@example.org"),
    );
    let now = flow::jobs::now();
    assert_eq!(
        c.pass(now, false)[0].next.as_ref().unwrap().step,
        Step::Judge
    );
    c.run_worker();
    let report = c.pass(now + 1, false);
    assert_eq!(report[0].error, None);
    assert_eq!(report[0].next.as_ref().unwrap().step, Step::Merge);
}

#[test]
fn pass_proof_names_the_tree_and_canned_output_never_grants_permission() {
    let mut c = Case::new();
    c.config.plugins.proof = Some(example());
    c.config
        .settings
        .proofs
        .insert("machine".into(), Recipe::Run(vec!["check".into()]));
    c.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 0);
    c.hub
        .post_status(&head(3), json!({"context":"machine","state":"pending"}));
    let now = flow::jobs::now();
    let report = c.pass(now, false);
    assert_eq!(report[0].next.as_ref().unwrap().step, Step::Prove);
    let record = c.record();
    let invocation: flow::jobs::Invocation =
        serde_json::from_slice(&std::fs::read(&record.jobs[0].path).unwrap()).unwrap();
    let tree = crate::branches::Branches::tree(&c.hub, REPO, &head(3)).unwrap();
    assert!(matches!(invocation.task,conary_core::plugin::Task::Proof{tree: ref t,..} if t==&tree));
    c.run_worker();
    let report = c.pass(now + 1, false);
    assert_eq!(
        report[0].next.as_ref().unwrap().step,
        Step::Wait { until: None }
    );
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn pass_memory_roundtrips_nonempty_composite_keys() {
    let c = Case::new();
    let repo = REPO.parse().unwrap();
    let data = Data::open(c.dir.path(), &repo).unwrap();
    let mut record = Record::default();
    let revision = Revision::from(&c.hub.pull(REPO, 3).unwrap());
    record
        .memory
        .judge_issuance
        .insert(revision.clone(), (10, Some(Judgment::Land)));
    record
        .memory
        .review_posts
        .insert(("reviewer".into(), revision.clone(), 20), 2);
    record
        .memory
        .unanswered
        .insert((Kind::Findings, revision), ["thread".into()].into());
    data.save(3, &record).unwrap();
    assert_eq!(data.load(3).unwrap().memory, record.memory);
}

#[test]
fn pass_releases_the_repository_lock_with_an_inherited_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let repo = REPO.parse().unwrap();
    let data = Data::open(dir.path(), &repo).unwrap();
    let inherited = data.duplicate_lock();
    drop(data);
    // A forked child can temporarily retain this same open-file description.
    // Completing one pass must release the lock before the next pass starts.
    let next = Data::open(dir.path(), &repo).unwrap();
    drop(inherited);
    drop(next);
}

#[test]
fn pass_reconciles_a_review_post_whose_answer_was_lost() {
    let c = Case::new();
    let now = flow::jobs::now();
    c.pass(now, false);
    c.run_worker();
    c.hub.lose(Call::FlowReview);
    assert!(c.pass(now + 1, false)[0].error.is_some());
    assert_eq!(c.hub.count(Call::FlowReview), 1);
    c.hub.heal(Call::FlowReview);
    let report = c.pass(now + 2, false);
    assert_eq!(report[0].error, None);
    assert_eq!(
        c.hub.count(Call::FlowReview),
        1,
        "the same producer answer is posted once"
    );
    assert!(c.record().jobs[0].posted);
}

mod safety;

fn negative_evidence(hub: &Hub, head: &str, result: &str) {
    use crate::attest::testing;
    let run = if head == super::head(3) { 703 } else { 704 };
    hub.put_checks(
        head,
        json!([{
            "id":run, "name":"check / negative", "status":"completed", "conclusion":"success",
            "html_url":format!("https://github.com/{REPO}/actions/runs/9/job/{run}"),
            "started_at":"2026-11-05T05:00:00Z"
        }]),
    );
    let statement = testing::negative_statement(TRUNK, head, result);
    let claims = testing::claims(REPO, head, run, &statement);
    hub.attest(run, &testing::file(&statement, &testing::token(&claims)));
}
