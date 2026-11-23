//! Scenario setup separates GitHub state from local service observations.
//! Every decision reads the Hub afresh. Only jobs and machine proof
//! observations are added afterwards: GitHub cannot give them.
use crate::api::Source;
use crate::flow::*;
use crate::sim::{CONNECTOR, Hub, REMI};
use serde_json::json;
use std::collections::BTreeMap;

pub(super) struct Case {
    pub hub: Hub,
    pub memory: Memory,
    pub settings: Settings,
    pub jobs: BTreeMap<Kind, Job>,
    pub claims: Vec<FixClaim>,
    pub stopped_jobs: std::collections::BTreeSet<String>,
    pub proof: Option<Proof>,
    pub proof_data: ProofData,
    /// What every head's preparation of its next proof already answered,
    /// for cases about what follows it.
    pub readiness: Option<Readiness>,
    pub failed: Option<Failure>,
    pub failed_why: Option<String>,
    pub succeeded: std::collections::BTreeSet<Failure>,
    pub base_held: bool,
    pub train: TrainState,
    /// What each finished judge job was asked on, as its invocation says.
    pub judge_requests: BTreeMap<String, Option<Basis>>,
    /// Why the flow's own judge answered `owner`, as its worker's answer says.
    pub judge_reason: Option<String>,
    /// Whether a `job` plugin serves the stuck role, as the pass says.
    pub stuck_role: bool,
}
impl Case {
    pub fn new() -> Self {
        let (hub, _, memory) = super::flow::setup();
        Self {
            hub,
            memory,
            settings: crate::sim::settings(),
            jobs: BTreeMap::new(),
            claims: Vec::new(),
            stopped_jobs: Default::default(),
            proof: None,
            proof_data: ProofData::default(),
            readiness: None,
            failed: None,
            failed_why: None,
            succeeded: Default::default(),
            base_held: false,
            train: TrainState::default(),
            judge_requests: BTreeMap::new(),
            judge_reason: None,
            stuck_role: false,
        }
    }
    pub fn roles(&mut self, names: &[&str]) {
        self.settings.reviewers = names.iter().map(|n| (*n).into()).collect();
        self.hub.remove_flow_reviews(REMI, 3);
    }
    pub fn review(&mut self, role: &str, verdict: Verdict, at: u64) {
        self.settings.reviewers.insert(role.into());
        self.hub.flow_review(REMI, 3, verdict, at);
    }
    pub fn answered(&self) {
        self.hub.resolve_flow_findings(REMI, 3);
    }
    pub fn threads(&self, count: u32) {
        self.hub.edit_flow(REMI, 3, |s| s.open_threads = count);
    }
    pub fn scope(&self) -> Revision {
        Revision::from(&self.hub.pull(REMI, 3).unwrap())
    }
    pub fn push(&self, label: &str) {
        let named;
        let sha = if label.len() == 40
            && label
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            label
        } else {
            named = crate::sim::tree_of(&[label]);
            &named
        };
        self.hub.push(REMI, 3, sha);
        self.hub.refresh_flow(REMI, 3);
        self.hub.ci(sha, &[("check", "success")]);
    }
    pub fn description(&self, body: &str, at: u64) {
        self.hub.edit_pull(REMI, 3, |p| p["body"] = json!(body));
        self.hub.refresh_flow(REMI, 3);
        self.hub.edit_flow(REMI, 3, |s| s.updated_at = at);
    }
    pub fn label(&self, labels: &[&str]) {
        self.hub.label(REMI, 3, labels);
    }
    /// `flow::judging_context` over a fresh read of the discussion on
    /// GitHub: its review threads and review bodies.
    pub fn context(&self) -> String {
        read_judging_context(&self.hub, REMI, 3).unwrap()
    }
    /// A judgment of the current revision. A contract word is a producer's
    /// v3 judgment, judged for every reason on the discussion now; any other
    /// word only an older issuance can carry.
    pub fn judge(&mut self, verdict: &str) {
        match Judgment::from_word(verdict) {
            Some(judgment) => {
                let basis = Basis {
                    why: super::every_reason(),
                    context: self.context(),
                };
                self.judge_on(judgment, basis);
            }
            None => self.judge_v2(verdict),
        }
    }
    /// A v2 judgment of the current revision: it says nothing of what it
    /// was judged on.
    pub fn judge_v2(&mut self, verdict: &str) {
        self.issue(|key, scope, generation| {
            key.decision(REMI, 3, "autoland-judgment", verdict, scope, generation)
        });
    }
    /// A producer's v3 judgment of the current revision, judged on `basis`.
    pub fn judge_on(&mut self, verdict: Judgment, basis: Basis) {
        self.issue(|key, scope, generation| {
            key.judgment_by(
                REMI,
                3,
                verdict,
                scope,
                generation,
                super::judge_provenance(),
                basis,
            )
            .unwrap()
        });
    }
    /// The PR's comments but the connector's: each case's sealed issuances
    /// count from one, however many reviews the connector summed up.
    fn issued(&self) -> u64 {
        let comments = self.hub.comments(REMI, 3).unwrap();
        comments
            .iter()
            .filter(|c| c.user.as_ref().is_none_or(|u| u.login != CONNECTOR))
            .count() as u64
    }
    /// Post a sealed judgment at the next generation.
    fn issue(&mut self, seal: impl FnOnce(&SealKey, &Revision, u64) -> String) {
        let key = SealKey::from_hex(&"a".repeat(64)).unwrap();
        self.settings.judge_key = Some(key.clone());
        self.settings.verdict_authors.insert("owner".into());
        let generation = self.issued() + 1;
        let marker = seal(&key, &self.scope(), generation);
        self.hub.flow_comment(REMI, 3, "owner", &marker);
    }
    pub fn sweep(&mut self, verdict: &str) {
        let key = SealKey::from_hex(&"b".repeat(64)).unwrap();
        self.settings.major_key = Some(key.clone());
        self.settings.verdict_authors.insert("owner".into());
        let generation = self.issued() + 1;
        let marker = key.decision(REMI, 3, "major-verdict", verdict, &self.scope(), generation);
        self.hub.flow_comment(REMI, 3, "owner", &marker);
    }
    pub fn approve(&mut self) {
        self.settings.people.insert("maintainer".into());
        let scope = self.scope();
        self.hub.person_review(
            REMI,
            3,
            "maintainer",
            "User",
            &scope.head,
            &owner_approval(&REMI.parse().unwrap(), 3, &scope),
            "APPROVED",
        );
    }
    pub fn secret(&self) {
        let h = self.hub.head_of(REMI, 3);
        self.hub.touch(&h, &["secrets/app.yaml"]);
    }
    pub fn no_ci(&self) {
        let h = self.hub.head_of(REMI, 3);
        self.hub.put_checks(&h, json!([]));
    }
    pub fn ci(&self, conclusion: &str) {
        let h = self.hub.head_of(REMI, 3);
        self.hub.ci(&h, &[("check", conclusion)]);
    }
    pub fn facts(&self) -> Facts {
        let mut f=read(&self.hub,&REMI.parse().unwrap(),3,
            &conary_core::Policy::parse(br#"{"r2":["^secrets/","(^|/)CODEOWNERS$"],"docs":["^docs/","^README.md$"],"notDocs":["^docs/DESIGN.md$"]}"#).unwrap(),
            self.settings.clone(),&["owner".into()]).unwrap();
        labeller(&self.hub, &mut f, &self.memory);
        f.judge_requests = self.judge_requests.clone();
        f.judge_reason = self.judge_reason.clone();
        f.jobs = self.jobs.clone();
        f.claims = self.claims.clone();
        f.stopped_jobs = self.stopped_jobs.clone();
        f.proof_data = self.proof_data.clone();
        if let Some(answer) = &self.readiness {
            f.proof_data.prepared.push(Preparation {
                job: Job {
                    id: "prepared".into(),
                    revision: f.revision.clone(),
                    state: JobState::Done,
                    started: 0,
                    repeated: false,
                },
                recipes: crate::flow::selected(&f),
                ended: Some(0),
                answer: Some(answer.clone()),
            });
        }
        if let Some(p) = &self.proof {
            f.proof = p.clone();
        }
        f.failed = self.failed;
        f.failed_why = self.failed_why.clone();
        f.succeeded = self.succeeded.clone();
        f.base_held |= self.base_held;
        f.train = self.train.clone();
        f.stuck_role = self.stuck_role;
        f
    }
    /// A preparation of the programs the next proof runs on the head (its
    /// builds, else its commands), started at 600: running while `ended` is
    /// `None`, else ended then with `answer`, or broken without one.
    pub fn prepared(&mut self, answer: Option<Readiness>, ended: Option<u64>) {
        let programs = |build: bool| -> BTreeMap<String, Recipe> {
            self.settings
                .proofs
                .iter()
                .filter(|(_, r)| match r {
                    Recipe::Build(_) => build,
                    Recipe::Run(_) => !build,
                    Recipe::Job(_) => false,
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let recipes = Some(programs(true))
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| programs(false));
        let id = format!("prepare-{}", self.proof_data.prepared.len() + 1);
        self.proof_data.prepared.push(Preparation {
            job: Job {
                id,
                revision: self.scope(),
                state: match (ended, &answer) {
                    (None, _) => JobState::Running,
                    (Some(_), Some(_)) => JobState::Done,
                    (Some(_), None) => JobState::Broken,
                },
                started: 600,
                repeated: false,
            },
            recipes,
            ended,
            answer,
        });
    }
    /// A finished preparation that says proving may start, and whether it
    /// needs a proof slot.
    pub fn ready(&mut self, slot: bool) {
        self.prepared(
            Some(Readiness {
                ready: true,
                slot,
                why: "inputs built".into(),
            }),
            Some(650),
        );
    }
    /// Every head's next proof prepared, needing a slot: proving starts as
    /// it did before proofs were prepared.
    pub fn programs(&mut self) {
        self.readiness = Some(Readiness {
            ready: true,
            slot: true,
            why: "inputs built".into(),
        });
    }
    pub fn view(&self, now: u64) -> Next {
        next(&self.facts(), &self.memory, now).0
    }
    pub fn pass(&mut self, now: u64) -> Next {
        let (n, m) = next(&self.facts(), &self.memory, now);
        self.memory = m;
        n
    }
}
