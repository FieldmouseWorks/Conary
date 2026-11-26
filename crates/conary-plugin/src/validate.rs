//! Path, Git object ID, digest, identity and task validation shared by
//! plugins and runners.
use super::{Failure, Identity, Problem, Task};

fn bad<T>(why: &str) -> Result<T, Failure> {
    Err(Failure(why.into()))
}

/// Whether `path` is relative and, read lexically, names something below the
/// repository root: never absolute, never the root itself, and no `..` that
/// climbs above it, however many components it passes through.
pub fn inside_repository(path: &str) -> bool {
    if path.trim().is_empty() || path.contains(['\\', '\0']) {
        return false;
    }
    let mut depth = 0usize;
    for component in std::path::Path::new(path).components() {
        match component {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return false,
        }
    }
    depth > 0
}

/// Whether `s` is a full Git object ID (40 or 64 ASCII hex digits).
pub fn valid_git_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether `s` is a SHA-256 digest in lowercase hex (64 lowercase hex digits).
pub fn valid_sha256_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether every string is nonblank and no string repeats.
pub fn unique_nonblank<'a>(items: impl IntoIterator<Item = &'a str>) -> bool {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .all(|s| !s.trim().is_empty() && seen.insert(s))
}

/// Check that a plugin or author identity has a nonblank engine and, if set,
/// a nonblank session.
pub fn valid_identity(by: &Identity) -> Result<(), Failure> {
    if by.engine.trim().is_empty() || by.session.as_ref().is_some_and(|s| s.trim().is_empty()) {
        bad("empty plugin identity")
    } else {
        Ok(())
    }
}

/// What every work task must carry, whichever contract carries it. The v4
/// fields are checked even when an older wire leaves them off, so callers
/// can't build a request a v4 plugin would get wrong.
pub fn valid_task(task: &Task) -> Result<(), Failure> {
    let full_objects = match task {
        Task::Review { base, head, .. } => valid_git_oid(base) && valid_git_oid(head),
        Task::Fix { head, problems, .. } => {
            valid_git_oid(head)
                && problems
                    .iter()
                    .all(|p| !matches!(p, Problem::Conflict { base, .. } if !valid_git_oid(base)))
        }
        Task::Judge { head, .. } => valid_git_oid(head),
        Task::Proof { head, tree, .. } | Task::Prepare { head, tree, .. } => {
            valid_git_oid(head) && valid_git_oid(tree)
        }
        Task::Job { .. } => true,
        Task::Describe => false,
    };
    if !full_objects {
        return bad("plugin requests require full Git object IDs");
    }
    match task {
        Task::Fix { pr, .. } if *pr == 0 => bad("a fix request needs its PR"),
        Task::Judge {
            pr, why, context, ..
        } => {
            if *pr == 0 {
                return bad("a judge request needs its PR");
            }
            if why.is_empty() || why.iter().enumerate().any(|(i, w)| why[..i].contains(w)) {
                return bad("a judge request needs a nonempty, unique why");
            }
            if !valid_sha256_digest(context) {
                return bad("a judge request needs its context digest");
            }
            Ok(())
        }
        Task::Prepare { checks, .. } if !unique_nonblank(checks.iter().map(String::as_str)) => {
            bad("prepare checks must be unique and nonblank")
        }
        _ => Ok(()),
    }
}
