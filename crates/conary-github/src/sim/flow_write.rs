//! The same flow effects as the real API, with the normal fault/call machinery.
use super::faults::{Asked, Call};
use super::{Hub, STEWARD};
use crate::api::ApiError;
use crate::flow::{Stage, Write};
use serde_json::json;
impl Write for Hub {
    fn dispatch(
        &self,
        repo: &str,
        workflow: &str,
        git_ref: &str,
        inputs: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), ApiError> {
        let dispatch_id = inputs.get("dispatch_id").cloned().unwrap_or_default();
        self.write(
            Asked::new(Call::Dispatch, repo),
            &[repo, workflow, git_ref, &dispatch_id],
            || {
                // A run's one job is the check run numbered a tenth of it.
                let id = self.run_id() * 10;
                let mut world = self.world.borrow_mut();
                let tip = world
                    .repos
                    .get(repo)
                    .and_then(|r| r.branches.get(git_ref).cloned())
                    .unwrap_or_default();
                world.workflow_runs.entry(repo.into()).or_default().insert(
                    0,
                    json!({"id":id,"display_title":format!("conary {dispatch_id}"),
                            "status":"queued","conclusion":null,"head_sha":tip,
                            "workflow":workflow,"ref":git_ref,"inputs":inputs}),
                );
                Ok(())
            },
        )
    }
    fn cancel_run(&self, repo: &str, run: u64) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::CancelRun, repo),
            &[repo, &run.to_string()],
            || {
                if !self.world.borrow().cancel_ignored {
                    self.finish_workflow(repo, run, "cancelled");
                }
                Ok(())
            },
        )
    }
    fn status(&self, repo: &str, head: &str, settled: bool, text: &str) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowStatus, repo).mark(if settled {
                crate::train::Mark::Landing
            } else {
                crate::train::Mark::Waiting
            }),
            &[repo, head],
            || {
                let mut world = self.world.borrow_mut();
                world.statuses.entry(head.into()).or_default().insert(
                0,
                json!({
                    "context":crate::flow::SETTLED, "state":if settled {"success"} else {"pending"},
                    "created_at":self.now(),"description":text,"creator":{"login":STEWARD,"type":"Bot"}
                }),
            );
                Ok(())
            },
        )
    }
    fn stage(&self, repo: &str, pr: u64, stage: Stage) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowStage, repo).pr(pr),
            &[repo, &pr.to_string()],
            || {
                self.edit_pull(repo, pr, |p| {
                    let labels = p["labels"].as_array_mut().unwrap();
                    labels.retain(|l| !Stage::owns_label(l["name"].as_str().unwrap_or("")));
                    if let Some(label) = stage.label() {
                        labels.push(json!({"name":label}));
                    }
                });
                Ok(())
            },
        )
    }
    fn comment(&self, repo: &str, pr: u64, body: &str) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowComment, repo).pr(pr),
            &[repo, &pr.to_string(), body],
            || {
                self.flow_comment(repo, pr, STEWARD, body);
                Ok(())
            },
        )
    }
    fn reply(&self, repo: &str, pr: u64, thread: &str, body: &str) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowReply, repo).pr(pr),
            &[repo, &pr.to_string(), thread, body],
            || {
                let mut world = self.world.borrow_mut();
                let flow = world
                    .repos
                    .get_mut(repo)
                    .unwrap()
                    .flow
                    .get_mut(&pr)
                    .unwrap();
                let thread = flow
                    .threads
                    .get_mut(thread)
                    .ok_or_else(|| ApiError::new("Could not resolve to a review thread"))?;
                thread.comments.push(crate::flow::ThreadComment {
                    author: Some(STEWARD.into()),
                    body: body.into(),
                });
                Ok(())
            },
        )
    }
    fn resolve(&self, repo: &str, pr: u64, thread: &str) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowResolve, repo).pr(pr),
            &[repo, &pr.to_string(), thread],
            || {
                let mut world = self.world.borrow_mut();
                let flow = world
                    .repos
                    .get_mut(repo)
                    .unwrap()
                    .flow
                    .get_mut(&pr)
                    .unwrap();
                flow.threads
                    .get_mut(thread)
                    .ok_or_else(|| ApiError::new("Could not resolve to a review thread"))?
                    .resolved = true;
                flow.open_threads = flow.threads.values().filter(|t| !t.resolved).count() as u32;
                Ok(())
            },
        )
    }
    fn update(&self, repo: &str, pr: u64, head: &str) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowUpdate, repo).pr(pr),
            &[repo, &pr.to_string(), head],
            || Ok(()),
        )
    }
    fn rerequest(&self, repo: &str, suite: u64) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowRerequest, repo),
            &[repo, &suite.to_string()],
            || {
                self.run_again(suite);
                Ok(())
            },
        )
    }
    fn close(&self, repo: &str, pr: u64) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowClose, repo).pr(pr),
            &[repo, &pr.to_string()],
            || {
                self.edit_pull(repo, pr, |p| p["state"] = json!("closed"));
                Ok(())
            },
        )
    }
    fn owner(&self, repo: &str, pr: u64, label: &str) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowOwner, repo).pr(pr),
            &[repo, &pr.to_string(), label],
            || {
                let mut labels: Vec<String> = self.repo_mut(repo, |r| {
                    r.pulls[&pr]["labels"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|l| l["name"].as_str().unwrap().to_owned())
                        .collect()
                });
                if !labels.iter().any(|l| l == label) {
                    labels.push(label.into());
                }
                let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
                // The flow writes as its App.
                self.label_as(repo, pr, &labels, (super::STEWARD, "Bot"));
                Ok(())
            },
        )
    }
    fn disable_auto_merge(&self, repo: &str, pr: u64) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowDisable, repo).pr(pr),
            &[repo, &pr.to_string()],
            || {
                self.edit_pull(repo, pr, |p| p["auto_merge"] = json!(null));
                Ok(())
            },
        )
    }
}

impl Hub {
    /// A dispatched run ends, as GitHub would end it.
    pub(crate) fn finish_workflow(&self, repo: &str, run: u64, conclusion: &str) {
        if let Some(runs) = self.world.borrow_mut().workflow_runs.get_mut(repo) {
            for r in runs.iter_mut().filter(|r| r["id"] == run) {
                if r["status"] != "completed" {
                    r["status"] = json!("completed");
                    r["conclusion"] = json!(conclusion);
                }
            }
        }
    }
    /// A workflow's run list is cut to its newest `cap` runs.
    pub(crate) fn cap_run_list(&self, cap: Option<usize>) {
        self.world.borrow_mut().list_cap = cap;
    }
    /// The run list can only be read in part, and says so.
    pub(crate) fn incomplete_run_list(&self, incomplete: bool) {
        self.world.borrow_mut().list_incomplete = incomplete;
    }
    /// Cancel requests are accepted and ignored, or honoured again.
    pub(crate) fn ignore_cancels(&self, ignored: bool) {
        self.world.borrow_mut().cancel_ignored = ignored;
    }
    /// Every dispatched run of a repo, newest first.
    pub(crate) fn dispatched(&self, repo: &str) -> Vec<serde_json::Value> {
        self.world
            .borrow()
            .workflow_runs
            .get(repo)
            .cloned()
            .unwrap_or_default()
    }
}
