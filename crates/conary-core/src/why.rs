//! `why`: the change that landed a line (`docs/DESIGN.md` section 7).

use std::collections::HashSet;
use std::path::Path;

use crate::git::{self, GitError};
use crate::id::CommitId;
use crate::ledger::{Change, Landing, Ledger};
use crate::op::Repo;

/// What the ledger says about one line of a checkout.
pub struct Why<'a> {
    pub repo: Repo,
    /// The commit that last changed the line.
    pub commit: CommitId,
    /// The change whose landing brought `commit` onto trunk, and that
    /// landing, if the ledger holds one.
    pub landed: Option<(&'a Change, &'a Landing)>,
}

/// Why `line` (from 1) of `file` in the checkout at `dir` is the way it is.
/// The checkout can be trunk, a branch that merged trunk in, or any commit
/// that holds the landing.
pub fn why<'a>(
    ledger: &'a Ledger,
    dir: &Path,
    file: &Path,
    line: u32,
) -> Result<Why<'a>, GitError> {
    let repo = git::github_repo(dir)?;
    let commit = git::blame(dir, file, line)?;
    let after = git::descendants(dir, &commit)?;
    Ok(Why {
        landed: landing(ledger, &repo, &after),
        repo,
        commit,
    })
}

/// The landing that brought a commit onto trunk, given the commit and what
/// came after it (`after`): the one whose trunk after is among them and
/// whose trunk before isn't. A merge into a branch, or a local commit, has
/// no landing of its own and doesn't stand in for one.
fn landing<'a>(
    ledger: &'a Ledger,
    repo: &Repo,
    after: &HashSet<String>,
) -> Option<(&'a Change, &'a Landing)> {
    ledger
        .changes()
        .filter(|c| &c.repo == repo)
        .filter_map(|c| Some((c, c.landing.as_ref()?)))
        .find(|(_, l)| after.contains(l.after.as_str()) && !after.contains(l.before.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::id::{ChangeId, RevisionId};
    use crate::op::{Actor, ActorKind, Authority, Body};

    fn c(n: &str) -> CommitId {
        n.repeat(40 / n.len()).parse().unwrap()
    }

    fn set(commits: &[&str]) -> HashSet<String> {
        commits.iter().map(|n| c(n).to_string()).collect()
    }

    #[test]
    fn a_line_is_found_by_the_landing_that_brought_its_commit_onto_trunk() {
        let repo = Repo {
            owner: "FieldmouseWorks".into(),
            name: "burrow".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(
            crate::log::Lock::take(dir.path()).unwrap(),
            ed25519_dalek::SigningKey::from_bytes(&[4; 32]),
            None,
        )
        .unwrap();
        // Trunk: a, then 1 merged as m, then 2 rebased as r1 r2, then 3
        // squashed as s.
        let mut land = |pr, before: &str, after: &str| {
            let change = ChangeId::new().unwrap();
            for body in [
                Body::Observe {
                    change,
                    repo: repo.clone(),
                    pr,
                    author: Actor {
                        kind: ActorKind::Person,
                        login: "TusanHomichi".into(),
                        session: None,
                    },
                },
                Body::Revision {
                    change,
                    base: c(before),
                    head: c("f"),
                },
                Body::Land {
                    change,
                    revision: RevisionId::new(&c(before), &c("f")),
                    before: c(before),
                    after: c(after),
                    by: Authority::Status {
                        login: "steward".into(),
                    },
                },
            ] {
                store.record(body).unwrap();
            }
        };
        // The ledger can hold them in any order.
        land(3, "e2", "e3");
        land(2, "e0", "e2");
        land(1, "a", "e0");
        let ledger = store.ledger();
        let pr = |after: &[&str]| landing(ledger, &repo, &set(after)).map(|(c, _)| c.pr);

        // A commit of 1's, seen from a branch that merged trunk in later (b):
        // the landing is 1's, not the branch's merge.
        assert_eq!(pr(&["d", "e0", "e1", "e2", "e3", "b"]), Some(1));
        // The first of 2's rebased commits, and the squash itself.
        assert_eq!(pr(&["e1", "e2", "e3"]), Some(2));
        assert_eq!(pr(&["e3"]), Some(3));
        // A commit on a branch that hasn't landed.
        assert_eq!(pr(&["b"]), None);
    }
}
