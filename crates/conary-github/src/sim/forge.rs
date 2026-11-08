//! The simulated GitHub behind every trait the code talks to GitHub
//! through, answering as GitHub's REST API does.

use serde_json::{Value, json};

use conary_core::policy::Paths;

use crate::api::{
    ApiError, CheckRun, Commit, Comparison, Event, PullRequest, Pulls, Review, Source, Status,
    compared,
};
use crate::attest::{Actions, Artifact, Job, Key, testing};
use crate::branches::{Branches, Merged};
use crate::train::{CONTEXT, Forge, Mark, PrMerge};
use crate::undo::Reverts;

use super::faults::{Asked, Call};
use super::{FRESH, Hub, STEWARD, World, fixtures, parse};

fn not_found(what: impl std::fmt::Display) -> ApiError {
    ApiError::new(format!("{what}: Not Found"))
}

fn repo_of<'a>(w: &'a World, repo: &str) -> Result<&'a super::Repo, ApiError> {
    w.repos.get(repo).ok_or_else(|| not_found(repo))
}

impl Source for Hub {
    fn pull(&self, repo: &str, number: u64) -> Result<PullRequest, ApiError> {
        let n = number.to_string();
        self.read(Asked::new(Call::Pull, repo).pr(number), &[repo, &n], |w| {
            let p = repo_of(w, repo)?.pulls.get(&number);
            p.map(parse).ok_or_else(|| not_found(format!("#{number}")))
        })
    }

    fn pulls(&self, repo: &str, which: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        self.read(Asked::new(Call::Pulls, repo), &[repo], |w| {
            let all = repo_of(w, repo)?.pulls.values();
            Ok(match which {
                Pulls::All => all.map(parse).collect(),
                Pulls::Open => all.filter(|p| p["state"] == "open").map(parse).collect(),
                Pulls::RecentlyClosed => all
                    .filter(|p| p["state"] == "closed")
                    .rev()
                    .map(parse)
                    .collect(),
            })
        })
    }

    fn check_runs(&self, repo: &str, sha: &str) -> Result<Vec<CheckRun>, ApiError> {
        self.read(Asked::new(Call::CheckRuns, repo), &[repo, sha], |w| {
            Ok(w.checks.get(sha).into_iter().flatten().map(parse).collect())
        })
    }

    fn statuses(&self, repo: &str, sha: &str) -> Result<Vec<Status>, ApiError> {
        self.read(Asked::new(Call::Statuses, repo), &[repo, sha], |w| {
            Ok(w.statuses
                .get(sha)
                .into_iter()
                .flatten()
                .map(parse)
                .collect())
        })
    }

    fn reviews(&self, repo: &str, number: u64) -> Result<Vec<Review>, ApiError> {
        let n = number.to_string();
        self.read(
            Asked::new(Call::Reviews, repo).pr(number),
            &[repo, &n],
            |w| {
                let r = repo_of(w, repo)?.reviews.get(&number);
                Ok(r.into_iter().flatten().map(parse).collect())
            },
        )
    }

    fn commit(&self, repo: &str, sha: &str) -> Result<Commit, ApiError> {
        self.read(Asked::new(Call::Commit, repo), &[repo, sha], |w| {
            let c = w.commits.get(sha).ok_or_else(|| not_found(sha))?;
            let mut v = fixtures::commit();
            v["sha"] = json!(sha);
            v["parents"] = json!(
                c.parents
                    .iter()
                    .map(|p| json!({"sha": p}))
                    .collect::<Vec<_>>()
            );
            if !c.by_github {
                v["committer"] = fixtures::user("TusanHomichi", "User");
                v["commit"]["verification"]["verified"] = json!(false);
            }
            Ok(parse(&v))
        })
    }

    fn commit_pulls(&self, repo: &str, sha: &str) -> Result<Vec<u64>, ApiError> {
        self.read(Asked::new(Call::CommitPulls, repo), &[repo, sha], |w| {
            Ok(repo_of(w, repo)?
                .linked
                .get(sha)
                .cloned()
                .unwrap_or_default())
        })
    }

    fn timeline(&self, repo: &str, number: u64) -> Result<Vec<Event>, ApiError> {
        let n = number.to_string();
        self.read(
            Asked::new(Call::Timeline, repo).pr(number),
            &[repo, &n],
            |w| {
                let t = repo_of(w, repo)?.timeline.get(&number);
                Ok(t.into_iter().flatten().map(parse).collect())
            },
        )
    }

    fn merge_base(&self, repo: &str, a: &str, b: &str) -> Result<String, ApiError> {
        self.read(Asked::new(Call::MergeBase, repo), &[repo, a, b], |w| {
            w.merge_base(a, b)
                .ok_or_else(|| not_found(format!("{a}...{b}")))
        })
    }

    fn files(&self, repo: &str, base: &str, head: &str) -> Result<Paths, ApiError> {
        self.read(Asked::new(Call::Files, repo), &[repo, base, head], |w| {
            let fork = w
                .merge_base(base, head)
                .ok_or_else(|| not_found(format!("{base}...{head}")))?;
            let files = w.compare(base, head).unwrap_or_default();
            let mut v = fixtures::compare();
            v["merge_base_commit"]["sha"] = json!(fork);
            v["files"] = json!(
                files
                    .iter()
                    .map(|t| json!({
                        "filename": t.path,
                        "previous_filename": t.from,
                        "status": if t.new { "added" } else { "modified" },
                    }))
                    .collect::<Vec<_>>()
            );
            Ok(compared(parse::<Comparison>(&v).files))
        })
    }
}

/// A check run's job ran in the run of ten times its id, on a fresh
/// runner of its own unless a test says otherwise, and an artifact's id is
/// its run's and its place among them.
impl Actions for Hub {
    fn job(&self, repo: &str, job: u64) -> Result<Option<Job>, ApiError> {
        let n = job.to_string();
        self.read(Asked::new(Call::Job, repo), &[repo, &n], |w| {
            Ok(Some(Job {
                run_id: job * 10,
                runner_name: Some(
                    w.runners
                        .get(&job)
                        .cloned()
                        .unwrap_or_else(|| format!("{FRESH}{job}")),
                ),
            }))
        })
    }

    fn artifacts(&self, repo: &str, run: u64) -> Result<Vec<Artifact>, ApiError> {
        let n = run.to_string();
        self.read(Asked::new(Call::Artifacts, repo), &[repo, &n], |w| {
            Ok(w.artifacts
                .get(&(run / 10))
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, (name, _))| Artifact {
                    id: run * 10 + i as u64,
                    name: name.clone(),
                    expired: false,
                })
                .collect())
        })
    }

    fn artifact(&self, repo: &str, id: u64) -> Result<Vec<u8>, ApiError> {
        let n = id.to_string();
        self.read(Asked::new(Call::Artifact, repo), &[repo, &n], |w| {
            w.artifacts
                .get(&(id / 100))
                .and_then(|a| a.get((id % 100) as usize))
                .map(|a| a.1.clone())
                .ok_or_else(|| not_found(format!("artifact {id}")))
        })
    }

    fn keys(&self) -> Result<Vec<Key>, ApiError> {
        self.read(Asked::new(Call::Keys, ""), &[], |_| {
            Ok(vec![testing::key()])
        })
    }
}

impl Branches for Hub {
    fn default_branch(&self, repo: &str) -> Result<String, ApiError> {
        self.read(Asked::new(Call::DefaultBranch, repo), &[repo], |w| {
            Ok(repo_of(w, repo)?.default.clone())
        })
    }

    fn branch(&self, repo: &str, name: &str) -> Result<Option<String>, ApiError> {
        self.read(Asked::new(Call::Branch, repo), &[repo, name], |w| {
            Ok(repo_of(w, repo)?.branches.get(name).cloned())
        })
    }

    fn branches(&self, repo: &str, prefix: &str) -> Result<Vec<String>, ApiError> {
        self.read(Asked::new(Call::Branches, repo), &[repo, prefix], |w| {
            Ok(repo_of(w, repo)?
                .branches
                .keys()
                .filter(|n| n.starts_with(prefix))
                .cloned()
                .collect())
        })
    }

    fn create_branch(&self, repo: &str, name: &str, sha: &str) -> Result<(), ApiError> {
        self.write(
            Asked::new(Call::CreateBranch, repo),
            &[repo, name, sha],
            || {
                if self.branch_at(repo, name).is_some() {
                    return Err(ApiError::new(format!("{name}: Reference already exists")));
                }
                if !self.world.borrow().commits.contains_key(sha) {
                    return Err(ApiError::new(format!("{sha}: Object does not exist")));
                }
                self.set_branch(repo, name, sha);
                Ok(())
            },
        )
    }

    fn delete_branch(&self, repo: &str, name: &str) -> Result<(), ApiError> {
        self.write(Asked::new(Call::DeleteBranch, repo), &[repo, name], || {
            self.remove_branch(repo, name);
            Ok(())
        })
    }

    fn merge(&self, repo: &str, into: &str, head: &str, message: &str) -> Result<Merged, ApiError> {
        self.write(
            Asked::new(Call::Merge, repo),
            &[repo, into, head, message],
            || {
                if self.branch_at(repo, into).is_none() {
                    return Err(not_found(format!("branch {into}")));
                }
                Ok(match self.merge_into(repo, into, head) {
                    Ok(None) => Merged::Already,
                    Ok(Some(sha)) => Merged::Commit {
                        tree: self.tree_at(&sha),
                        sha,
                    },
                    Err(()) => Merged::Conflict,
                })
            },
        )
    }

    fn tree(&self, repo: &str, sha: &str) -> Result<String, ApiError> {
        self.read(Asked::new(Call::Tree, repo), &[repo, sha], |w| {
            let c = w.commits.get(sha).ok_or_else(|| not_found(sha))?;
            Ok(super::tree_of(&super::names(&c.patches)))
        })
    }
}

impl Reverts for Hub {
    fn make_commit(
        &self,
        repo: &str,
        tree: &str,
        parent: &str,
        message: &str,
    ) -> Result<String, ApiError> {
        self.write(
            Asked::new(Call::MakeCommit, repo),
            &[repo, tree, parent, message],
            || {
                let patches = self.world.borrow().trees.get(tree).cloned();
                let patches = patches.ok_or_else(|| not_found(format!("tree {tree}")))?;
                let sha = self.next_sha();
                self.commit_as(&sha, &[parent.into()], patches);
                self.world
                    .borrow_mut()
                    .made
                    .push((tree.into(), parent.into(), message.into()));
                Ok(sha)
            },
        )
    }

    fn open_pull(
        &self,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ApiError> {
        self.write(
            Asked::new(Call::OpenPull, repo),
            &[repo, head, base, title],
            || {
                let open = self.world.borrow().repos[repo]
                    .pulls
                    .values()
                    .any(|p| p["state"] == "open" && p["head"]["ref"] == head);
                if open {
                    return Err(ApiError::new("A pull request already exists"));
                }
                let sha = self
                    .branch_at(repo, head)
                    .ok_or_else(|| not_found(format!("branch {head}")))?;
                let number = self
                    .pull_numbers(repo)
                    .last()
                    .map_or(100, |n| (n + 1).max(100));
                self.pr(repo, number, &sha);
                self.world.borrow_mut().statuses.remove(&sha);
                self.edit_pull(repo, number, |p| {
                    p["title"] = json!(title);
                    p["body"] = json!(body);
                    p["user"] = fixtures::user(STEWARD, "Bot");
                    p["head"]["ref"] = json!(head);
                    p["base"]["ref"] = json!(base);
                });
                Ok(number)
            },
        )
    }

    fn pulls_from(&self, repo: &str, head: &str) -> Result<Vec<u64>, ApiError> {
        self.read(Asked::new(Call::PullsFrom, repo), &[repo, head], |w| {
            Ok(repo_of(w, repo)?
                .pulls
                .values()
                .filter(|p| p["head"]["ref"] == head)
                .filter_map(|p| p["number"].as_u64())
                .collect())
        })
    }
}

impl Forge for Hub {
    fn mark(
        &self,
        repo: &str,
        sha: &str,
        mark: Mark,
        description: &str,
        url: Option<&str>,
    ) -> Result<(), ApiError> {
        let asked = Asked::new(Call::Mark, repo).mark(mark);
        self.write(asked, &[repo, sha, description], || {
            let state = match mark {
                Mark::Landing => "success",
                Mark::Waiting => "pending",
                Mark::Red => "failure",
                Mark::Refused => "error",
            };
            let at = self.now();
            self.post_status(
                sha,
                super::built(
                    fixtures::status(),
                    &json!({"context": CONTEXT, "state": state, "description": description,
                            "target_url": url, "created_at": at,
                            "creator": fixtures::user(STEWARD, "Bot")}),
                ),
            );
            Ok(())
        })
    }

    fn land(&self, repo: &str, pr: u64, head: &str) -> Result<PrMerge, ApiError> {
        let n = pr.to_string();
        self.write(
            Asked::new(Call::Land, repo).pr(pr),
            &[repo, &n, head],
            || {
                let refused = self
                    .world
                    .borrow()
                    .refused
                    .get(&(repo.to_owned(), pr))
                    .cloned();
                if let Some(why) = refused {
                    return Ok(PrMerge::Refused(why));
                }
                let p: Value = self.pull_json(repo, pr);
                if p["head"]["sha"] != head {
                    return Ok(PrMerge::Refused("Head branch was modified".into()));
                }
                if p["state"] != "open" {
                    return Ok(PrMerge::Refused("Pull Request is not mergeable".into()));
                }
                Ok(match self.squash(repo, pr, head) {
                    Ok(_) => PrMerge::Merged,
                    Err(why) => PrMerge::Refused(why),
                })
            },
        )
    }
}

impl crate::flow::Read for Hub {
    fn snapshot(&self, repo: &str, number: u64) -> Result<crate::flow::Snapshot, ApiError> {
        let n = number.to_string();
        self.read(
            Asked::new(Call::FlowSnapshot, repo).pr(number),
            &[repo, &n],
            |w| {
                repo_of(w, repo)?
                    .flow
                    .get(&number)
                    .cloned()
                    .ok_or_else(|| not_found(format!("flow #{number}")))
            },
        )
    }
}

impl Hub {
    pub(crate) fn flow_snapshot(&self, repo: &str, number: u64, snapshot: crate::flow::Snapshot) {
        self.world
            .borrow_mut()
            .repos
            .get_mut(repo)
            .expect("repo exists")
            .flow
            .insert(number, snapshot);
    }
}
