//! What the REST API writes: the ledger's anchor, the train's branches
//! and merges, undo's reverts, and each repository's notes.

use super::*;

#[derive(Debug, Deserialize)]
struct GitRef {
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitObject {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GitCommit {
    sha: String,
    message: String,
    parents: Vec<GitObject>,
}

impl crate::anchor::Anchors for Api {
    fn get(&self, repo: &str, name: &str) -> Result<Option<crate::anchor::Kept>, ApiError> {
        let short = name.strip_prefix("refs/").unwrap_or(name);
        let Some(r) = self.get_found::<GitRef>(&format!("/repos/{repo}/git/ref/{short}"))? else {
            return Ok(None);
        };
        crate::anchor::Anchors::commit(self, repo, &r.object.sha).map(Some)
    }

    fn commit(&self, repo: &str, sha: &str) -> Result<crate::anchor::Kept, ApiError> {
        let c: GitCommit = self.get(&format!("/repos/{repo}/git/commits/{sha}"))?;
        Ok(crate::anchor::Kept {
            commit: c.sha,
            message: c.message,
            parents: c.parents.into_iter().map(|p| p.sha).collect(),
        })
    }

    fn set(
        &self,
        repo: &str,
        name: &str,
        parent: Option<&str>,
        message: &str,
    ) -> Result<String, ApiError> {
        let tree: GitObject = self.send(
            false,
            &format!("/repos/{repo}/git/trees"),
            &serde_json::json!({"tree": [
                {"path": "anchor", "mode": "100644", "type": "blob", "content": message}
            ]}),
        )?;
        let commit: GitObject = self.send(
            false,
            &format!("/repos/{repo}/git/commits"),
            &serde_json::json!({"message": message, "tree": tree.sha, "parents": parent.into_iter().collect::<Vec<_>>()}),
        )?;
        self.move_ref(repo, name, parent.is_some(), &commit.sha)?;
        Ok(commit.sha)
    }
}

impl Api {
    /// Moves `name` to `commit`, a commit on the one it's at (`existing`),
    /// or makes it. A fast-forward, never a force, so a writer racing
    /// another, or a ref moved by hand, fails here.
    fn move_ref(
        &self,
        repo: &str,
        name: &str,
        existing: bool,
        commit: &str,
    ) -> Result<(), ApiError> {
        let _: serde_json::Value = if existing {
            self.send(
                true,
                &format!(
                    "/repos/{repo}/git/refs/{}",
                    name.strip_prefix("refs/").unwrap_or(name)
                ),
                &serde_json::json!({"sha": commit, "force": false}),
            )?
        } else {
            self.send(
                false,
                &format!("/repos/{repo}/git/refs"),
                &serde_json::json!({"ref": name, "sha": commit}),
            )?
        };
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct TreeCommit {
    tree: GitObject,
}

#[derive(Debug, Deserialize)]
struct NamedRef {
    #[serde(rename = "ref")]
    name: String,
}

/// The repo's default branch, as its own record says.
#[derive(Debug, Deserialize)]
struct Defaults {
    default_branch: String,
}

/// How a repo lets PRs be merged.
#[derive(Debug, Deserialize)]
struct MergeMethods {
    allow_squash_merge: Option<bool>,
    allow_merge_commit: Option<bool>,
    allow_rebase_merge: Option<bool>,
}

impl MergeMethods {
    /// The method the steward merges with too: squash, else a merge
    /// commit, else rebase. None when GitHub didn't say, for its default.
    fn first(&self) -> Option<&'static str> {
        [
            (self.allow_squash_merge, "squash"),
            (self.allow_merge_commit, "merge"),
            (self.allow_rebase_merge, "rebase"),
        ]
        .into_iter()
        .find_map(|(allowed, method)| (allowed == Some(true)).then_some(method))
    }
}

#[derive(Debug, Deserialize)]
struct Refusal {
    message: Option<String>,
}

/// What the merges API answers a merge with: the commit, and in it its tree.
#[derive(Debug, Deserialize)]
struct MergeCommit {
    sha: String,
    commit: TreeCommit,
}

impl crate::branches::Branches for Api {
    fn default_branch(&self, repo: &str) -> Result<String, ApiError> {
        let r: Defaults = self.get(&format!("/repos/{repo}"))?;
        Ok(r.default_branch)
    }

    fn branch(&self, repo: &str, name: &str) -> Result<Option<String>, ApiError> {
        Ok(self
            .get_found::<GitRef>(&format!("/repos/{repo}/git/ref/heads/{name}"))?
            .map(|r| r.object.sha))
    }

    fn branches(&self, repo: &str, prefix: &str) -> Result<Vec<String>, ApiError> {
        // One answer, every match: this endpoint doesn't page.
        let refs: Vec<NamedRef> =
            self.get(&format!("/repos/{repo}/git/matching-refs/heads/{prefix}"))?;
        Ok(refs
            .into_iter()
            .filter_map(|r| r.name.strip_prefix("refs/heads/").map(str::to_owned))
            .collect())
    }

    fn create_branch(&self, repo: &str, name: &str, sha: &str) -> Result<(), ApiError> {
        let _: serde_json::Value = self.send(
            false,
            &format!("/repos/{repo}/git/refs"),
            &serde_json::json!({"ref": format!("refs/heads/{name}"), "sha": sha}),
        )?;
        Ok(())
    }

    fn delete_branch(&self, repo: &str, name: &str) -> Result<(), ApiError> {
        let path = format!("/repos/{repo}/git/refs/heads/{name}");
        let url = format!("{}{path}", self.base);
        let mut response = self
            .agent
            .delete(&url)
            .header("Authorization", &self.authorization(&path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .map_err(|e| unreachable(&url, e))?;
        match response.status().as_u16() {
            // 422 is GitHub's answer for a ref that isn't there.
            200..=299 | 404 | 422 => Ok(()),
            _ => Err(failed(&url, &mut response)),
        }
    }

    fn merge(
        &self,
        repo: &str,
        into: &str,
        head: &str,
        message: &str,
    ) -> Result<crate::branches::Merged, ApiError> {
        use crate::branches::Merged;
        let path = format!("/repos/{repo}/merges");
        let url = format!("{}{path}", self.base);
        let mut response = self
            .agent
            .post(&url)
            .header("Authorization", &self.authorization(&path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(serde_json::json!({"base": into, "head": head, "commit_message": message}))
            .map_err(|e| unreachable(&url, e))?;
        match response.status().as_u16() {
            201 => {
                let made: MergeCommit = parse(&url, &read(&url, &mut response)?)?;
                Ok(Merged::Commit {
                    sha: made.sha,
                    tree: made.commit.tree.sha,
                })
            }
            204 => Ok(Merged::Already),
            409 => Ok(Merged::Conflict),
            _ => Err(failed(&url, &mut response)),
        }
    }

    fn tree(&self, repo: &str, sha: &str) -> Result<String, ApiError> {
        let commit: TreeCommit = self.get(&format!("/repos/{repo}/git/commits/{sha}"))?;
        Ok(commit.tree.sha)
    }
}

impl crate::train::Forge for Api {
    fn mark(
        &self,
        repo: &str,
        sha: &str,
        mark: crate::train::Mark,
        description: &str,
        url: Option<&str>,
    ) -> Result<(), ApiError> {
        use crate::train::{CONTEXT, Mark};
        let state = match mark {
            Mark::Landing => "success",
            Mark::Waiting => "pending",
            Mark::Red => "failure",
            Mark::Refused => "error",
        };
        // GitHub takes a description of up to 140 characters.
        let description: String = description.chars().take(140).collect();
        let mut body =
            serde_json::json!({"state": state, "context": CONTEXT, "description": description});
        if let Some(url) = url {
            body["target_url"] = serde_json::json!(url);
        }
        let _: serde_json::Value =
            self.send(false, &format!("/repos/{repo}/statuses/{sha}"), &body)?;
        Ok(())
    }

    fn land(&self, repo: &str, pr: u64, head: &str) -> Result<crate::train::PrMerge, ApiError> {
        use crate::train::PrMerge;
        let methods: MergeMethods = self.get(&format!("/repos/{repo}"))?;
        // The head that was checked, or nothing: a push since makes the sha
        // stale, and GitHub refuses it.
        let mut body = serde_json::json!({"sha": head});
        if let Some(method) = methods.first() {
            body["merge_method"] = serde_json::json!(method);
        }
        let path = format!("/repos/{repo}/pulls/{pr}/merge");
        let url = format!("{}{path}", self.base);
        let mut response = self
            .agent
            .put(&url)
            .header("Authorization", &self.authorization(&path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body)
            .map_err(|e| unreachable(&url, e))?;
        match response.status().as_u16() {
            200 => Ok(PrMerge::Merged),
            // Not mergeable, a rule it doesn't meet, or the head moved.
            status @ (405 | 409 | 422) => {
                let said: Refusal =
                    parse(&url, &read(&url, &mut response)?).unwrap_or(Refusal { message: None });
                Ok(PrMerge::Refused(
                    said.message
                        .unwrap_or_else(|| format!("http status {status}")),
                ))
            }
            _ => Err(failed(&url, &mut response)),
        }
    }
}

impl crate::undo::Reverts for Api {
    fn make_commit(
        &self,
        repo: &str,
        tree: &str,
        parent: &str,
        message: &str,
    ) -> Result<String, ApiError> {
        let made: GitObject = self.send(
            false,
            &format!("/repos/{repo}/git/commits"),
            &serde_json::json!({"message": message, "tree": tree, "parents": [parent]}),
        )?;
        Ok(made.sha)
    }

    fn open_pull(
        &self,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ApiError> {
        let made: PullNumber = self.send(
            false,
            &format!("/repos/{repo}/pulls"),
            &serde_json::json!({"title": title, "head": head, "base": base, "body": body}),
        )?;
        Ok(made.number)
    }

    fn pulls_from(&self, repo: &str, head: &str) -> Result<Vec<u64>, ApiError> {
        // `head` filters by OWNER:BRANCH, so a fork's branch of the same
        // name is never taken for the repo's own. It matches a PR's head as
        // it was opened, so a merged PR's deleted branch still finds it.
        let owner = repo.split('/').next().unwrap_or(repo);
        let found: Vec<PullNumber> = self.pages(&format!(
            "/repos/{repo}/pulls?state=all&head={owner}:{head}&sort=created&direction=asc"
        ))?;
        Ok(found.into_iter().map(|p| p.number).collect())
    }
}

#[derive(Debug, Deserialize)]
struct Tree {
    tree: Vec<TreeEntry>,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

impl crate::notes::Notes for Api {
    fn get(&self, repo: &str, name: &str) -> Result<Option<crate::notes::Kept>, ApiError> {
        let short = name.strip_prefix("refs/").unwrap_or(name);
        let Some(r) = self.get_found::<GitRef>(&format!("/repos/{repo}/git/ref/{short}"))? else {
            return Ok(None);
        };
        let commit: TreeCommit =
            self.get(&format!("/repos/{repo}/git/commits/{}", r.object.sha))?;
        let tree: Tree = self.get(&format!(
            "/repos/{repo}/git/trees/{}?recursive=1",
            commit.tree.sha
        ))?;
        if tree.truncated {
            return Err(ApiError::new(format!(
                "{repo}'s {name} holds more notes than one read of its tree returns"
            )));
        }
        let notes = tree
            .tree
            .into_iter()
            .filter(|e| e.kind == "blob")
            .filter_map(|e| {
                // A note's path is the commit it's on, maybe fanned out.
                let on: String = e.path.chars().filter(|&c| c != '/').collect();
                let commit =
                    matches!(on.len(), 40 | 64) && on.bytes().all(|b| b.is_ascii_hexdigit());
                commit.then_some((on, (e.path, e.sha)))
            })
            .collect();
        Ok(Some(crate::notes::Kept {
            commit: r.object.sha,
            tree: commit.tree.sha,
            notes,
        }))
    }

    fn set(
        &self,
        repo: &str,
        name: &str,
        parent: Option<&crate::notes::Kept>,
        files: &[(String, String)],
    ) -> Result<(), ApiError> {
        let entries: Vec<_> = files
            .iter()
            .map(|(path, text)| {
                serde_json::json!({"path": path, "mode": "100644", "type": "blob", "content": text})
            })
            .collect();
        let mut tree = serde_json::json!({"tree": entries});
        if let Some(p) = parent {
            tree["base_tree"] = serde_json::json!(p.tree);
        }
        let tree: GitObject = self.send(false, &format!("/repos/{repo}/git/trees"), &tree)?;
        let message = match files.len() {
            1 => "conary: one landing's note".to_owned(),
            n => format!("conary: {n} landings' notes"),
        };
        let commit: GitObject = self.send(
            false,
            &format!("/repos/{repo}/git/commits"),
            &serde_json::json!({
                "message": message,
                "tree": tree.sha,
                "parents": parent.map(|p| &p.commit).into_iter().collect::<Vec<_>>(),
            }),
        )?;
        self.move_ref(repo, name, parent.is_some(), &commit.sha)
    }
}
