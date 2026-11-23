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
    /// What Codex's GitHub connector posts of the PR's head at `at`: its
    /// summary comment, and for findings a review on the head whose receipt
    /// names it.
    pub(crate) fn flow_review(&self, repo: &str, pr: u64, verdict: crate::flow::Verdict, at: u64) {
        use crate::flow::Verdict;
        let head = self.pull_json(repo, pr)["head"]["sha"]
            .as_str()
            .unwrap()
            .to_owned();
        let when = jiff::Timestamp::from_second(at as i64).unwrap().to_string();
        let (status, findings) = match verdict {
            Verdict::Running => ("running", None),
            Verdict::Clean => ("completed", Some(false)),
            Verdict::Findings => ("completed", Some(true)),
        };
        if verdict == Verdict::Findings {
            let mut world = self.world.borrow_mut();
            let reviews = world
                .repos
                .get_mut(repo)
                .unwrap()
                .reviews
                .entry(pr)
                .or_default();
            let id = reviews.len() as u64 + 1;
            reviews.push(json!({"id":id,"user":fixtures::user(super::CONNECTOR,"Bot"),
                "state":"COMMENTED","commit_id":head,"submitted_at":when,
                "body":format!("<!-- codex-review-receipt:v1 -->\n\n### Codex Review\n\n**Reviewed commit:** `{}`\n", &head[..10.min(head.len())])}));
        }
        self.flow_summary(repo, pr, status, findings, at);
    }

    /// The connector's summary comment on the PR's head at `at`, its code
    /// review in `status` with `findings` as it says them.
    pub(crate) fn flow_summary(
        &self,
        repo: &str,
        pr: u64,
        status: &str,
        findings: Option<bool>,
        at: u64,
    ) {
        let head = self.pull_json(repo, pr)["head"]["sha"]
            .as_str()
            .unwrap()
            .to_owned();
        let when = jiff::Timestamp::from_second(at as i64).unwrap().to_string();
        let state = json!({"code_review":{"commit_sha":head,"has_any_findings":findings,"status":status},
            "security_review":{"commit_sha":null,"has_any_findings":null,"status":null}});
        let c = json!({"user":fixtures::user(super::CONNECTOR,"Bot"),
            "body":format!("<!-- codex-pull-request-review-summary -->\n<!-- codex-review-state:v1 {state} -->\n\nCodex Review Summary"),
            "created_at":when,"updated_at":when});
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn person_review(
        &self,
        repo: &str,
        pr: u64,
        who: &str,
        kind: &str,
        head: &str,
        body: &str,
        state: &str,
    ) {
        let mut world = self.world.borrow_mut();
        let reviews = world
            .repos
            .get_mut(repo)
            .unwrap()
            .reviews
            .entry(pr)
            .or_default();
        let id = reviews.len() as u64 + 1;
        reviews.push(
            json!({"id":id,"user":fixtures::user(who,kind),"state":state,
            "body":body,"commit_id":head,"submitted_at":"2026-11-01T00:00:00Z"}),
        );
    }
    /// Edit review `id`'s body, as its author can on GitHub.
    pub(crate) fn edit_review_body(&self, repo: &str, pr: u64, id: u64, body: &str) {
        let mut world = self.world.borrow_mut();
        let reviews = world
            .repos
            .get_mut(repo)
            .unwrap()
            .reviews
            .get_mut(&pr)
            .unwrap();
        let review = reviews.iter_mut().find(|r| r["id"] == id).unwrap();
        review["body"] = json!(body);
    }
    /// Everything the connector posted on the PR, gone.
    pub(crate) fn remove_flow_reviews(&self, repo: &str, pr: u64) {
        let mut world = self.world.borrow_mut();
        let r = world.repos.get_mut(repo).unwrap();
        r.reviews.remove(&pr);
        if let Some(comments) = r.comments.get_mut(&pr) {
            comments.retain(|c| c["user"]["login"] != super::CONNECTOR);
        }
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
    /// The connector's last findings review on the PR, its threads resolved.
    pub(crate) fn resolve_flow_findings(&self, repo: &str, pr: u64) {
        let id = self.world.borrow().repos[repo]
            .reviews
            .get(&pr)
            .unwrap()
            .iter()
            .rev()
            .find(|r| r["user"]["login"] == super::CONNECTOR)
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
    /// `head` was put on `branch` by `who`, an account of GitHub's type
    /// `kind` (`User`, or `Bot` for an App).
    pub(crate) fn pushed_by(&self, repo: &str, branch: &str, head: &str, who: &str, kind: &str) {
        self.world
            .borrow_mut()
            .pushes
            .entry((repo.into(), branch.into()))
            .or_default()
            .insert(
                head.into(),
                crate::api::User {
                    login: who.into(),
                    kind: kind.into(),
                },
            );
    }
    pub(crate) fn flow_thread(&self, repo: &str, pr: u64, id: &str, resolved: bool) {
        self.edit_flow(repo, pr, |s| {
            s.threads.entry(id.into()).or_default().resolved = resolved;
            s.open_threads = s.threads.values().filter(|t| !t.resolved).count() as u32;
        });
    }
    /// An open review thread `id` on `path`:`line`, opened by `author`
    /// saying `body`.
    pub(crate) fn flow_thread_at(
        &self,
        repo: &str,
        pr: u64,
        id: &str,
        (path, line): (&str, u64),
        (author, body): (&str, &str),
    ) {
        self.edit_flow(repo, pr, |s| {
            s.threads.insert(
                id.into(),
                crate::flow::ReviewThread {
                    resolved: false,
                    path: path.into(),
                    line: Some(line),
                    comments: vec![crate::flow::ThreadComment {
                        author: Some(author.into()),
                        body: body.into(),
                    }],
                },
            );
            s.open_threads = s.threads.values().filter(|t| !t.resolved).count() as u32;
        });
    }
    /// The comments on review thread `id` as GitHub shows them now, as
    /// `(author, body)`: a finding, its answers, or an edit to either.
    pub(crate) fn flow_thread_comments(
        &self,
        repo: &str,
        pr: u64,
        id: &str,
        comments: &[(&str, &str)],
    ) {
        self.edit_flow(repo, pr, |s| {
            s.threads.entry(id.into()).or_default().comments = comments
                .iter()
                .map(|(author, body)| crate::flow::ThreadComment {
                    author: Some((*author).into()),
                    body: (*body).into(),
                })
                .collect();
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

impl Hub {
    pub(crate) fn request_changes(&self, repo: &str, pr: u64, who: &str) {
        self.world
            .borrow_mut()
            .repos
            .get_mut(repo)
            .unwrap()
            .reviews
            .entry(pr)
            .or_default()
            .push(
                serde_json::json!({"user":{"login":who,"type":"User"},"state":"CHANGES_REQUESTED"}),
            );
    }
}

/// `comment`, whose author the world keeps by REST's login, as a snapshot
/// reads it: answered in GraphQL's form (an App by its slug alone, typed a
/// `Bot`) and read by the API's own reader.
pub(super) fn read_back(comment: &crate::flow::ThreadComment) -> crate::flow::ThreadComment {
    let author = comment.author.as_deref().map(|login| {
        let (kind, login) = login
            .strip_suffix("[bot]")
            .map_or(("User", login), |slug| ("Bot", slug));
        json!({"__typename": kind, "login": login})
    });
    let node: crate::api::ThreadNode = parse(&json!({
        "author": author,
        "body": comment.body,
        "pullRequestReview": null,
    }));
    node.into()
}
