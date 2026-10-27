//! The git a checkout answers for `why`: which commit last touched a line,
//! and which GitHub repository the checkout is of. Git itself is run as a
//! command; what it prints is parsed here.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use crate::id::CommitId;
use crate::op::Repo;

#[derive(Debug)]
pub enum GitError {
    /// Git couldn't be run, or said no.
    Git(String),
    /// The line isn't in a commit yet.
    Uncommitted,
    /// The checkout's origin isn't a GitHub repository.
    NotGithub(String),
    /// What the notes ref holds for a commit isn't a note.
    NotANote { commit: String, error: String },
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Git(e) => write!(f, "git: {e}"),
            Self::Uncommitted => f.write_str("that line isn't committed yet"),
            Self::NotGithub(url) => write!(f, "origin {url:?} isn't a GitHub repository"),
            Self::NotANote { commit, error } => write!(f, "the note on {commit}: {error}"),
        }
    }
}

impl std::error::Error for GitError {}

fn run(dir: &Path, args: &[&str]) -> Result<std::process::Output, GitError> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| GitError::Git(e.to_string()))
}

fn git(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    let out = run(dir, args)?;
    if !out.status.success() {
        return Err(GitError::Git(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| GitError::Git(e.to_string()))
}

/// The commit that last changed `line` (from 1) of `file` in the checkout
/// at `dir`, as `git blame` finds it on the checked-out head.
pub fn blame(dir: &Path, file: &Path, line: u32) -> Result<CommitId, GitError> {
    let range = format!("{line},{line}");
    let file = file
        .to_str()
        .ok_or_else(|| GitError::Git("the path isn't UTF-8".into()))?;
    parse_blame(&git(
        dir,
        &["blame", "--porcelain", "-L", &range, "--", file],
    )?)
}

fn parse_blame(porcelain: &str) -> Result<CommitId, GitError> {
    let sha = porcelain
        .split_whitespace()
        .next()
        .ok_or_else(|| GitError::Git("blame printed nothing".into()))?;
    if sha.bytes().all(|b| b == b'0') {
        return Err(GitError::Uncommitted);
    }
    sha.parse()
        .map_err(|_| GitError::Git(format!("blame printed {sha:?} as a commit")))
}

/// `commit` and every commit after it that the checked-out head holds: those
/// that descend from it and are ancestors of the head, from wherever on the
/// way they're reached.
pub fn descendants(dir: &Path, commit: &CommitId) -> Result<HashSet<String>, GitError> {
    let range = format!("{commit}..HEAD");
    let out = git(dir, &["rev-list", "--ancestry-path", &range])?;
    Ok(out
        .lines()
        .map(str::to_owned)
        .chain([commit.to_string()])
        .collect())
}

/// The GitHub repository the checkout's `origin` names: its URL as
/// configured, before any `insteadOf` sends the fetches somewhere else (a
/// mirror, a proxy).
pub fn github_repo(dir: &Path) -> Result<Repo, GitError> {
    let url = git(dir, &["config", "--get", "remote.origin.url"])?;
    parse_remote(url.trim())
}

/// Fetches `origin`'s `notes_ref` over the checkout's own, and says whether
/// `origin` has one.
pub fn fetch_notes(dir: &Path, notes_ref: &str) -> Result<bool, GitError> {
    let found = run(dir, &["ls-remote", "--exit-code", "origin", notes_ref])?;
    // `--exit-code` exits 2 when nothing matched.
    if found.status.code() == Some(2) {
        return Ok(false);
    }
    if !found.status.success() {
        return Err(GitError::Git(
            String::from_utf8_lossy(&found.stderr).trim().to_owned(),
        ));
    }
    let refspec = format!("+{notes_ref}:{notes_ref}");
    git(dir, &["fetch", "--quiet", "--no-tags", "origin", &refspec])?;
    Ok(true)
}

/// The notes on `notes_ref` for the commits in `commits`: each one's text,
/// by the commit it's on.
pub fn notes(
    dir: &Path,
    notes_ref: &str,
    commits: &HashSet<String>,
) -> Result<Vec<(String, String)>, GitError> {
    let list = git(dir, &["notes", "--ref", notes_ref, "list"])?;
    parse_notes_list(&list)?
        .into_iter()
        .filter(|(_, commit)| commits.contains(commit))
        .map(|(blob, commit)| Ok((commit, git(dir, &["cat-file", "blob", &blob])?)))
        .collect()
}

/// `git notes list`'s lines, `<note blob> <commit>`, as pairs.
fn parse_notes_list(list: &str) -> Result<Vec<(String, String)>, GitError> {
    list.lines()
        .map(|l| {
            l.split_once(' ')
                .map(|(blob, commit)| (blob.to_owned(), commit.to_owned()))
                .ok_or_else(|| GitError::Git(format!("notes list printed {l:?}")))
        })
        .collect()
}

/// A remote's URL without its userinfo, which can hold a token: fit to
/// print.
fn redact(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").map_or(("", url), |(s, r)| (s, r));
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let host = match rest[..authority_end].rsplit_once('@') {
        Some((_, host)) => format!("{host}{}", &rest[authority_end..]),
        None => rest.to_owned(),
    };
    if scheme.is_empty() {
        host
    } else {
        format!("{scheme}://{host}")
    }
}

fn parse_remote(url: &str) -> Result<Repo, GitError> {
    let not = || GitError::NotGithub(redact(url));
    let path = match ["https://", "ssh://"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme))
    {
        // Credentials in the URL are left behind with the host.
        Some(rest) => rest
            .rsplit_once('@')
            .map_or(rest, |(_, host)| host)
            .strip_prefix("github.com/"),
        None => url.strip_prefix("git@github.com:"),
    }
    .ok_or_else(not)?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    match path.split('/').collect::<Vec<_>>()[..] {
        [owner, name] if !owner.is_empty() && !name.is_empty() => Ok(Repo {
            owner: owner.to_owned(),
            name: name.to_owned(),
        }),
        _ => Err(not()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blame_names_the_commit_that_last_changed_the_line() {
        let sha = "0b8c3fd5c5ec7f0c43d42a0d6e3f3c6b9e1f2a3b";
        let porcelain = format!(
            "{sha} 12 12 1\nauthor Peter\nauthor-mail <p@example.com>\nfilename README.md\n\tthe line\n"
        );
        assert_eq!(parse_blame(&porcelain).unwrap().as_str(), sha);
        let uncommitted = format!("{} 1 1 1\nauthor Not Committed Yet\n", "0".repeat(40));
        assert!(matches!(
            parse_blame(&uncommitted),
            Err(GitError::Uncommitted)
        ));
        assert!(parse_blame("").is_err());
    }

    #[test]
    fn notes_list_pairs_each_note_with_its_commit() {
        let (blob, commit) = ("1".repeat(40), "2".repeat(40));
        let pairs = parse_notes_list(&format!("{blob} {commit}\n")).unwrap();
        assert_eq!(pairs, [(blob, commit)]);
        assert!(parse_notes_list("").unwrap().is_empty());
        assert!(parse_notes_list("garbage\n").is_err());
    }

    #[test]
    fn origin_is_read_in_every_form_github_hands_out() {
        for url in [
            "https://github.com/FieldmouseWorks/burrow",
            "https://github.com/FieldmouseWorks/burrow.git",
            "https://github.com/FieldmouseWorks/burrow/",
            "git@github.com:FieldmouseWorks/burrow.git",
            "ssh://git@github.com/FieldmouseWorks/burrow.git",
            "https://x-access-token:secret@github.com/FieldmouseWorks/burrow",
            "https://github.com/fieldmouseworks/BURROW",
        ] {
            let repo = parse_remote(url).unwrap();
            let want = Repo {
                owner: "FieldmouseWorks".into(),
                name: "burrow".into(),
            };
            assert_eq!(repo, want, "{url}");
        }
        for url in [
            "https://gitlab.com/FieldmouseWorks/burrow",
            "https://github.com/FieldmouseWorks",
            "https://github.com/FieldmouseWorks/burrow/tree/main",
            "/data/dev/src/burrow",
        ] {
            assert!(parse_remote(url).is_err(), "{url}");
        }
    }

    #[test]
    fn a_refused_origin_is_named_without_its_credentials() {
        for url in [
            "https://user:TOKEN@github.example/owner/repo",
            "https://TOKEN@github.example:8443/owner/repo",
            "ssh://git:TOKEN@gitlab.com/owner/repo.git",
            "https://user:TOKEN@github.com",
            "TOKEN@github.example:owner/repo",
            "https://a@b:TOKEN@github.example/owner/repo",
        ] {
            let e = parse_remote(url).unwrap_err().to_string();
            assert!(!e.contains("TOKEN"), "{url}: {e}");
        }
        assert_eq!(
            redact("https://user:TOKEN@github.example:8443/owner/repo"),
            "https://github.example:8443/owner/repo"
        );
    }
}
