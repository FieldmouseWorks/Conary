//! The same flow effects as the real API, with the normal fault/call machinery.
use super::faults::{Asked, Call};
use super::{Hub, STEWARD};
use crate::api::ApiError;
use crate::flow::{Stage, Write};
use serde_json::json;
impl Write for Hub {
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
