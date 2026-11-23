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

/// When Renovate's release-age wait ends: the latest time its last
/// artifact comment names, when every block of that comment ends in the
/// wait. A block that ends in anything else is a real failure, so there's
/// no wait. A block that ends in the wait's words with no time after them
/// is a mark that didn't parse ([`super::degraded`]), whatever the other
/// blocks say.
pub(super) fn release(
    comments: &[crate::api::Comment],
    settings: &super::Settings,
) -> Result<Option<u64>, super::degraded::Unparsed> {
    const WAIT: &str = "lock-age: nothing in the refresh is old enough until ";
    let Some(login) = settings.renovate_login.as_deref() else {
        return Ok(None);
    };
    let Some(body) = comments
        .iter()
        .filter(|c| c.user.as_ref().is_some_and(|u| u.login == login))
        .filter_map(|c| c.body.as_deref())
        .rfind(|b| b.contains("Artifact update problem"))
    else {
        return Ok(None);
    };
    let blocks = regex::Regex::new(r"(?ms)^```\n(.*?)^```").expect("constant regex");
    let time = regex::Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$").expect("constant regex");
    let mut latest = None;
    let mut failed = false;
    for block in blocks.captures_iter(body) {
        let Some(last) = block[1].lines().rfind(|l| !l.trim().is_empty()) else {
            failed = true;
            continue;
        };
        let Some(until) = last.trim().strip_prefix(WAIT) else {
            failed = true;
            continue;
        };
        let at = time
            .is_match(until)
            .then(|| super::reviews::timestamp(until).ok())
            .flatten()
            .ok_or(super::degraded::Unparsed("its time isn't one"))?;
        latest = Some(latest.map_or(at, |t: u64| t.max(at)));
    }
    Ok(if failed { None } else { latest })
}
