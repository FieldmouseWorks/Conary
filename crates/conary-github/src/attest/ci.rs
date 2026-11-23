//! Git-only planning for the reusable attestation workflow. No candidate
//! hooks, filters or programs run while deriving the attested identities.
use std::process::Command;

pub mod removal;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn git(repo: &std::path::Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn oid(id: &str) -> Result<()> {
    if !matches!(id.len(), 40 | 64)
        || !id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err("CI requires full lowercase git object IDs".into());
    }
    Ok(())
}

pub struct Plan {
    pub base: String,
    pub tree: Option<String>,
    pub tests: Vec<String>,
    /// Where the PR deletes a test file, the trees a failure on `tree` is
    /// judged by. Never signed.
    pub judging: Option<removal::Judging>,
}

// These are the path rules documented in ci-attestation.md. Git's glob
// pathspec implements them, including **, without a second glob engine.
const TESTS: &[&str] = &[
    ":(glob)tests/**",
    ":(glob)**/tests.rs",
    ":(glob)**/tests/**",
    ":(glob)**/test_*.py",
    ":(glob)**/*_test.*",
    ":(glob)**/*.test.ts",
    ":(glob)src/test/**",
];

/// Whether the test changes add or change a test beyond pure moves. An
/// added test path is a move only when the same revision deletes a test
/// path with the identical mode and blob ID, each deletion accounting for
/// one addition at most. No similarity score: a file edited as it moved,
/// or moved in from a path that is no test path, still counts.
fn adds_tests(tests: &[removal::Change]) -> bool {
    let mut deleted = std::collections::HashMap::<(&str, &str), usize>::new();
    for change in tests.iter().filter(|c| c.deletes) {
        *deleted
            .entry((&change.was_mode, &change.was_id))
            .or_default() += 1;
    }
    tests.iter().any(|change| {
        if change.deletes {
            return false;
        }
        if !change.added {
            return true;
        }
        match deleted.get_mut(&(change.mode.as_str(), change.id.as_str())) {
            Some(left) if *left > 0 => {
                *left -= 1;
                false
            }
            _ => true,
        }
    })
}

pub fn plan(repo: &std::path::Path, base: &str, head: &str) -> Result<Plan> {
    oid(base)?;
    oid(head)?;
    let base = git(repo, &["merge-base", base, head])?.trim().to_owned();
    let changes = removal::changes(repo, &base, head, true)?;
    let tests: Vec<String> = changes
        .iter()
        .filter(|c| !c.deletes)
        .map(|c| c.path.clone())
        .collect();
    let all = removal::changes(repo, &base, head, false)?;
    if !adds_tests(&changes) || all.iter().all(|a| changes.iter().any(|c| c.path == a.path)) {
        return Ok(Plan {
            base,
            tree: None,
            tests: vec![],
            judging: None,
        });
    }
    let tree = removal::tree_with(repo, &base, &changes.iter().collect::<Vec<_>>())?;
    let judging = removal::judging(repo, &base, &changes)?;
    Ok(Plan {
        base,
        tree: Some(tree),
        tests,
        judging,
    })
}

/// Read the exact tested tree, rejecting a revision unrelated to its head.
pub fn tree(repo: &std::path::Path, commit: &str, head: Option<&str>) -> Result<String> {
    oid(commit)?;
    if let Some(head) = head {
        oid(head)?;
        git(repo, &["merge-base", "--is-ancestor", head, commit])?;
    }
    Ok(git(repo, &["rev-parse", &format!("{commit}^{{tree}}")])?
        .trim()
        .to_owned())
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Job {
    result: String,
}

/// Summarise only job names and conclusions, excluding arbitrary outputs.
/// Empty, malformed and cancelled runs never
/// produce evidence; skipped jobs cannot turn a gate green.
pub fn summary(jobs: &str) -> Result<(super::make::Gate, String)> {
    let jobs: std::collections::BTreeMap<String, Job> = serde_json::from_str(jobs)?;
    if jobs.is_empty()
        || jobs.values().any(|j| {
            !matches!(
                j.result.as_str(),
                "success" | "failure" | "skipped" | "cancelled"
            )
        })
    {
        return Err("caller must name test jobs with Actions conclusions".into());
    }
    if jobs.values().any(|j| j.result == "cancelled") {
        return Err("cancelled runs attest nothing".into());
    }
    let result = if jobs.values().all(|j| j.result == "success") {
        super::make::Gate::Pass
    } else {
        super::make::Gate::Fail
    };
    Ok((result, serde_json::to_string(&jobs)?))
}

/// Authenticate the caller's CI wiring against its repository's reviewed
/// default branch. GitHub supplies the workflow identity; the signer supplies
/// this check, so a candidate cannot replace needs results or test invocations.
/// The entire .github tree is protected, including local reusable workflows
/// and actions that the caller can delegate to.
pub fn trusted_caller(
    repo: &std::path::Path,
    commit: &str,
    repository: &str,
    workflow_ref: &str,
    default_branch: &str,
) -> Result<()> {
    oid(commit)?;
    let path = workflow_ref
        .strip_prefix(&format!("{repository}/"))
        .and_then(|s| s.rsplit_once('@').map(|(path, _)| path))
        .filter(|p| {
            p.starts_with(".github/workflows/") && (p.ends_with(".yml") || p.ends_with(".yaml"))
        })
        .ok_or("the caller must be a workflow in the candidate's repository")?;
    if path.split('/').any(|p| p == ".." || p.is_empty()) {
        return Err("invalid caller workflow path".into());
    }
    let trusted = format!("refs/remotes/origin/{default_branch}");
    // Validate the complete ref rather than allowing rev-parse expressions.
    git(repo, &["check-ref-format", &trusted])?;
    let trusted = git(repo, &["rev-parse", "--verify", &trusted])?
        .trim()
        .to_owned();
    oid(&trusted)?;
    git(repo, &["cat-file", "-e", &format!("{trusted}:{path}")])?;
    let ci_tree = |revision: &str| -> Result<String> {
        Ok(git(
            repo,
            &["rev-parse", "--verify", &format!("{revision}:.github")],
        )?
        .trim()
        .to_owned())
    };
    if ci_tree(commit)? != ci_tree(&trusted)? {
        return Err("candidate CI wiring differs from the reviewed default branch; caller results and negative commands cannot be trusted".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
