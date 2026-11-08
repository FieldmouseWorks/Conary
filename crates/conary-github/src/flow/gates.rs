//! A dependency bot's workflow repin is not permission to edit the gate.
use conary_core::policy::Paths;
use std::collections::BTreeSet;

fn workflow(path: &str) -> bool {
    let Some(file) = path.strip_prefix(".github/workflows/") else {
        return false;
    };
    !file.contains('/') && (file.ends_with(".yml") || file.ends_with(".yaml"))
}
fn pair(
    path: Option<&str>,
    old: &mut Vec<String>,
    new: &mut Vec<String>,
    pinned: &mut BTreeSet<String>,
) -> bool {
    let valid = old.len() == new.len() && old.iter().zip(new.iter()).all(|(a, b)| a == b);
    if valid
        && !old.is_empty()
        && let Some(path) = path
    {
        pinned.insert(path.into());
    }
    old.clear();
    new.clear();
    valid
}
fn pins(diff: &str) -> bool {
    if diff.is_empty() {
        return false;
    }
    let pin = regex::Regex::new(r"^(\s*(-\s+)?uses:\s*[^\s#@]+@)[0-9A-Za-z._/-]+(\s+#.*)?$")
        .expect("constant regex");
    let mut path = None;
    let mut hunk = false;
    let mut gated = BTreeSet::new();
    let mut pinned = BTreeSet::new();
    let mut old = Vec::new();
    let mut new = Vec::new();
    for line in diff.lines() {
        if (!(hunk && line.starts_with(['+', '-', '\\']))
            || line.starts_with('-') && !new.is_empty())
            && !pair(path, &mut old, &mut new, &mut pinned)
        {
            return false;
        }
        if let Some(header) = line.strip_prefix("diff --git ") {
            let Some((a, b)) = header.split_once(" b/") else {
                if line.contains(".github/") {
                    return false;
                }
                path = None;
                continue;
            };
            let Some(a) = a.strip_prefix("a/") else {
                if line.contains(".github/") {
                    return false;
                }
                path = None;
                continue;
            };
            path = Some(b);
            hunk = false;
            if a.starts_with(".github/") || b.starts_with(".github/") {
                if a != b || !workflow(b) {
                    return false;
                }
                gated.insert(b.to_owned());
            } else {
                path = None;
            }
        } else if path.is_some()
            && [
                "old mode ",
                "new mode ",
                "new file mode ",
                "deleted file mode ",
                "rename from ",
                "rename to ",
                "copy from ",
                "copy to ",
                "Binary files ",
                "GIT binary patch",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            return false;
        } else if line.starts_with("@@") {
            hunk = true;
        } else if path.is_some()
            && hunk
            && line.starts_with(['+', '-'])
            && !line[1..].trim().is_empty()
        {
            let Some(m) = pin.captures(&line[1..]) else {
                return false;
            };
            if line.starts_with('-') {
                old.push(m[1].to_owned());
            } else {
                new.push(m[1].to_owned());
            }
        }
    }
    pair(path, &mut old, &mut new, &mut pinned) && gated.is_subset(&pinned)
}

pub(super) fn edited(paths: &Paths, diff: impl FnOnce() -> Option<String>) -> bool {
    if paths.cut {
        return true;
    }
    let github = |p: &str| p.starts_with(".github/");
    if !paths
        .files
        .iter()
        .any(|p| github(&p.path) || p.from.as_deref().is_some_and(github))
    {
        return false;
    }
    if paths
        .files
        .iter()
        .any(|p| github(&p.path) && !workflow(&p.path))
    {
        return true;
    }
    !diff().is_some_and(|d| pins(&d))
}

pub(super) fn enabled(body: &str) -> bool {
    regex::Regex::new(r"\*\*Automerge\*\*: Enabled\b")
        .expect("constant regex")
        .is_match(body)
}
pub(super) fn spent(body: &str) -> bool {
    let states: Vec<_> = body
        .lines()
        .filter_map(|l| l.strip_prefix("🚦 **Automerge**: "))
        .collect();
    !states.is_empty()
        && states
            .iter()
            .all(|s| s.starts_with("Disabled because a matching PR was automerged previously"))
}
pub(super) fn release(comments: &[crate::api::Comment], settings: &super::Settings) -> Option<u64> {
    let login = settings.renovate_login.as_deref()?;
    let body = comments
        .iter()
        .filter(|c| c.user.as_ref().is_some_and(|u| u.login == login))
        .filter_map(|c| c.body.as_deref())
        .rfind(|b| b.contains("Artifact update problem"))?;
    let blocks = regex::Regex::new(r"(?ms)^```\n(.*?)^```").expect("constant regex");
    let waiting = regex::Regex::new(
        r"^lock-age: nothing in the refresh is old enough until (\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ)$",
    )
    .expect("constant regex");
    let mut latest = None;
    for block in blocks.captures_iter(body) {
        let last = block[1].lines().rfind(|l| !l.trim().is_empty())?;
        let stamp = waiting.captures(last.trim())?;
        let at = super::reviews::timestamp(&stamp[1]).ok()?;
        latest = Some(latest.map_or(at, |t: u64| t.max(at)));
    }
    latest
}
