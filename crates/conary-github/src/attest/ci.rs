//! Git-only planning for the reusable attestation workflow. No candidate
//! hooks, filters or programs run while deriving the attested identities.
use std::process::Command;

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

fn paths(
    repo: &std::path::Path,
    base: &str,
    head: &str,
    tests: bool,
    filter: &str,
) -> Result<Vec<String>> {
    let mut args = vec![
        "diff-tree",
        "--no-commit-id",
        "-r",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--name-only",
        "-z",
        filter,
        base,
        head,
        "--",
    ];
    if tests {
        args.extend_from_slice(TESTS);
    }
    Ok(git(repo, &args)?
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
}

pub fn plan(repo: &std::path::Path, base: &str, head: &str) -> Result<Plan> {
    oid(base)?;
    oid(head)?;
    let base = git(repo, &["merge-base", base, head])?.trim().to_owned();
    let tests = paths(repo, &base, head, true, "--diff-filter=ACMT")?;
    let changed_tests = paths(repo, &base, head, true, "--diff-filter=ACDMT")?;
    let all = paths(repo, &base, head, false, "--diff-filter=ACDMT")?;
    if tests.is_empty() || all.iter().all(|p| changed_tests.contains(p)) {
        return Ok(Plan {
            base,
            tree: None,
            tests: vec![],
        });
    }
    // A separate index prevents touching the candidate's worktree, hooks,
    // filters, or configuration-controlled checkout conversions.
    let temporary = tempfile::tempdir()?;
    let index = temporary.path().join("index");
    let run = |cmd: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .env("GIT_INDEX_FILE", &index)
            .args(cmd)
            .output()?;
        if !out.status.success() {
            return Err("negative index construction failed".into());
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    };
    run(&["read-tree", &base])?;
    // ls-tree supplies modes and blob IDs; update-index --index-info takes
    // those verbatim and never invokes a candidate's smudge/clean filters.
    for path in changed_tests {
        let entry = git(
            repo,
            &["ls-tree", "-z", head, "--", &format!(":(literal){path}")],
        )?;
        use std::io::Write;
        use std::process::Stdio;
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .env("GIT_INDEX_FILE", &index)
            .args(["update-index", "-z", "--index-info"])
            .stdin(Stdio::piped())
            .spawn()?;
        let entry = if entry.is_empty() {
            format!("0 {}\t{path}\0", "0".repeat(head.len()))
        } else {
            entry
        };
        child.stdin.take().unwrap().write_all(entry.as_bytes())?;
        if !child.wait()?.success() {
            return Err("negative index update failed".into());
        }
    }
    let tree = run(&["write-tree"])?;
    Ok(Plan {
        base,
        tree: Some(tree),
        tests,
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
