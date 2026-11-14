//! Statuses and check runs on commits, and the attestations CI leaves
//! behind for the jobs that passed.

use serde_json::{Value, json};

use crate::attest::{ARTIFACT, FILE, testing};
use crate::train::Mark;

use super::{Hub, Posted, REMI, STEWARD, built, fixtures, parse};

impl Hub {
    /// The steward settles `sha`: its `review-settled` status, a success.
    pub(crate) fn settle(&self, sha: &str) {
        self.post_status(
            sha,
            built(
                fixtures::status(),
                &json!({"context": "review-settled", "state": "success", "target_url": null,
                   "description": "settled",
                   "created_at": "2026-10-29T10:00:00Z",
                   "creator": fixtures::user(STEWARD, "Bot")}),
            ),
        );
    }

    /// Posts a status on `sha`, newest of all, as a test writes GitHub's
    /// answer.
    pub(crate) fn post_status(&self, sha: &str, v: Value) {
        self.world
            .borrow_mut()
            .statuses
            .entry(sha.into())
            .or_default()
            .insert(0, v);
    }

    /// The statuses on `sha` are `statuses`, newest first, as a test writes
    /// GitHub's answer.
    pub(crate) fn put_statuses(&self, sha: &str, statuses: Value) {
        self.world.borrow_mut().statuses.remove(sha);
        for s in statuses.as_array().unwrap().iter().rev() {
            self.post_status(sha, s.clone());
        }
    }

    /// Every status on `sha`, newest first, as GitHub lists them.
    pub(crate) fn statuses_json(&self, sha: &str) -> Value {
        let all = self.world.borrow().statuses.get(sha).cloned();
        Value::Array(all.unwrap_or_default())
    }

    /// Everything on `sha` but the train's own statuses, newest first.
    pub(crate) fn others_statuses(&self, sha: &str) -> Value {
        let all = self.world.borrow().statuses.get(sha).cloned();
        Value::Array(
            all.into_iter()
                .flatten()
                .filter(|s| s["context"] != crate::train::CONTEXT)
                .collect(),
        )
    }

    /// Every status the train posted, oldest first.
    pub(crate) fn marks(&self) -> Vec<Posted> {
        let w = self.world.borrow();
        let mut all: Vec<(String, Posted)> = Vec::new();
        for (sha, statuses) in &w.statuses {
            for s in statuses
                .iter()
                .filter(|s| s["context"] == crate::train::CONTEXT)
            {
                let mark = match s["state"].as_str() {
                    Some("success") => Mark::Landing,
                    Some("pending") => Mark::Waiting,
                    Some("failure") => Mark::Red,
                    _ => Mark::Refused,
                };
                all.push((
                    s["created_at"].as_str().unwrap().into(),
                    Posted {
                        sha: sha.clone(),
                        mark,
                        description: s["description"].as_str().unwrap_or("").into(),
                        url: s["target_url"].as_str().map(str::to_owned),
                    },
                ));
            }
        }
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all.into_iter().map(|(_, p)| p).collect()
    }

    /// What the train's statuses said became of each head, as (commit,
    /// mark): landing, red or refused, the pending ones aside.
    pub(crate) fn marked(&self) -> Vec<(String, Mark)> {
        self.marks()
            .into_iter()
            .filter(|p| p.mark != Mark::Waiting)
            .map(|p| (p.sha, p.mark))
            .collect()
    }

    /// The train's latest status on a commit.
    pub(crate) fn latest(&self, sha: &str) -> Option<Mark> {
        self.marks()
            .into_iter()
            .rev()
            .find(|p| p.sha == sha)
            .map(|p| p.mark)
    }

    /// The next check run id: GitHub's are unique.
    pub(crate) fn run_id(&self) -> u64 {
        let mut w = self.world.borrow_mut();
        w.runs += 1;
        w.runs
    }

    /// CI on a commit: each check's name and conclusion, all completed,
    /// and each pass attested.
    /// One check suite of the commit's, as GitHub Actions makes one for
    /// a workflow's run on it.
    pub(crate) fn ci(&self, sha: &str, runs: &[(&str, &str)]) {
        let suite = u64::from_str_radix(&sha[sha.len() - 8..], 16).unwrap_or(1);
        let runs: Vec<Value> = runs
            .iter()
            .enumerate()
            .map(|(i, (name, conclusion))| {
                built(
                    fixtures::check_run(),
                    &json!({"id": self.run_id(), "name": name, "status": "completed",
                            "conclusion": conclusion,
                            "html_url": format!("https://github.com/FieldmouseWorks/remi/actions/runs/{i}"),
                            "started_at": format!("2026-10-29T11:0{i}:00Z"),
                            "check_suite": {"id": suite}}),
                )
            })
            .collect();
        self.put_checks(sha, Value::Array(runs));
        self.attest_passes(sha);
    }

    /// One more completed check run on `sha`, its job in a run of its own,
    /// reporting `text` if anything, and attested by nothing yet: its id.
    pub(crate) fn check_run(
        &self,
        sha: &str,
        name: &str,
        conclusion: &str,
        text: Option<&str>,
    ) -> u64 {
        let id = self.run_id();
        self.add_check(
            sha,
            built(
                fixtures::check_run(),
                &json!({"id": id, "name": name, "status": "completed",
                        "conclusion": conclusion, "check_suite": {"id": id},
                        "output": {"text": text}}),
            ),
        );
        id
    }

    /// The check runs on `sha` are `runs`, as a test writes GitHub's
    /// answer.
    pub(crate) fn put_checks(&self, sha: &str, runs: Value) {
        let runs = runs.as_array().unwrap().clone();
        self.world.borrow_mut().checks.insert(sha.into(), runs);
    }

    /// One more check run on `sha`, as a test writes GitHub's answer.
    pub(crate) fn add_check(&self, sha: &str, v: Value) {
        self.world
            .borrow_mut()
            .checks
            .entry(sha.into())
            .or_default()
            .push(v);
    }

    pub(crate) fn remove_check(&self, sha: &str, name: &str) {
        if let Some(checks) = self.world.borrow_mut().checks.get_mut(sha) {
            checks.retain(|c| c["name"].as_str() != Some(name));
        }
    }
    pub(crate) fn remove_checks(&self, sha: &str) {
        self.world.borrow_mut().checks.remove(sha);
    }

    /// The check runs on `sha`, as GitHub lists them.
    pub(crate) fn checks_on(&self, sha: &str) -> Vec<crate::api::CheckRun> {
        let w = self.world.borrow();
        w.checks.get(sha).into_iter().flatten().map(parse).collect()
    }

    /// Each completed, successful run on a commit attested, as its job
    /// attests: its statement and token in an artifact of its run.
    pub(crate) fn attest_passes(&self, sha: &str) {
        for run in self
            .checks_on(sha)
            .iter()
            .filter(|r| r.conclusion.as_deref() == Some("success"))
        {
            self.attest(run.id, &self.attestation(sha, run.id));
        }
    }

    /// An honest attestation of a pass on a commit by its check run.
    pub(crate) fn attestation(&self, sha: &str, run: u64) -> String {
        let statement = testing::statement(sha, &self.tree_at(sha), "pass");
        let claims = testing::claims(REMI, sha, run, &statement);
        testing::file(&statement, &testing::token(&claims))
    }

    /// Keeps `file` as an attestation in a run's artifacts.
    pub(crate) fn attest(&self, run: u64, file: &str) {
        let mut w = self.world.borrow_mut();
        let kept = w.artifacts.entry(run).or_default();
        let name = format!("{ARTIFACT}{}", kept.len());
        kept.push((name, testing::zipped(FILE, file.as_bytes())));
    }

    /// Takes away job `run`'s artifacts, or every job's.
    pub(crate) fn forget_artifacts(&self, run: Option<u64>) {
        let mut w = self.world.borrow_mut();
        match run {
            Some(run) => _ = w.artifacts.remove(&run).unwrap(),
            None => w.artifacts.clear(),
        }
    }

    /// Job `run`'s artifacts are past their retention: GitHub still lists
    /// them, as expired, and serves none.
    pub(crate) fn expire_artifacts(&self, run: u64) {
        self.world.borrow_mut().expired.insert(run);
    }

    /// Job `run` ran on `runner`, not a fresh one of its own.
    pub(crate) fn ran_on(&self, run: u64, runner: &str) {
        self.world.borrow_mut().runners.insert(run, runner.into());
    }
}
