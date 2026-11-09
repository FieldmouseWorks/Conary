//! The complete comparison's commits, not the PR endpoint's 250-commit list.
use super::*;

#[derive(Debug, Clone)]
pub struct History {
    pub commits: Vec<Commit>,
    /// GitHub may stop short of its own count. This is a permanent cut of
    /// the fixed commit range, distinct from a failed or inconsistent read.
    pub complete: bool,
}

#[derive(Deserialize)]
struct Page {
    merge_base_commit: Parent,
    total_commits: usize,
    commits: Vec<Commit>,
}

impl Api {
    pub(super) fn history(&self, repo: &str, base: &str, head: &str) -> Result<History, ApiError> {
        let mut commits = Vec::new();
        let mut identity = None;
        let mut seen = std::collections::BTreeSet::new();
        for page in 1..=MAX_PAGES {
            let got: Page = self.get(&format!(
                "/repos/{repo}/compare/{base}...{head}?per_page=100&page={page}"
            ))?;
            let scope = (got.merge_base_commit.sha, got.total_commits);
            if identity.as_ref().is_some_and(|i| i != &scope) {
                return Err(ApiError::new("comparison changed between pages"));
            }
            let more = got.commits.len() == 100;
            for c in got.commits {
                if !seen.insert(c.sha.clone()) {
                    return Err(ApiError::new("comparison repeated a commit"));
                }
                commits.push(c);
            }
            let count = scope.1;
            identity = Some(scope);
            if !more || commits.len() >= count {
                if commits.len() > count {
                    return Err(ApiError::new("comparison count is inconsistent"));
                }
                let complete = commits.len() == count;
                if count == 0 && identity.as_ref().is_none_or(|(fork, _)| fork != head) {
                    return Err(ApiError::new("empty comparison did not reach the PR head"));
                }
                if complete && count > 0 && !seen.contains(head) {
                    return Err(ApiError::new("comparison did not include the PR head"));
                }
                return Ok(History { commits, complete });
            }
        }
        Err(ApiError::new("comparison exceeded its page limit"))
    }
}
