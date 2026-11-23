//! Whether a failure on the negative tree is the new tests', or the tree's
//! own, made by taking away a test file the old code names (#569). The
//! negative tree is what's signed and stays as it is; the trees here are
//! only to judge a failure on it by, by the rule burrow's flake workflow
//! keeps (its `crates/burrow-flake-check/README.md`, "A failure the
//! negative tree made"), so the two signers judge a move alike.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use super::{Result, TESTS, git};

/// A test file's change, from the merge base to the head.
#[derive(Clone)]
pub(super) struct Change {
    was_mode: String,
    was_id: String,
    mode: String,
    id: String,
    deletes: bool,
    added: bool,
    pub(super) path: String,
}

/// The test files' changes, renames as the path left and the path made.
pub(super) fn changes(repo: &Path, base: &str, head: &str) -> Result<Vec<Change>> {
    let mut args = vec![
        "diff-tree",
        "--no-commit-id",
        "-r",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--no-abbrev",
        "--raw",
        "-z",
        "--diff-filter=ACDMT",
        base,
        head,
        "--",
    ];
    args.extend_from_slice(TESTS);
    let out = git(repo, &args)?;
    let mut fields = out.split('\0');
    let mut changes = vec![];
    while let Some(meta) = fields.next().filter(|m| !m.is_empty()) {
        let path = fields.next().ok_or("incomplete raw diff")?;
        let meta: Vec<_> = meta.trim_start_matches(':').split(' ').collect();
        let [was_mode, mode, was_id, id, status] = meta[..] else {
            return Err("bad raw diff".into());
        };
        changes.push(Change {
            was_mode: was_mode.into(),
            was_id: was_id.into(),
            mode: mode.into(),
            id: id.into(),
            deletes: status == "D",
            added: status == "A",
            path: path.into(),
        });
    }
    Ok(changes)
}

impl Change {
    /// Whether this added file takes a deleted one's place, so that old code
    /// naming that one may find this instead: a directory where the file
    /// was, or the other spelling of a Rust module or target (`x.rs` for
    /// `x/mod.rs` or `x/main.rs`). Whether it does is for a run to say.
    fn replaces(&self, gone: &str) -> bool {
        self.within(gone)
            || (self.added
                && matches!((module(&self.path), module(gone)), (Some(a), Some(b)) if a == b))
    }
    /// Whether this added file made a deleted one a directory, so that no
    /// tree holds both.
    fn within(&self, gone: &str) -> bool {
        self.added && Path::new(&self.path).starts_with(gone)
    }
    /// A deleted file's old content, where the file that took its place is.
    fn standing_in(&self, gone: &Change) -> Change {
        Change {
            mode: gone.was_mode.clone(),
            id: gone.was_id.clone(),
            deletes: false,
            ..self.clone()
        }
    }
}

fn module(path: &str) -> Option<&str> {
    let p = path.strip_suffix(".rs")?;
    Some(
        p.strip_suffix("/mod")
            .or_else(|| p.strip_suffix("/main"))
            .unwrap_or(p),
    )
}

/// The base's tree with those changes made, in an index of its own: git
/// alone, so no candidate filter, hook or checkout conversion runs.
pub(super) fn tree_with(repo: &Path, base: &str, changes: &[&Change]) -> Result<String> {
    let temporary = tempfile::tempdir()?;
    let index = temporary.path().join("index");
    let git_index = |args: &[&str], input: Option<&[u8]>| -> Result<String> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .env("GIT_INDEX_FILE", &index)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            use std::io::Write;
            stdin.write_all(input)?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(format!("negative index: git {} failed", args[0]).into());
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    };
    git_index(&["read-tree", base], None)?;
    // The removals first: a file can be in its directory's way. Mode 0
    // removes; the rest go in verbatim, never through a filter.
    let mut entries = String::new();
    for c in changes.iter().filter(|c| c.deletes) {
        entries += &format!("0 {}\t{}\0", "0".repeat(c.was_id.len()), c.path);
    }
    for c in changes.iter().filter(|c| !c.deletes) {
        entries += &format!("{} {}\t{}\0", c.mode, c.id, c.path);
    }
    git_index(
        &["update-index", "-z", "--index-info"],
        Some(entries.as_bytes()),
    )?;
    git_index(&["write-tree"], None)
}

/// The trees to judge a failure on the negative tree by, where the PR
/// deletes a test file. None of them is ever signed.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Judging {
    /// The merge base's tree: no new test is in it.
    pub base: String,
    /// The removal alone: the base with the deleted test files away, and
    /// their old content where the PR replaced one.
    pub removed: String,
    /// Where the removal alone fails too, the trees to try in turn.
    pub restored: Vec<Trial>,
}

/// One way of putting the deleted test files back beside the new tests.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Trial {
    /// The base with its test files at `tree`'s paths and only old content
    /// in them. Only where it passes is a failure on `tree` more than a
    /// file's absence, or two copies' clash.
    pub control: String,
    /// The new tests, with the deleted files put back: a replaced one stays
    /// away first (old code finds what replaced it), then only one that
    /// became a directory does (a `#[path]` names the old one).
    pub tree: String,
}

/// Which of the deleted files a tree leaves away.
#[derive(Clone, Copy)]
enum Away {
    /// All, as the negative tree does.
    Deleted,
    /// Those the PR replaced.
    Replaced,
    /// Those that became directories: the fewest a tree can.
    Directories,
}

pub(super) fn judging(repo: &Path, base: &str, changes: &[Change]) -> Result<Option<Judging>> {
    let deleted: Vec<_> = changes.iter().filter(|c| c.deletes).collect();
    if deleted.is_empty() {
        return Ok(None);
    }
    let away = |away: Away| -> Vec<&Change> {
        deleted
            .iter()
            .copied()
            .filter(|gone| match away {
                Away::Deleted => true,
                Away::Replaced => changes.iter().any(|a| a.replaces(&gone.path)),
                Away::Directories => changes.iter().any(|a| a.within(&gone.path)),
            })
            .collect()
    };
    // Every deleted file's old content where something replaced it.
    let standing: Vec<Change> = deleted
        .iter()
        .flat_map(|gone| {
            changes
                .iter()
                .filter(|a| a.replaces(&gone.path))
                .map(|a| a.standing_in(gone))
        })
        .collect();
    let control = |away: &[&Change]| {
        let changes: Vec<&Change> = away.iter().copied().chain(&standing).collect();
        tree_with(repo, base, &changes)
    };
    let removed = control(&away(Away::Deleted))?;
    let mut restored: Vec<Trial> = vec![];
    for kind in [Away::Replaced, Away::Directories] {
        let left = away(kind);
        // All of them away is the removal, which is already judged.
        if left.len() == deleted.len() {
            continue;
        }
        let applied: Vec<&Change> = changes
            .iter()
            .filter(|c| !c.deletes || left.iter().any(|l| l.path == c.path))
            .collect();
        let trial = Trial {
            control: control(&left)?,
            tree: tree_with(repo, base, &applied)?,
        };
        if !restored.contains(&trial) {
            restored.push(trial);
        }
    }
    Ok(Some(Judging {
        base: git(repo, &["rev-parse", &format!("{base}^{{tree}}")])?
            .trim()
            .to_owned(),
        removed,
        restored,
    }))
}

/// What the negative-command did on the negative tree, judged.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// It passed: the new tests pass on the old code.
    Pass,
    /// It failed, and the new tests are what fail it.
    Fail,
    /// It failed where the tree's removal of a test file can be the cause:
    /// nothing is attested, and this says why.
    Unsure(&'static str),
}

const TREE_MADE: &str = "the command fails with the test files the old code names taken away, and not with them put back: the tree made that failure, not the new tests. A new test has to fail where the old code runs it: one in a file that moved isn't run there, so leave that file where it was, or fail a test in a file that stayed";
const NEITHER: &str = "the command isn't seen to pass on the base with a test file the PR replaced away, nor with it beside what replaced it: a failure can be the tree's, not the new tests'. Leave that file where it was";

/// What the old code alone says of the deleted test files, from the trees
/// that hold no new test.
enum Old<'a> {
    /// No test file was deleted.
    Untouched,
    /// The base passes with them taken away.
    PassesWithout,
    /// It passes with them back this way, beside what replaced them.
    PassesBeside(&'a Judging, &'a Trial),
    /// It passes no way the trees try.
    Neither,
}

/// Judge the negative tree by running the command on it and, where the PR
/// deletes test files, on the judging trees, each at most once. `run` says
/// whether the command passed on a tree.
///
/// The trees of old code alone run first, as far as a failure on the
/// negative tree would need them: when they run, no new test has run on
/// the machine, so nothing one leaves behind (a process, a file) can make
/// one pass. Every tree run after the negative tree holds all the new tests
/// it does, so what's left behind there the tests could do themselves.
pub fn judge(
    negative: &str,
    judging: Option<&Judging>,
    mut run: impl FnMut(&str) -> Result<bool>,
) -> Result<Outcome> {
    let mut seen = BTreeMap::new();
    let mut passes = |tree: &str| -> Result<bool> {
        if let Some(&passed) = seen.get(tree) {
            return Ok(passed);
        }
        let passed = run(tree)?;
        seen.insert(tree.to_owned(), passed);
        Ok(passed)
    };
    let old = match judging {
        None => Old::Untouched,
        Some(j) if passes(&j.removed)? => Old::PassesWithout,
        Some(j) => {
            let mut old = Old::Neither;
            for t in &j.restored {
                if passes(&t.control)? {
                    old = Old::PassesBeside(j, t);
                    break;
                }
            }
            old
        }
    };
    if passes(negative)? {
        return Ok(Outcome::Pass);
    }
    Ok(match old {
        // The removal alone doesn't break it: the new tests do.
        Old::Untouched | Old::PassesWithout => Outcome::Fail,
        Old::Neither => Outcome::Unsure(NEITHER),
        // No new test is in the base, nor in a control that passed.
        Old::PassesBeside(j, t) => {
            if t.tree == j.base || t.tree == t.control || passes(&t.tree)? {
                Outcome::Unsure(TREE_MADE)
            } else {
                Outcome::Fail
            }
        }
    })
}

#[cfg(test)]
mod tests;
