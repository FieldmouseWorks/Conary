//! GitHub's read-only test merges and commit provenance. Preview commits are
//! derived from the model, without moving a branch or recording a merge.
use super::{Hub, World, fixtures, names, parse, tree_of};
use crate::api::{ApiError, Commit};
use serde_json::{Value, json};
use std::collections::BTreeSet;

impl World {
    pub(super) fn preview(
        &self,
        repo: &str,
        number: u64,
    ) -> Option<(String, Vec<String>, BTreeSet<String>)> {
        let r = self.repos.get(repo)?;
        let p = r.pulls.get(&number)?;
        let base = r.branches.get(p["base"]["ref"].as_str()?)?;
        let head = p["head"]["sha"].as_str()?;
        let patches = self.three_way(base, head)?;
        let sha = tree_of(&[base.as_str(), head, &tree_of(&names(&patches))]);
        Some((sha, vec![base.clone(), head.into()], patches))
    }
    pub(super) fn preview_for(
        &self,
        repo: &str,
        sha: &str,
    ) -> Option<(Vec<String>, BTreeSet<String>)> {
        for n in self.repos.get(repo)?.pulls.keys() {
            if let Some((got, parents, patches)) = self.preview(repo, *n)
                && got == sha
            {
                return Some((parents, patches));
            }
        }
        None
    }
    pub(super) fn commit_record(&self, repo: &str, sha: &str) -> Result<Commit, ApiError> {
        let (parents, github) = if let Some(c) = self.commits.get(sha) {
            (c.parents.clone(), c.by_github)
        } else if let Some((parents, _)) = self.preview_for(repo, sha) {
            (parents, true)
        } else {
            return Err(ApiError::new(format!("commit {sha}: Not Found")));
        };
        let mut v = self
            .commit_records
            .get(sha)
            .cloned()
            .unwrap_or_else(fixtures::commit);
        v["sha"] = json!(sha);
        v["parents"] = json!(parents.iter().map(|p| json!({"sha":p})).collect::<Vec<_>>());
        if !github {
            v["committer"] = fixtures::user("TusanHomichi", "User");
            v["commit"]["verification"]["verified"] = json!(false);
        }
        Ok(parse(&v))
    }
}
impl Hub {
    pub(crate) fn commit_details(
        &self,
        sha: &str,
        message: Option<&str>,
        author: Option<&str>,
        committer: Option<&str>,
    ) {
        let mut v = fixtures::commit();
        v["commit"]["message"] = json!(message);
        v["commit"]["author"]["email"] = json!(author);
        v["commit"]["committer"]["email"] = json!(committer);
        self.world.borrow_mut().commit_records.insert(sha.into(), v);
    }
    pub(crate) fn commit_response(&self, sha: &str, value: Value) {
        self.world
            .borrow_mut()
            .commit_records
            .insert(sha.into(), value);
    }
}

impl Hub {
    pub(crate) fn edit_flow(
        &self,
        repo: &str,
        pr: u64,
        edit: impl FnOnce(&mut crate::flow::Snapshot),
    ) {
        edit(
            self.world
                .borrow_mut()
                .repos
                .get_mut(repo)
                .unwrap()
                .flow
                .get_mut(&pr)
                .unwrap(),
        );
    }
    /// Read the PR's new projection after an intentional edit, distinct from
    /// a stale endpoint scripted by leaving the old projection in place.
    pub(crate) fn refresh_flow(&self, repo: &str, pr: u64) {
        let revision =
            crate::flow::Revision::from(&crate::api::Source::pull(self, repo, pr).unwrap());
        self.edit_flow(repo, pr, |s| {
            s.revision = revision.clone();
        });
    }
}

impl Hub {
    pub(crate) fn flow_review(
        &self,
        repo: &str,
        pr: u64,
        reviewer: &str,
        verdict: crate::flow::Verdict,
        at: u64,
    ) {
        let p = self.pull_json(repo, pr);
        let scope = crate::flow::Revision::from(&crate::api::Source::pull(self, repo, pr).unwrap());
        let mark = json!({"commit_sha":p["head"]["sha"],"base":p["base"]["ref"],"findings":u32::from(verdict==crate::flow::Verdict::Findings),"scope":scope.description,"reviewed_at":at});
        let id = self.world.borrow().repos[repo]
            .reviews
            .get(&pr)
            .map_or(1, |r| r.len() as u64 + 1);
        let review = json!({"id":id,"user":fixtures::user(super::STEWARD,"Bot"),"body":format!("<!-- {reviewer}-review:v2 {mark} -->"),"commit_id":p["head"]["sha"],"submitted_at":chrono::DateTime::from_timestamp(at as i64,0).unwrap().to_rfc3339()});
        self.world
            .borrow_mut()
            .repos
            .get_mut(repo)
            .unwrap()
            .reviews
            .entry(pr)
            .or_default()
            .push(review);
    }
    pub(crate) fn remove_flow_reviews(&self, repo: &str, pr: u64) {
        self.world
            .borrow_mut()
            .repos
            .get_mut(repo)
            .unwrap()
            .reviews
            .remove(&pr);
    }
}

impl Hub {
    pub(crate) fn flow_comment(&self, repo: &str, pr: u64, who: &str, body: &str) {
        let c = json!({"user":fixtures::user(who,if who.ends_with("[bot]"){"Bot"}else{"User"}),"body":body,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"});
        self.world
            .borrow_mut()
            .repos
            .get_mut(repo)
            .unwrap()
            .comments
            .entry(pr)
            .or_default()
            .push(c);
    }
}

impl Hub {
    pub(crate) fn flow_diff(&self, base: &str, head: &str, diff: &str) {
        self.world
            .borrow_mut()
            .diffs
            .insert((base.into(), head.into()), diff.into());
    }
}

impl Hub {
    pub(crate) fn resolve_flow_findings(&self, repo: &str, pr: u64, role: &str) {
        let id = self.world.borrow().repos[repo]
            .reviews
            .get(&pr)
            .unwrap()
            .iter()
            .rev()
            .find(|r| {
                r["body"]
                    .as_str()
                    .is_some_and(|b| b.contains(&format!("<!-- {role}-review:v2 ")))
            })
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        self.edit_flow(repo, pr, |s| {
            s.resolved_reviews.insert(id);
            s.open_threads = 0;
        });
    }
}

impl Hub {
    pub(crate) fn pushed_by(&self, repo: &str, branch: &str, head: &str, who: &str) {
        self.world
            .borrow_mut()
            .pushes
            .entry((repo.into(), branch.into()))
            .or_default()
            .insert(head.into(), who.into());
    }
    pub(crate) fn flow_thread(&self, repo: &str, pr: u64, id: &str, resolved: bool) {
        self.edit_flow(repo, pr, |s| {
            s.threads.insert(id.into(), resolved);
            s.open_threads = s.threads.values().filter(|resolved| !**resolved).count() as u32;
        });
    }
}

impl Hub {
    pub(crate) fn short_history(&self, base: &str, head: &str, shown: usize) {
        self.world
            .borrow_mut()
            .history_limits
            .insert((base.into(), head.into()), shown);
    }
}

impl Hub {
    pub(crate) fn renamed_path(&self, patch: &str, from: &str, to: &str) {
        self.world.borrow_mut().touched.insert(
            patch.into(),
            vec![conary_core::policy::Touched {
                path: to.into(),
                from: Some(from.into()),
                new: true,
            }],
        );
    }
}

impl World {
    pub(super) fn base_observation(&self, repo: &str) -> Option<crate::flow::BaseObservation> {
        use crate::flow::Ci;
        let r = self.repos.get(repo)?;
        let head = r.branches.get(&r.default)?;
        let mut states = Vec::new();
        for c in self.checks.get(head).into_iter().flatten() {
            states.push(match (c["status"].as_str(), c["conclusion"].as_str()) {
                (_, Some("failure")) => Ci::Red,
                (Some("completed"), Some("success" | "neutral" | "skipped")) => Ci::Green,
                _ => Ci::Pending,
            });
        }
        for s in self.statuses.get(head).into_iter().flatten() {
            states.push(match s["state"].as_str() {
                Some("failure") => Ci::Red,
                Some("success") => Ci::Green,
                _ => Ci::Pending,
            });
        }
        let ci = if states.contains(&Ci::Red) {
            Ci::Red
        } else if states.is_empty() || states.contains(&Ci::Pending) {
            Ci::Pending
        } else {
            Ci::Green
        };
        Some(crate::flow::BaseObservation {
            name: r.default.clone(),
            head: head.clone(),
            ci,
        })
    }
}

impl Hub {
    pub(crate) fn edit_flow_comment(&self, repo: &str, pr: u64, index: usize, body: &str) {
        let mut world = self.world.borrow_mut();
        let comment = &mut world
            .repos
            .get_mut(repo)
            .unwrap()
            .comments
            .get_mut(&pr)
            .unwrap()[index];
        comment["body"] = json!(body);
        comment["updated_at"] = json!("2030-01-01T00:00:00Z");
    }
}
