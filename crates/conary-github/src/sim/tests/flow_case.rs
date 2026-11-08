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
    pub proof: Option<Proof>,
    pub proof_data: ProofData,
    pub failed: Option<Failure>,
    pub failed_review: Option<String>,
    pub succeeded: std::collections::BTreeSet<Failure>,
    pub base_held: bool,
    pub unseen: bool,
}
impl Case {
    pub fn new() -> Self {
        let (hub, _, memory) = super::flow::setup();
        Self {
            hub,
            memory,
            settings: Settings::default(),
            services: BTreeMap::new(),
            jobs: BTreeMap::new(),
            claims: Vec::new(),
            proof: None,
            proof_data: ProofData::default(),
            failed: None,
            failed_review: None,
            succeeded: Default::default(),
            base_held: false,
            unseen: false,
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
    pub fn push(&self, sha: &str) {
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
    pub fn judge(&mut self, verdict: &str) {
        let key = SealKey::from_hex(&"a".repeat(64)).unwrap();
        self.settings.judge_key = Some(key.clone());
        self.settings.verdict_authors.insert("owner".into());
        let p = self.hub.pull(REMI, 3).unwrap();
        let on = format!(
            "{} {}",
            p.base.name,
            fingerprint(p.body.as_deref().unwrap_or(""))
        );
        let tag = key.seal(REMI, 3, "autoland-judgment", verdict, &p.head.sha, &on);
        self.hub.flow_comment(
            REMI,
            3,
            "owner",
            &format!("<!-- autoland-judgment: {verdict} {} {tag} -->", p.head.sha),
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
                r.under_way = s.under_way;
                r.failed_at = s.failed_at;
                r.queue_failed_at = s.queue_failed_at;
                r.missing = s.missing;
            }
        }
        f.jobs = self.jobs.clone();
        f.claims = self.claims.clone();
        f.proof_data = self.proof_data.clone();
        if let Some(p) = &self.proof {
            f.proof = p.clone();
        }
        f.failed = self.failed;
        f.failed_review = self.failed_review.clone();
        f.succeeded = self.succeeded.clone();
        f.base_held = self.base_held;
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
