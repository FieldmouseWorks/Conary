//! prs.py's CI decisions, read from Hub before every assertion of next().
//! Scenario comments cite executable blocks in burrow's Python tests.
use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::head;
use crate::flow::*;
use crate::sim::{Hub, REMI};

pub(super) const BLANK_DOCS: &str = "check / docs only into ${{ github.event.pull_request.base.ref }} from ${{ needs.lane.outputs.fork }}";
pub(super) const BLANK_LANE: &str = "check / lane into ${{ github.event.pull_request.base.ref }}";
pub(super) const GATE: &str = "check / check";
pub(super) const TWIN: &str = ".github/workflows/flake-retarget.yml";

#[derive(Clone)]
pub(super) struct Run {
    pub(super) suite: u64,
    pub(super) metadata: Suite,
    pub(super) checks: Vec<Value>,
}
impl Run {
    pub(super) fn new(suite: u64, jobs: &[(&str, &str)]) -> Self {
        Self {
            suite,
            metadata: Suite {
                status: "completed".into(),
                conclusion: None,
                app: "github-actions".into(),
                workflow: Some(Workflow {
                    id: "flake".into(),
                    name: "flake".into(),
                    path: Some(".github/workflows/flake.yml".into()),
                }),
            },
            checks: jobs
                .iter()
                .enumerate()
                .map(|(i, (name, conclusion))| {
                    json!({
                        "id": suite * 100 + i as u64, "name": name, "status": "completed",
                        "conclusion": conclusion, "check_suite": {"id": suite}
                    })
                })
                .collect(),
        }
    }
    pub(super) fn whole(suite: u64, base: &str, conclusion: &str) -> Self {
        Self::new(
            suite,
            &[
                (&format!("check / lane into {base}"), "success"),
                (GATE, conclusion),
                (BLANK_DOCS, "skipped"),
            ],
        )
    }
    pub(super) fn docs(suite: u64, fork: &str, conclusion: &str) -> Self {
        Self::new(
            suite,
            &[
                ("check / lane into main", "success"),
                (GATE, "skipped"),
                (
                    &format!("check / docs only into main from {fork}"),
                    conclusion,
                ),
            ],
        )
    }
    pub(super) fn going(mut self) -> Self {
        self.metadata.status = "in_progress".into();
        self
    }
    pub(super) fn workflow(mut self, id: &str, path: Option<&str>) -> Self {
        self.metadata.workflow = Some(Workflow {
            id: id.into(),
            name: "flake".into(),
            path: path.map(str::to_owned),
        });
        self
    }
    pub(super) fn no_workflow(mut self) -> Self {
        self.metadata.workflow = None;
        self
    }
    pub(super) fn add(mut self, name: &str, conclusion: &str) -> Self {
        let i = self.checks.len() as u64;
        self.checks.push(json!({"id": self.suite*100+i,"name":name,"status":"completed","conclusion":conclusion,"check_suite":{"id":self.suite}}));
        self
    }
}

pub(super) struct Scenario {
    pub(super) hub: Hub,
    pub(super) memory: Memory,
}
impl Scenario {
    pub(super) fn new() -> Self {
        let (hub, f, memory) = super::flow::setup();
        hub.flow_snapshot(
            REMI,
            3,
            Snapshot {
                default_branch: None,
                revision: f.revision.clone(),
                draft: false,
                complete: true,
                merge_state: "CLEAN".into(),
                open_threads: 0,
                threads: Default::default(),
                resolved_reviews: Default::default(),
                updated_at: 0,
                suites: BTreeMap::new(),
                checks_complete: true,
                rebase_removed: Vec::new(),
                timeline_cut_at: None,
            },
        );
        Self { hub, memory }
    }
    pub(super) fn read(&self, runs: &[Run], proofs: &[&str]) -> Facts {
        self.read_settings(
            runs,
            Settings {
                proofs: proofs
                    .iter()
                    .map(|p| ((*p).into(), Recipe::Job("run proof".into())))
                    .collect(),
                ..Settings::default()
            },
        )
    }
    pub(super) fn read_settings(&self, runs: &[Run], settings: Settings) -> Facts {
        self.hub.put_checks(
            &head(3),
            Value::Array(runs.iter().flat_map(|r| r.checks.clone()).collect()),
        );
        self.hub.flow_suites(
            REMI,
            3,
            runs.iter().map(|r| (r.suite, r.metadata.clone())).collect(),
        );
        read(
            &self.hub,
            &REMI.parse().unwrap(),
            3,
            &conary_core::Policy::parse(b"{}").unwrap(),
            settings,
            &["owner".into()],
        )
        .unwrap()
    }
    pub(super) fn assert(&self, runs: &[Run], state: Ci, names: &[&str]) {
        let f = self.read(runs, &[]);
        let observed = if f.checks.iter().any(|c| c.state == Ci::Red) {
            Ci::Red
        } else if f.checks.iter().any(|c| c.state == Ci::Pending) {
            Ci::Pending
        } else if f.checks.is_empty() {
            Ci::None
        } else {
            Ci::Green
        };
        assert_eq!(observed, state, "checks: {:?}", f.checks);
        let actual: std::collections::BTreeSet<_> = f
            .checks
            .iter()
            .filter(|c| c.state == state && state != Ci::Green)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            actual,
            names.iter().copied().collect(),
            "checks: {:?}",
            f.checks
        );
        let n = next(&f, &self.memory, 4000).0;
        match state {
            Ci::Green => assert_eq!(n.step, Step::Merge),
            Ci::Red => assert_eq!(n.step, Step::HandFix(Kind::Red)),
            Ci::Pending => assert_eq!(n.stage, Stage::Ci),
            Ci::None => panic!("these scenarios all have CI"),
        }
    }
}
