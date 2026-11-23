//! [`Actions`] on the REST API: a job, a run's artifacts, an
//! artifact's zip, and the keys GitHub signs its Actions OIDC tokens with.

use serde::Deserialize;

use super::{Api, ApiError, PER_PAGE, Page, failed, paged, parse, read, unreachable};
use crate::attest::{Actions, Artifact, Job, KEYS, Key};

#[derive(Deserialize)]
struct Artifacts {
    artifacts: Vec<Artifact>,
}

#[derive(Deserialize)]
struct Keys {
    keys: Vec<Key>,
}

impl Actions for Api {
    fn job(&self, repo: &str, job: u64) -> Result<Option<Job>, ApiError> {
        self.get_found(&format!("/repos/{repo}/actions/jobs/{job}"))
    }

    fn artifacts(&self, repo: &str, run: u64) -> Result<Vec<Artifact>, ApiError> {
        paged(
            &format!("run {run}'s artifacts"),
            self.read_for,
            |page, left| {
                let got: Artifacts = self.get_within(
                    &format!("/repos/{repo}/actions/runs/{run}/artifacts?per_page=100&page={page}"),
                    left,
                )?;
                let more = got.artifacts.len() == PER_PAGE;
                Ok(Page {
                    items: got.artifacts,
                    more,
                })
            },
        )
    }

    /// GitHub answers with a redirect to the zip, a minute's signed URL on
    /// its storage, which the token isn't sent to.
    fn artifact(&self, repo: &str, id: u64) -> Result<Vec<u8>, ApiError> {
        let path = format!("/repos/{repo}/actions/artifacts/{id}/zip");
        let url = format!("{}{path}", self.base);
        let mut response = self
            .agent
            .get(&url)
            .header("Authorization", &self.authorization(&path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .map_err(|e| unreachable(&url, e))?;
        if !response.status().is_success() {
            return Err(failed(&url, &mut response));
        }
        read(&url, &mut response)
    }

    /// Public, so asked for with no token.
    fn keys(&self) -> Result<Vec<Key>, ApiError> {
        let mut response = self
            .agent
            .get(KEYS)
            .call()
            .map_err(|e| unreachable(KEYS, e))?;
        if !response.status().is_success() {
            return Err(failed(KEYS, &mut response));
        }
        Ok(parse::<Keys>(KEYS, &read(KEYS, &mut response)?)?.keys)
    }
}
