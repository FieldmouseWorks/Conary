//! Effects for an explicitly requested flow pass.
use super::*;
use crate::flow::{Stage, Write};
use serde_json::{Value, json};

impl Api {
    fn flow_put(&self, path: &str, body: Value) -> Result<(), ApiError> {
        let url = format!("{}{path}", self.base);
        let mut response = self
            .agent
            .put(&url)
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
        let stages = [
            Stage::Review,
            Stage::Reviewer,
            Stage::Fix,
            Stage::Ci,
            Stage::ProofSlot,
            Stage::Proof,
            Stage::ReleaseAge,
            Stage::Train,
            Stage::Merge,
            Stage::Owner,
            Stage::Base,
        ];
        for label in pull.labels {
            if stages
                .iter()
                .any(|s| s.label() == Some(label.name.as_str()))
                && stage.label() != Some(label.name.as_str())
            {
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

    fn review(&self, repo: &str, pr: u64, head: &str, body: &str) -> Result<(), ApiError> {
        let _: Value = self.send(
            false,
            &format!("/repos/{repo}/pulls/{pr}/reviews"),
            &json!({"commit_id":head,"body":body,"event":"COMMENT"}),
        )?;
        Ok(())
    }
    fn update(&self, repo: &str, pr: u64, head: &str) -> Result<(), ApiError> {
        self.flow_put(
            &format!("/repos/{repo}/pulls/{pr}/update-branch"),
            json!({"expected_head_sha":head}),
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
        let path = format!("/repos/{repo}/pulls/{pr}");
        let url = format!("{}/graphql", self.base);
        let mut response = self.agent.post(&url).header("Authorization", &self.authorization(&path)?)
            .send_json(json!({"query":"mutation($id:ID!){disablePullRequestAutoMerge(input:{pullRequestId:$id}){clientMutationId}}","variables":{"id":id}}))
            .map_err(|e| unreachable(&url,e))?;
        if !response.status().is_success() {
            return Err(failed(&url, &mut response));
        }
        let result: Value = parse(&url, &read(&url, &mut response)?)?;
        if result.get("errors").is_some() {
            return Err(ApiError::new("GitHub refused disabling auto merge"));
        }
        Ok(())
    }
}
