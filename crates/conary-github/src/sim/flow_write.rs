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
        let nonce = inputs.get("nonce").cloned().unwrap_or_default();
        self.write(
            Asked::new(Call::Dispatch, repo),
            &[repo, workflow, git_ref, &nonce],
            || {
                let id = self.run_id();
                self.world
                    .borrow_mut()
                    .workflow_runs
                    .entry(repo.into())
                    .or_default()
                    .insert(
                        0,
                        json!({"id":id,"display_title":format!("conary {nonce}"),
                            "status":"queued","conclusion":null,
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
                self.finish_workflow(repo, run, "cancelled");
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
    fn review(&self, repo: &str, pr: u64, head: &str, body: &str) -> Result<(), ApiError> {
        self.write(Asked::new(Call::FlowReview, repo).pr(pr), &[repo,&pr.to_string(),head,body], || {
            let mut world = self.world.borrow_mut();
            let records = world.repos.get_mut(repo).unwrap().reviews.entry(pr).or_default();
            records.push(json!({"id":records.len()+1,"commit_id":head,"body":body,
                "submitted_at":"2099-01-01T00:00:00Z","user":{"login":STEWARD,"type":"Bot"},"state":"COMMENTED"}));
            Ok(())
        })
    }
    fn update(&self, repo: &str, pr: u64, head: &str) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::FlowUpdate, repo).pr(pr),
            &[repo, &pr.to_string(), head],
            || Ok(()),
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
                self.edit_pull(repo, pr, |p| {
                    let labels = p["labels"].as_array_mut().unwrap();
                    if !labels.iter().any(|l| l["name"] == label) {
                        labels.push(json!({"name":label}));
                    }
                });
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
