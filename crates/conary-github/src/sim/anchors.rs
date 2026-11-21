//! Refs outside branches, as the ledger's anchor keeps them
//! ([`crate::anchor`]): each move a commit on the one before.

use crate::anchor::{Anchors, Kept};
use crate::api::ApiError;

use super::Hub;
use super::faults::{Asked, Call};
use super::forge::not_found;

impl Anchors for Hub {
    fn get(&self, repo: &str, name: &str) -> Result<Option<Kept>, ApiError> {
        self.read(Asked::new(Call::Ref, repo), &[repo, name], |w| {
            Ok(w.refs.get(&(repo.to_owned(), name.to_owned())).cloned())
        })
    }

    fn commit(&self, repo: &str, sha: &str) -> Result<Kept, ApiError> {
        self.read(Asked::new(Call::RefCommit, repo), &[repo, sha], |w| {
            w.ref_commits
                .get(sha)
                .cloned()
                .ok_or_else(|| not_found(format!("commit {sha}")))
        })
    }

    fn set(
        &self,
        repo: &str,
        name: &str,
        parent: Option<&str>,
        message: &str,
    ) -> Result<String, ApiError> {
        let at = parent.unwrap_or("none");
        self.write(Asked::new(Call::SetRef, repo), &[repo, name, at], || {
            let key = (repo.to_owned(), name.to_owned());
            let now = self.world.borrow().refs.get(&key).map(|k| k.commit.clone());
            if now.as_deref() != parent {
                return Err(ApiError::new(format!(
                    "422: {name} isn't at {at}: Reference cannot be updated"
                )));
            }
            let kept = Kept {
                commit: self.next_sha(),
                message: message.to_owned(),
                parents: parent.into_iter().map(str::to_owned).collect(),
            };
            let mut w = self.world.borrow_mut();
            w.ref_commits.insert(kept.commit.clone(), kept.clone());
            w.refs.insert(key, kept.clone());
            Ok(kept.commit)
        })
    }
}
