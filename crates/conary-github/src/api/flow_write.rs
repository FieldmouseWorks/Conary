//! Effects for an explicitly requested flow pass.
use super::*;
use crate::flow::{Stage, Write};
use serde_json::{Value, json};

impl Api {
    /// A GraphQL mutation about PR `pr`, with the repository's token. Any
    /// error GitHub returns is a refusal.
    fn mutation(&self, repo: &str, pr: u64, query: &str, variables: Value) -> Result<(), ApiError> {
        let path = format!("/repos/{repo}/pulls/{pr}");
        let url = self.graphql();
        let mut response = self
            .agent
            .post(&url)
            .header("Authorization", &self.authorization(&path)?)
            .send_json(json!({"query":query,"variables":variables}))
            .map_err(|e| unreachable(&url, e))?;
        if !response.status().is_success() {
            return Err(failed(&url, &mut response));
        }
        let result: Value = parse(&url, &read(&url, &mut response)?)?;
        if result.get("errors").is_some() {
            return Err(ApiError::new("GitHub refused a GraphQL mutation"));
        }
        Ok(())
    }
    /// A PUT, or else a POST, whose answer says nothing the flow reads.
    pub(super) fn flow_call(&self, put: bool, path: &str, body: Value) -> Result<(), ApiError> {
        let url = format!("{}{path}", self.base);
        let request = if put {
            self.agent.put(&url)
        } else {
            self.agent.post(&url)
        };
        let mut response = request
            .header("Authorization", &self.authorization(path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body)
            .map_err(|e| unreachable(&url, e))?;
        if !response.status().is_success() {
            return Err(failed(&url, &mut response));
        }
        Ok(())
    }
}
impl Api {
    fn flow_post(&self, path: &str, body: Value) -> Result<(), ApiError> {
        let url = format!("{}{path}", self.base);
        let mut response = self
            .agent
            .post(&url)
            .header("Authorization", &self.authorization(path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body)
            .map_err(|e| unreachable(&url, e))?;
        if !response.status().is_success() {
            return Err(failed(&url, &mut response));
        }
        Ok(())
    }
}
impl Write for Api {
    fn dispatch(
        &self,
        repo: &str,
        workflow: &str,
        git_ref: &str,
        inputs: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), ApiError> {
        self.flow_post(
            &format!("/repos/{repo}/actions/workflows/{workflow}/dispatches"),
            json!({"ref":git_ref,"inputs":inputs}),
        )
    }
    fn cancel_run(&self, repo: &str, run: u64) -> Result<(), ApiError> {
        self.flow_post(
            &format!("/repos/{repo}/actions/runs/{run}/cancel"),
            json!({}),
        )
    }
    fn status(&self, repo: &str, head: &str, settled: bool, text: &str) -> Result<(), ApiError> {
        let _: Value = self.send(
            false,
            &format!("/repos/{repo}/statuses/{head}"),
            &json!({"context":crate::flow::SETTLED,"state":if settled {"success"} else {"pending"},
                "description":text.chars().take(140).collect::<String>()}),
        )?;
        Ok(())
    }
    fn stage(&self, repo: &str, pr: u64, stage: Stage) -> Result<(), ApiError> {
        let pull = self.pull(repo, pr)?;
        for label in pull.labels {
            if Stage::owns_label(&label.name) && stage.label() != Some(label.name.as_str()) {
                let encoded = label.name.replace(' ', "%20").replace(':', "%3A");
                let path = format!("/repos/{repo}/issues/{pr}/labels/{encoded}");
                let url = format!("{}{path}", self.base);
                let mut response = self
                    .agent
                    .delete(&url)
                    .header("Authorization", &self.authorization(&path)?)
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28")
                    .call()
                    .map_err(|e| unreachable(&url, e))?;
                if !response.status().is_success() && response.status().as_u16() != 404 {
                    return Err(failed(&url, &mut response));
                }
            }
        }
        if let Some(label) = stage.label() {
            self.owner(repo, pr, label)?;
        }
        Ok(())
    }

    fn comment(&self, repo: &str, pr: u64, body: &str) -> Result<(), ApiError> {
        let _: Value = self.send(
            false,
            &format!("/repos/{repo}/issues/{pr}/comments"),
            &json!({"body":body}),
        )?;
        Ok(())
    }
    fn reply(&self, repo: &str, pr: u64, thread: &str, body: &str) -> Result<(), ApiError> {
        self.mutation(
            repo,
            pr,
            "mutation($thread:ID!,$body:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$thread,body:$body}){comment{id}}}",
            json!({"thread":thread,"body":body}),
        )
    }
    fn resolve(&self, repo: &str, pr: u64, thread: &str) -> Result<(), ApiError> {
        self.mutation(
            repo,
            pr,
            "mutation($thread:ID!){resolveReviewThread(input:{threadId:$thread}){thread{isResolved}}}",
            json!({"thread":thread}),
        )
    }
    fn update(&self, repo: &str, pr: u64, head: &str) -> Result<(), ApiError> {
        self.flow_call(
            true,
            &format!("/repos/{repo}/pulls/{pr}/update-branch"),
            json!({"expected_head_sha":head}),
        )
    }
    fn rerun_failed_jobs(&self, repo: &str, run: u64) -> Result<(), ApiError> {
        self.flow_call(
            false,
            &format!("/repos/{repo}/actions/runs/{run}/rerun-failed-jobs"),
            json!({}),
        )
    }
    fn rerequest(&self, repo: &str, suite: u64) -> Result<(), ApiError> {
        self.flow_call(
            false,
            &format!("/repos/{repo}/check-suites/{suite}/rerequest"),
            json!({}),
        )
    }
    fn close(&self, repo: &str, pr: u64) -> Result<(), ApiError> {
        let _: Value = self.send(
            true,
            &format!("/repos/{repo}/pulls/{pr}"),
            &json!({"state":"closed"}),
        )?;
        Ok(())
    }
    fn owner(&self, repo: &str, pr: u64, label: &str) -> Result<(), ApiError> {
        if self.pull(repo, pr)?.labels.iter().any(|l| l.name == label) {
            return Ok(());
        }
        let _: Value = self.send(
            false,
            &format!("/repos/{repo}/issues/{pr}/labels"),
            &json!({"labels":[label]}),
        )?;
        Ok(())
    }
    fn disable_auto_merge(&self, repo: &str, pr: u64) -> Result<(), ApiError> {
        let pull: Value = self.get(&format!("/repos/{repo}/pulls/{pr}"))?;
        let id = pull["node_id"]
            .as_str()
            .ok_or_else(|| ApiError::new("PR lacks node ID"))?;
        self.mutation(
            repo,
            pr,
            "mutation($id:ID!){disablePullRequestAutoMerge(input:{pullRequestId:$id}){clientMutationId}}",
            json!({"id":id}),
        )
    }
}
