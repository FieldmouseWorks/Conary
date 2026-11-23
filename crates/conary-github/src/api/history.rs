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
        let mut identity: Option<(String, usize)> = None;
        let mut seen = std::collections::BTreeSet::new();
        let path = format!("/repos/{repo}/compare/{base}...{head}");
        let commits = paged(&path, self.read_for, |page, left| {
            let got: Page = self.get_within(&format!("{path}?per_page=100&page={page}"), left)?;
            let scope = (got.merge_base_commit.sha, got.total_commits);
            if identity.as_ref().is_some_and(|i| i != &scope) {
                return Err(ApiError::new("comparison changed between pages"));
            }
            for c in &got.commits {
                if !seen.insert(c.sha.clone()) {
                    return Err(ApiError::new("comparison repeated a commit"));
                }
            }
            let more = got.commits.len() == PER_PAGE && seen.len() < scope.1;
            identity = Some(scope);
            Ok(super::Page {
                items: got.commits,
                more,
            })
        })?;
        let (fork, count) = identity.expect("a paged read reads its first page");
        if commits.len() > count {
            return Err(ApiError::new("comparison count is inconsistent"));
        }
        let complete = commits.len() == count;
        if count == 0 && fork != head {
            return Err(ApiError::new("empty comparison did not reach the PR head"));
        }
        if complete && count > 0 && !seen.contains(head) {
            return Err(ApiError::new("comparison did not include the PR head"));
        }
        Ok(History { commits, complete })
    }
}
