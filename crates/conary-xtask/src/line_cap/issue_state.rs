// crates/conary-xtask/src/line_cap/issue_state.rs

//! The allowlist's issue-state snapshot: which cited issues are still open.
//!
//! The cap gate is hermetic (no network I/O), so
//! `scripts/refresh-line-cap-issue-state.sh` is the only producer of this
//! snapshot and the only line-cap step that talks to GitHub. The snapshot is
//! bound to the canonical allowlist entry set it was generated from, so the
//! checked-in file either describes the current allowlist or it does not,
//! regardless of how either file reached the working tree.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::{issue_number, positive_number};

const REFRESH_ISSUE_STATE_SCRIPT: &str = "scripts/refresh-line-cap-issue-state.sh";
/// Separates the recorded issue states from the allowlist entries they were
/// generated for. The snapshot is bound to that entry set, so the gate can
/// tell a stale snapshot from a current one without reading file metadata.
const ALLOWLIST_SECTION: &str = "== allowlist";

/// GitHub state of one cited issue, as recorded in the checked-in snapshot.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum IssueState {
    Open,
    Closed,
}

impl IssueState {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "OPEN" => Some(Self::Open),
            "CLOSED" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// Checked-in issue state for every allowlist citation.
///
/// The gate performs no network I/O: `scripts/refresh-line-cap-issue-state.sh`
/// is the only producer of this snapshot and the only line-cap step that talks
/// to GitHub.
#[derive(Debug)]
pub(crate) struct IssueStateSnapshot {
    pub(crate) refreshed: String,
    pub(crate) states: BTreeMap<u64, IssueState>,
    /// The canonical allowlist entries this snapshot was generated from:
    /// `(repository-relative path, issue number)`. This binding is what makes
    /// the snapshot's provenance checkable without consulting file metadata:
    /// the checked-in snapshot either describes the current allowlist or it
    /// does not, regardless of how either file reached the working tree.
    pub(crate) allowlist: BTreeSet<(String, u64)>,
}

/// The allowlist's canonical entry set: every `(path, issue)` pair, sorted and
/// deduplicated. This is the binding recorded in the snapshot and compared
/// against it.
fn canonical_allowlist_entries(allowlist: &BTreeMap<String, String>) -> BTreeSet<(String, u64)> {
    allowlist
        .iter()
        .filter_map(|(path, issue)| issue_number(issue).map(|number| (path.clone(), number)))
        .collect()
}

pub(crate) fn read_issue_state(path: &Path) -> Result<IssueStateSnapshot, String> {
    let contents = fs::read_to_string(path).map_err(|error| {
        format!(
            "line-cap issue-state snapshot not found: {}: {error}",
            path.display()
        )
    })?;
    parse_issue_state(&contents, path)
}

pub(crate) fn parse_issue_state(contents: &str, path: &Path) -> Result<IssueStateSnapshot, String> {
    let mut refreshed = None;
    let mut states = BTreeMap::new();
    let mut allowlist = BTreeSet::new();
    let mut in_allowlist = false;

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == ALLOWLIST_SECTION {
            in_allowlist = true;
            continue;
        }
        if in_allowlist {
            // `<repository-relative path> #<issue>`, the allowlist's own
            // format, recorded verbatim so the binding is reviewable by eye.
            let fields = line.split_whitespace().collect::<Vec<_>>();
            let (Some(entry_path), Some(issue)) = (fields.first(), fields.get(1)) else {
                return Err(format!(
                    "invalid recorded allowlist entry in {} (expected '<path> #<issue>'): {line}",
                    path.display()
                ));
            };
            let Some(number) = issue_number(issue) else {
                return Err(format!(
                    "invalid recorded allowlist issue in {} (expected '#<issue>'): {line}",
                    path.display()
                ));
            };
            if !allowlist.insert(((*entry_path).to_string(), number)) {
                return Err(format!(
                    "duplicate recorded allowlist entry in {}: {line}",
                    path.display()
                ));
            }
            continue;
        }
        // `#` lines are comments, but a data line may carry the issue sigil
        // (`#154 OPEN`); the checked-in snapshot writes it bare (`154 OPEN`).
        let data = match line.strip_prefix('#') {
            Some(rest) if !rest.trim_start().starts_with(|c: char| c.is_ascii_digit()) => {
                if let Some(value) = rest.trim().strip_prefix("refreshed:") {
                    if refreshed.is_some() {
                        return Err(format!(
                            "duplicate refreshed date in issue-state snapshot {}",
                            path.display()
                        ));
                    }
                    refreshed = Some(value.trim().to_string());
                }
                continue;
            }
            Some(rest) => rest,
            None => line,
        };
        let fields = data.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 {
            return Err(format!(
                "invalid issue-state entry in {} (expected '<issue> <STATE>'): {line}",
                path.display()
            ));
        }
        let Some(number) = positive_number(fields[0]) else {
            return Err(format!(
                "invalid issue-state entry in {} (expected '<issue> <STATE>'): {line}",
                path.display()
            ));
        };
        let Some(state) = IssueState::parse(fields[1]) else {
            return Err(format!(
                "invalid issue state in {} (expected OPEN or CLOSED): {line}",
                path.display()
            ));
        };
        if states.insert(number, state).is_some() {
            return Err(format!(
                "duplicate issue-state entry in {}: #{number}",
                path.display()
            ));
        }
    }

    let refreshed = refreshed.ok_or_else(|| {
        format!(
            "issue-state snapshot {} has no 'refreshed: <YYYY-MM-DD>' line",
            path.display()
        )
    })?;
    Ok(IssueStateSnapshot {
        refreshed,
        states,
        allowlist,
    })
}

/// The snapshot cannot silently rot: any allowlist change must be followed by a
/// refresh, because freshly cited issues are exactly what the snapshot cannot know.
/// The snapshot is bound to the allowlist's canonical entry set, not to file
/// metadata. Modification times cannot answer this question: `git restore`
/// rewrites them without changing policy, a fresh checkout stamps every file
/// with the same time, and a same-day edit leaves them indistinguishable. The
/// recorded binding either matches the current allowlist or it does not.
pub(crate) fn validate_allowlist_binding(
    allowlist: &BTreeMap<String, String>,
    snapshot_path: &Path,
    snapshot: &IssueStateSnapshot,
) -> Result<(), String> {
    let current = canonical_allowlist_entries(allowlist);
    if current == snapshot.allowlist {
        return Ok(());
    }
    let added = current
        .difference(&snapshot.allowlist)
        .map(|(path, issue)| format!("{path} #{issue}"))
        .collect::<Vec<_>>();
    let removed = snapshot
        .allowlist
        .difference(&current)
        .map(|(path, issue)| format!("{path} #{issue}"))
        .collect::<Vec<_>>();
    let mut detail = Vec::new();
    if !added.is_empty() {
        detail.push(format!("not recorded: {}", added.join(", ")));
    }
    if !removed.is_empty() {
        detail.push(format!(
            "recorded but no longer cited: {}",
            removed.join(", ")
        ));
    }
    Err(format!(
        "line-cap allowlist does not match issue-state snapshot {} (refreshed: {}); {}. Run {REFRESH_ISSUE_STATE_SCRIPT}",
        snapshot_path.display(),
        snapshot.refreshed,
        detail.join("; ")
    ))
}

pub(crate) fn validate_allowlist_issue_state(
    allowlist: &BTreeMap<String, String>,
    snapshot_path: &Path,
    snapshot: &IssueStateSnapshot,
    errors: &mut Vec<String>,
) {
    for (path, issue) in allowlist {
        let Some(number) = issue_number(issue) else {
            continue;
        };
        match snapshot.states.get(&number) {
            Some(IssueState::Open) => {}
            Some(IssueState::Closed) => errors.push(format!(
                "allowlist entry {path} cites {issue}; snapshot records {issue} as CLOSED in {}",
                snapshot_path.display()
            )),
            None => errors.push(format!(
                "allowlist entry {path} cites {issue}, which is absent from issue-state snapshot {}",
                snapshot_path.display()
            )),
        }
    }
}
