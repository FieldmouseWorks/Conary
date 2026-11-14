//! Scenario setup separates GitHub state from local service observations.
//! Every decision reads the Hub afresh. Only jobs, service availability and
//! machine proof observations are added afterwards: GitHub cannot give them.
use crate::api::Source;
use crate::flow::*;
use crate::sim::{Hub, REMI};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Default, Clone)]
pub(super) struct Service {
    pub queued: bool,
    pub unavailable_until: Option<u64>,
    pub answer: Option<Review>,
    pub under_way: bool,
    pub under_way_on: Option<Revision>,
    pub failed_at: Option<u64>,
    pub queue_failed_at: Option<u64>,
    pub missing: bool,
}
pub(super) struct Case {
    pub hub: Hub,
    pub memory: Memory,
    pub settings: Settings,
    pub services: BTreeMap<String, Service>,
    pub jobs: BTreeMap<Kind, Job>,
    pub claims: Vec<FixClaim>,
    pub stopped_jobs: std::collections::BTreeSet<String>,
    pub proof: Option<Proof>,
    pub proof_data: ProofData,
    pub failed: Option<Failure>,
    pub failed_review: Option<String>,
    pub review_unavailable: bool,
    pub succeeded: std::collections::BTreeSet<Failure>,
    pub base_held: bool,
    pub train: TrainState,
    pub unseen: bool,
    /// The discussion the workers' answers make: what `Facts::context`
    /// digests. Empty until a test says otherwise.
    pub discussion: Vec<(
        conary_core::plugin::Finding,
        Option<conary_core::plugin::FindingAnswer>,
    )>,
}
impl Case {
    pub fn new() -> Self {
        let (hub, _, memory) = super::flow::setup();
        Self {
            hub,
            memory,
            settings: crate::sim::settings(),
            services: BTreeMap::new(),
            jobs: BTreeMap::new(),
            claims: Vec::new(),
            stopped_jobs: Default::default(),
            proof: None,
            proof_data: ProofData::default(),
            failed: None,
            failed_review: None,
            review_unavailable: false,
            succeeded: Default::default(),
            base_held: false,
            train: TrainState::default(),
            unseen: false,
            discussion: Vec::new(),
        }
    }
    pub fn roles(&mut self, names: &[&str]) {
        self.settings.reviewers = names.iter().map(|n| (*n).into()).collect();
        self.hub.remove_flow_reviews(REMI, 3);
    }
    pub fn review(&mut self, role: &str, verdict: Verdict, at: u64) {
        self.settings.reviewers.insert(role.into());
        self.hub.flow_review(REMI, 3, role, verdict, at);
    }
    pub fn answered(&self, role: &str) {
        self.hub.resolve_flow_findings(REMI, 3, role);
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
    /// `flow::judging_context` over the discussion now.
    pub fn context(&self) -> String {
        judging_context(self.discussion.iter().map(|(f, a)| (f, a.as_ref())))
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
    /// Post a sealed judgment at the next generation.
    fn issue(&mut self, seal: impl FnOnce(&SealKey, &Revision, u64) -> String) {
        let key = SealKey::from_hex(&"a".repeat(64)).unwrap();
        self.settings.judge_key = Some(key.clone());
        self.settings.verdict_authors.insert("owner".into());
        let generation = self.hub.comments(REMI, 3).unwrap().len() as u64 + 1;
        let marker = seal(&key, &self.scope(), generation);
        self.hub.flow_comment(REMI, 3, "owner", &marker);
    }
    pub fn sweep(&mut self, verdict: &str) {
        let key = SealKey::from_hex(&"b".repeat(64)).unwrap();
        self.settings.major_key = Some(key.clone());
        self.settings.verdict_authors.insert("owner".into());
        let generation = self.hub.comments(REMI, 3).unwrap().len() as u64 + 1;
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
    pub fn service(&mut self, name: &str) -> &mut Service {
        self.services.entry(name.into()).or_default()
    }
    pub fn facts(&self) -> Facts {
        let mut f=read(&self.hub,&REMI.parse().unwrap(),3,
            &conary_core::Policy::parse(br#"{"r2":["^secrets/","(^|/)CODEOWNERS$"],"docs":["^docs/","^README.md$"],"notDocs":["^docs/DESIGN.md$"],"hidden":["^hidden/"]}"#).unwrap(),
            self.settings.clone(),&["owner".into()]).unwrap();
        for (name, s) in &self.services {
            if let Some(r) = f.reviewers.get_mut(name) {
                r.queued = s.queued;
                r.unavailable_until = s.unavailable_until;
                r.answer = s.answer.clone();
                r.under_way = s
                    .under_way_on
                    .clone()
                    .or_else(|| s.under_way.then(|| f.revision.clone()));
                r.failed_at = s.failed_at;
                r.queue_failed_at = s.queue_failed_at;
                r.missing = s.missing;
            }
        }
        f.context = Some(self.context());
        f.jobs = self.jobs.clone();
        f.claims = self.claims.clone();
        f.stopped_jobs = self.stopped_jobs.clone();
        f.proof_data = self.proof_data.clone();
        if let Some(p) = &self.proof {
            f.proof = p.clone();
        }
        f.failed = self.failed;
        f.failed_review = self.failed_review.as_ref().map(|role| FailedPost {
            role: role.clone(),
            unavailable: self.review_unavailable,
        });
        f.succeeded = self.succeeded.clone();
        f.base_held |= self.base_held;
        f.train = self.train.clone();
        f.unseen |= self.unseen;
        f
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
