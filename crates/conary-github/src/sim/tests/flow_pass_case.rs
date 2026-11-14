//! Exercise the real executor and runner against Hub over successive passes.
use super::{TRUNK, head};
use crate::api::Source;
use crate::attest::testing;
use crate::flow::{self, *};
use crate::sim::FRESH;
use crate::sim::Hub;
use serde_json::json;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

pub(super) const REPO: &str = "conary-test/flow-simulation";
pub(super) struct Deferred(pub(super) RefCell<Vec<PathBuf>>);
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
pub(super) fn example() -> PathBuf {
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

pub(super) struct Case {
    pub(super) hub: Hub,
    pub(super) store: RefCell<conary_core::Store>,
    _ledger: tempfile::TempDir,
    pub(super) dir: tempfile::TempDir,
    pub(super) config: flow::Config,
    pub(super) policy: conary_core::Policy,
    pub(super) deferred: Deferred,
}
impl Case {
    pub(super) fn new() -> Self {
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
            settings: crate::sim::settings(),
            ..Default::default()
        };
        config.settings.people.insert("maintainer".into());
        config.settings.settle_delay = 0;
        config.settings.handoff_after = 0;
        config.plugins.review = Some(example());
        config.plugins.fix = Some(example());
        config.evidence = Some(Evidence {
            signer: testing::SIGNER.into(),
            fresh: Some(FRESH.into()),
            require: vec!["check".into()],
            negative: "negative".into(),
            producers: None,
        });
        config.settings.judge_key = Some(SealKey::from_hex(&"a".repeat(64)).unwrap());
        config.settings.verdict_authors.insert("producer".into());
        let ledger = tempfile::tempdir().unwrap();
        let store = conary_core::Store::open(
            conary_core::log::Lock::take(ledger.path()).unwrap(),
            ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
            None,
        )
        .unwrap();
        let case = Self {
            store: RefCell::new(store),
            _ledger: ledger,
            hub,
            dir: tempfile::tempdir().unwrap(),
            config,
            policy: conary_core::Policy::parse(b"{}").unwrap(),
            deferred: Deferred(RefCell::new(vec![])),
        };
        case.good_ci(&head(3));
        case
    }
    pub(super) fn pass(&self, now: u64, dry: bool) -> Vec<flow::Report> {
        let mut store = self.store.borrow_mut();
        let mut pass = Pass {
            config: &self.config,
            policy: &self.policy,
            root: self.dir.path(),
            launcher: &self.deferred,
            now,
            dry_run: dry,
            ledger: None,
        };
        if dry {
            pass.ledger = Some(store.ledger());
            pass.run(&self.hub, &REPO.parse().unwrap(), None, None)
                .unwrap()
        } else {
            pass.run(&self.hub, &REPO.parse().unwrap(), None, Some(&mut store))
                .unwrap()
        }
    }
    pub(super) fn run_worker(&self) {
        for path in self.deferred.0.borrow_mut().drain(..) {
            flow::jobs::worker(&path).unwrap();
            let invocation: flow::jobs::Invocation =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let answer: flow::jobs::Completion =
                serde_json::from_slice(&std::fs::read(path.with_extension("answer")).unwrap())
                    .unwrap();
            match (&invocation.task, &answer.result) {
                (
                    conary_core::plugin::Task::Fix { .. },
                    Some(Ok(conary_core::plugin::Response {
                        answer:
                            conary_core::plugin::Answer::Fix {
                                result: conary_core::plugin::FixResult::Pushed { head },
                                ..
                            },
                        ..
                    })),
                ) => {
                    self.hub.push(REPO, 3, head);
                    self.hub.refresh_flow(REPO, 3);
                    self.good_ci(head);
                }
                (
                    conary_core::plugin::Task::Review { .. },
                    Some(Ok(conary_core::plugin::Response {
                        answer: conary_core::plugin::Answer::Review { findings },
                        ..
                    })),
                ) => {
                    let record = self.record();
                    let work = record.jobs.iter().find(|w| w.path == path).unwrap();
                    let body = review_body("reviewer", &work.job.revision, answer.at, findings);
                    self.sign_check(
                        &work.job.revision.head,
                        "conary/review/reviewer",
                        Some(&review_receipt(&work.job.revision.head, &body)),
                    );
                }
                (
                    conary_core::plugin::Task::Judge { .. },
                    Some(Ok(conary_core::plugin::Response {
                        answer: conary_core::plugin::Answer::Judge { .. },
                        ..
                    })),
                ) => self.judgment(1, Judgment::Pass, true),
                _ => {}
            }
        }
    }
    pub(super) fn record(&self) -> flow::Record {
        Data::read(self.dir.path(), &REPO.parse().unwrap(), 3).unwrap()
    }
    pub(super) fn approve(&self) {
        let revision = Revision::from(&self.hub.pull(REPO, 3).unwrap());
        self.hub.person_review(
            REPO,
            3,
            "maintainer",
            "User",
            &revision.head,
            &owner_approval(&REPO.parse().unwrap(), 3, &revision),
            "APPROVED",
        );
    }
    pub(super) fn clean_review(&self) {
        self.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 0);
        let records = self.hub.reviews(REPO, 3).unwrap();
        let r = records.last().unwrap();
        self.sign_check(
            &head(3),
            "conary/review/reviewer",
            Some(&review_receipt(&head(3), r.body.as_deref().unwrap())),
        );
    }
    pub(super) fn sign_check(&self, head: &str, name: &str, receipt: Option<&[u8]>) -> u64 {
        let id = self.hub.run_id();
        self.hub.add_check(
            head,
            json!({"id":id,"name":name,"status":"completed",
            "conclusion":"success","started_at":"2026-11-01T00:00:00Z"}),
        );
        self.attest_run(head, id, receipt);
        id
    }
    pub(super) fn attest_run(&self, head: &str, id: u64, receipt: Option<&[u8]>) {
        let tree = crate::branches::Branches::tree(&self.hub, REPO, head).unwrap();
        let mut statement = testing::statement(head, &tree, "pass");
        if let Some(receipt) = receipt {
            let mut predicate: serde_json::Value =
                serde_json::from_str(statement.lines().nth(2).unwrap()).unwrap();
            predicate["log"]["sha256"] = json!(receipt_digest(receipt));
            statement = format!("conary commit v1\n{head}\n{predicate}\n");
        }
        let claims = testing::claims(REPO, head, id, &statement);
        self.hub
            .attest(id, &testing::file(&statement, &testing::token(&claims)));
    }
    pub(super) fn negative(&self, head: &str, result: &str) -> u64 {
        let id = self.hub.run_id();
        self.hub.add_check(
            head,
            json!({"id":id,"name":"negative","status":"completed",
            "conclusion":if result=="pass" {"failure"} else {"success"}}),
        );
        let base = crate::api::Source::merge_base(&self.hub, REPO, TRUNK, head).unwrap();
        let statement = testing::negative_statement(&base, head, result);
        let claims = testing::claims(REPO, head, id, &statement);
        self.hub
            .attest(id, &testing::file(&statement, &testing::token(&claims)));
        id
    }
    pub(super) fn good_ci(&self, head: &str) {
        self.hub.ci(head, &[("check", "success")]);
        for run in self.hub.checks_on(head) {
            // Hub's generic convenience method signs for its default repo;
            // this fixture uses the example's reserved simulation namespace.
            self.hub.forget_artifacts(Some(run.id));
            self.attest_run(head, run.id, None);
        }
        self.negative(head, "none");
    }
    /// A producer's judgment, judged for every reason on an empty discussion.
    pub(super) fn judgment(&self, generation: u64, verdict: Judgment, attest: bool) {
        self.judgment_on(generation, verdict, attest, super::judged_for_everything());
    }
    /// A producer's v3 judgment of the PR's current revision, judged on
    /// `basis`, and its signed receipt when `attest`.
    pub(super) fn judgment_on(
        &self,
        generation: u64,
        verdict: Judgment,
        attest: bool,
        basis: crate::flow::Basis,
    ) {
        let revision = Revision::from(&self.hub.pull(REPO, 3).unwrap());
        let provenance = super::judge_provenance();
        let body = self
            .config
            .settings
            .judge_key
            .as_ref()
            .unwrap()
            .judgment_by(
                REPO,
                3,
                verdict,
                &revision,
                generation,
                provenance.clone(),
                basis.clone(),
            )
            .unwrap();
        self.hub.flow_comment(REPO, 3, "producer", &body);
        if attest {
            self.sign_check(
                &revision.head,
                "conary/judgment",
                Some(&judgment_receipt_by(
                    &revision,
                    generation,
                    Some(verdict),
                    Some(&provenance),
                    Some(&basis),
                )),
            );
        }
    }
    pub(super) fn as_bot(&mut self) {
        self.hub
            .edit_pull(REPO, 3, |p| p["head"]["ref"] = json!("renovate/dep"));
        self.hub.commit_details(
            &head(3),
            Some("change: bot"),
            Some("bot@example.org"),
            Some("bot@example.org"),
        );
        self.config.settings.renovate_email = Some("bot@example.org".into());
        self.config
            .settings
            .bot_emails
            .insert("bot@example.org".into());
        self.hub.refresh_flow(REPO, 3);
    }
}
