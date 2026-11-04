//! Undo on GitHub (`docs/DESIGN.md` section 5): the PR that reverts a
//! landed change, opened and recorded in the ledger as its undo. The revert
//! is built the way the train builds a candidate, through GitHub's API
//! ([`Branches`]): a commit of the tree trunk had before the change landed,
//! made on its landing, reverts that landing and nothing after it; then
//! trunk is merged into its branch, as GitHub's "update branch" would.
//! Where later work on trunk built on what it reverts, that merge
//! conflicts, and the PR opens without it, conflicting with trunk, for a
//! job to resolve like any PR's conflict. By session or time, a repo's
//! changes take one PR: the newest one's revert is its base, and each older
//! one's revert is merged in after, as `git revert` would lay them.

use std::collections::BTreeSet;

use conary_core::ledger::{Change, Invalid};
use conary_core::op::{Asked, Body, Repo};
use conary_core::store::StoreError;
use conary_core::{ChangeId, Store};

use serde::{Deserialize, Serialize};

use crate::api::{ApiError, Source};
use crate::branches::{BRANCHES, Branches, Merged};
use crate::ingest::{self, Config, IngestError};

/// The writes undo needs beyond a repo's [`Branches`].
pub trait Reverts: Branches {
    /// Makes a commit of `tree` on `parent`, as the token's own: its sha.
    fn make_commit(
        &self,
        repo: &str,
        tree: &str,
        parent: &str,
        message: &str,
    ) -> Result<String, ApiError>;
    /// Opens a PR from the branch `head` onto `base`: its number.
    fn open_pull(
        &self,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ApiError>;
    /// Every PR from the repo's own branch `head`, open, closed or merged,
    /// whether the branch is there still or not: oldest first.
    fn pulls_from(&self, repo: &str, head: &str) -> Result<Vec<u64>, ApiError>;
}

/// Where the `attempt`th undo of `change` is made. A run that stops
/// before recording its undo leaves the next one the same name, so it
/// takes up what that one made; one recorded, and closed, moves the next
/// on to a fresh branch.
pub fn undo_branch(change: ChangeId, attempt: usize) -> String {
    format!("{BRANCHES}undo/{change}/{attempt}")
}

/// What an undo's PR reverts and what asked for it, on a line of its body
/// that `undo` writes, so ingest knows the PR for an undo whatever reads it
/// first. Only a PR from an [`undo_branch`] in the repo itself is read for
/// it: anyone can write a body, and a fork's branch is anyone's to name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Marker {
    pub reverts: Vec<ChangeId>,
    pub asked: Asked,
}

const MARKER: &str = "<!-- conary-undo: ";

impl Marker {
    pub(crate) fn line(&self) -> String {
        let json = serde_json::to_string(self).expect("a marker serializes");
        format!("{MARKER}{json} -->")
    }

    /// The marker of a PR from `branch` with `body`, if it's an undo's.
    pub(crate) fn read(branch: &str, body: &str) -> Option<Self> {
        let rest = branch.strip_prefix(BRANCHES)?.strip_prefix("undo/")?;
        let (_, attempt) = rest.split_once('/')?;
        attempt.parse::<usize>().ok()?;
        let line = body
            .lines()
            .rev()
            .find_map(|l| l.trim().strip_prefix(MARKER))?;
        serde_json::from_str(line.strip_suffix("-->")?.trim()).ok()
    }
}

#[derive(Debug)]
pub enum UndoError {
    Api(ApiError),
    Store(StoreError),
    Ingest(IngestError),
    /// The ledger has no such change.
    Unknown(ChangeId),
    /// It hasn't landed, so there's nothing on trunk to revert.
    NotLanded(ChangeId),
    /// Its undo has landed already: undoing that undo puts it back.
    Reverted {
        change: ChangeId,
        by: ChangeId,
    },
    /// Its undo landed, and `by` undid that, which put it back. A change
    /// takes one undo once one has landed, so undoing `by` reverts it again.
    UndoneUndo {
        change: ChangeId,
        undo: ChangeId,
        by: ChangeId,
    },
    /// The trunk branch isn't there.
    NoTrunk(String),
    /// Trunk already lacks what it brought: its revert would change nothing.
    Nothing(ChangeId),
    /// Nothing that landed is what was asked for.
    NoneLanded(Asked),
}

impl std::fmt::Display for UndoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::Store(e) => e.fmt(f),
            Self::Ingest(e) => e.fmt(f),
            Self::Unknown(id) => write!(f, "change {id} isn't in the ledger"),
            Self::NotLanded(id) => write!(f, "change {id} hasn't landed"),
            Self::Reverted { change, by } => write!(
                f,
                "change {change} is reverted already, by {by}: undo that to put it back"
            ),
            Self::UndoneUndo { change, undo, by } => write!(
                f,
                "change {change}'s undo {undo} landed and {by} undid it: undo {by} to revert it again"
            ),
            Self::NoTrunk(name) => write!(f, "no trunk branch {name}"),
            Self::Nothing(id) => write!(
                f,
                "trunk already lacks what change {id} brought, so reverting it changes nothing"
            ),
            Self::NoneLanded(Asked::Actor { session }) => {
                write!(f, "session {session} has landed nothing to undo")
            }
            Self::NoneLanded(Asked::Since { since }) => {
                write!(f, "nothing to undo has landed since {}", time(*since))
            }
            Self::NoneLanded(Asked::Change) => write!(f, "nothing to undo"),
        }
    }
}

impl std::error::Error for UndoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(e) => Some(e),
            Self::Ingest(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ApiError> for UndoError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<StoreError> for UndoError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<IngestError> for UndoError {
    fn from(e: IngestError) -> Self {
        Self::Ingest(e)
    }
}

/// How the undo's PR came to be open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// Opened now, and whether it conflicts with trunk as it is.
    Opened { conflicts: bool },
    /// Open already, from an earlier run.
    Found,
}

/// The undo, open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undone {
    /// The undo's own change, and its PR.
    pub change: ChangeId,
    pub pr: u64,
    /// What it reverts, newest first.
    pub reverts: Vec<ChangeId>,
    /// What was asked for and its revert couldn't take, newest first: its
    /// revert conflicts with the others'. Each is left for an undo of its own.
    pub left: Vec<ChangeId>,
    pub how: How,
    /// Ops written; 0 when the ledger already held everything.
    pub recorded: usize,
}

/// Opens the PR that reverts `change`'s landing, and records it as its
/// undo. It reads the change's undos as GitHub has them first,
/// so run again, it finds the undo it opened rather than open another.
pub fn undo(
    store: &mut Store,
    forge: &dyn Reverts,
    source: &dyn Source,
    config: &Config,
    change: ChangeId,
) -> Result<Undone, UndoError> {
    let recorded = refresh(store, forge, source, config, change)?;
    let ledger = store.ledger();
    let x = ledger.change(change).ok_or(UndoError::Unknown(change))?;
    if x.landing.is_none() {
        return Err(UndoError::NotLanded(change));
    }
    if let Some(by) = x.reverted_by {
        return Err(UndoError::Reverted { change, by });
    }
    if let Some(found) = found(ledger, change, recorded)? {
        return Ok(found);
    }
    let repo = x.repo.clone();
    let mut got = open(
        store,
        forge,
        source,
        config,
        &repo,
        &[change],
        Asked::Change,
    )?;
    got.recorded += recorded;
    Ok(got)
}

/// Opens one PR in each repo that reverts every change `session` landed
/// there and nothing reverts yet, newest first, and records it as their
/// undo. Run again, it finds those, and opens one only for what's landed
/// since or was left out.
pub fn undo_actor(
    store: &mut Store,
    forge: &dyn Reverts,
    source: &dyn Source,
    config: &Config,
    session: &str,
) -> Result<Vec<Undone>, UndoError> {
    let asked = Asked::Actor {
        session: session.to_owned(),
    };
    let by = |c: &Change| c.author.session.as_deref() == Some(session);
    let mut repos: Vec<Repo> = store
        .ledger()
        .changes()
        .filter(|c| by(c))
        .map(|c| c.repo.clone())
        .collect();
    repos.sort();
    repos.dedup();
    let mut got = Vec::new();
    for repo in repos {
        got.extend(group(store, forge, source, config, &repo, &asked, &|c| {
            by(c)
        })?);
    }
    if got.is_empty() {
        return Err(UndoError::NoneLanded(asked));
    }
    Ok(got)
}

/// Opens the PR that reverts everything that landed on `repo`'s trunk
/// after `since` (milliseconds since the Unix epoch) and nothing reverts
/// yet, newest first, and records it as their undo. Run again, it finds
/// that, and opens another only for what's landed since or was left out.
pub fn undo_since(
    store: &mut Store,
    forge: &dyn Reverts,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    since: i64,
) -> Result<Vec<Undone>, UndoError> {
    let asked = Asked::Since { since };
    let after = |c: &Change| c.landing.as_ref().is_some_and(|l| l.at > since);
    let got = group(store, forge, source, config, repo, &asked, &after)?;
    if got.is_empty() {
        return Err(UndoError::NoneLanded(asked));
    }
    Ok(got)
}

/// The undos of `repo`'s landed changes that `asked` names (`pick`): the
/// ones open already, found, and one opened for those that have none.
fn group(
    store: &mut Store,
    forge: &dyn Reverts,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    asked: &Asked,
    pick: &dyn Fn(&Change) -> bool,
) -> Result<Vec<Undone>, UndoError> {
    let picked = |store: &Store| -> Vec<ChangeId> {
        let mut picked: Vec<&Change> = store
            .ledger()
            .changes()
            .filter(|c| &c.repo == repo && c.landing.is_some() && c.reverted_by.is_none())
            .filter(|c| pick(c))
            .collect();
        picked.sort_by_key(|c| std::cmp::Reverse((c.landing.as_ref().map(|l| l.at), c.pr)));
        picked.into_iter().map(|c| c.id).collect()
    };
    let mut recorded = 0;
    for id in picked(store) {
        recorded += refresh(store, forge, source, config, id)?;
    }
    let mut got: Vec<Undone> = Vec::new();
    let mut todo = Vec::new();
    for id in picked(store) {
        match found(store.ledger(), id, 0) {
            Ok(Some(f)) => {
                if !got.iter().any(|g| g.change == f.change) {
                    got.push(f);
                }
            }
            Ok(None) => todo.push(id),
            // Its undo landed and was undone: undoing that is what reverts it.
            Err(UndoError::UndoneUndo { .. }) => {}
            Err(e) => return Err(e),
        }
    }
    if !todo.is_empty() {
        match open(store, forge, source, config, repo, &todo, asked.clone()) {
            Ok(opened) => got.push(opened),
            Err(UndoError::Nothing(_)) if !got.is_empty() => {}
            Err(e) => return Err(e),
        }
    }
    if let Some(first) = got.first_mut() {
        first.recorded += recorded;
    }
    Ok(got)
}

/// The undo of `change` that's open, if any. One landed and undone in turn
/// is an error: undoing that undo is what reverts it again.
fn found(
    ledger: &conary_core::ledger::Ledger,
    change: ChangeId,
    recorded: usize,
) -> Result<Option<Undone>, UndoError> {
    // The ledger takes one undo at a time, and once one has landed it's the
    // only one. Since this change isn't reverted, its undo is open still, or
    // landed and undone in turn.
    let Some(u) = ledger.undo_of(change).and_then(|u| ledger.change(u)) else {
        return Ok(None);
    };
    if let Some(by) = u.reverted_by {
        return Err(UndoError::UndoneUndo {
            change,
            undo: u.id,
            by,
        });
    }
    Ok(Some(Undone {
        change: u.id,
        pr: u.pr,
        reverts: u
            .undo
            .as_ref()
            .map(|d| d.reverts.clone())
            .unwrap_or_default(),
        left: Vec::new(),
        how: How::Found,
        recorded,
    }))
}

/// Opens the PR that reverts `changes`, newest first, as trunk is now, and
/// records it as their undo, asked by `asked`. The newest one's revert is
/// the branch's base, trunk merged in; each older one's is merged in after,
/// so what landed between them stays. One whose revert conflicts is left
/// out, for an undo of its own.
fn open(
    store: &mut Store,
    forge: &dyn Reverts,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    changes: &[ChangeId],
    asked: Asked,
) -> Result<Undone, UndoError> {
    let ledger = store.ledger();
    let newest = ledger.change(changes[0]).expect("picked from the ledger");
    let full = repo.to_string();
    let branch = undo_branch(newest.id, newest.undone_by.len() + 1);
    let revert = |x: &Change| -> Result<String, UndoError> {
        let landing = x.landing.as_ref().expect("picked as landed");
        let before = forge.tree(&full, landing.before.as_str())?;
        let message = format!(
            "{}\n\nThis reverts {}, #{}'s landing.",
            title(x),
            landing.after,
            x.pr
        );
        Ok(forge.make_commit(&full, &before, landing.after.as_str(), &message)?)
    };

    let trunk_name = forge.default_branch(&full)?;
    let trunk = forge
        .branch(&full, &trunk_name)?
        .ok_or_else(|| UndoError::NoTrunk(trunk_name.clone()))?;
    // A branch already there is an earlier run's, which stopped before its
    // PR: the same revert, on what trunk was then.
    if forge.branch(&full, &branch)?.is_none() {
        forge.create_branch(&full, &branch, &revert(newest)?)?;
    }
    let message = format!("Merge {trunk_name} into {branch}");
    let conflicts = match forge.merge(&full, &branch, &trunk, &message)? {
        Merged::Commit { .. } | Merged::Already => false,
        Merged::Conflict => true,
    };
    let (mut reverts, mut left) = (vec![newest.id], Vec::new());
    for &id in &changes[1..] {
        let x = ledger.change(id).expect("picked from the ledger");
        let message = format!("Revert #{} onto {branch}", x.pr);
        match forge.merge(&full, &branch, &revert(x)?, &message)? {
            Merged::Commit { .. } | Merged::Already => reverts.push(id),
            Merged::Conflict => left.push(id),
        }
    }
    let head = forge
        .branch(&full, &branch)?
        .ok_or_else(|| ApiError::new(format!("{full}'s {branch} went as it was made")))?;
    if !conflicts && forge.tree(&full, &head)? == forge.tree(&full, &trunk)? {
        forge.delete_branch(&full, &branch)?;
        return Err(UndoError::Nothing(newest.id));
    }
    let x: Vec<&Change> = reverts
        .iter()
        .map(|id| ledger.change(*id).expect("picked"))
        .collect();
    let skipped: Vec<&Change> = left
        .iter()
        .map(|id| ledger.change(*id).expect("picked"))
        .collect();
    let title = match &asked {
        Asked::Change => title(newest),
        Asked::Actor { session } => {
            format!("undo: revert {} from session {session}", count(x.len()))
        }
        Asked::Since { since } => format!("undo: revert {} since {}", count(x.len()), time(*since)),
    };
    let marker = Marker {
        reverts: reverts.clone(),
        asked,
    };
    let body = body(&x, &skipped, &trunk_name, conflicts, &marker);
    let pr = forge.open_pull(&full, &branch, &trunk_name, &title, &body)?;
    let (undo, recorded) = record(store, source, config, repo, pr, &marker)?;
    Ok(Undone {
        change: undo,
        pr,
        reverts,
        left,
        how: How::Opened { conflicts },
        recorded,
    })
}

fn count(n: usize) -> String {
    if n == 1 {
        "1 change".into()
    } else {
        format!("{n} changes")
    }
}

/// Milliseconds since the Unix epoch as a UTC time, to the second.
fn time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, s) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// Brings the ledger up to what GitHub holds of `change`'s undos, so what
/// `undo` does next is decided on them as they are now. Whether the change
/// is reverted turns on each of its undos and, an undo of an undo putting
/// it back, on theirs in turn, so each one down the tree is read again,
/// closed ones too (a closed PR can reopen), and so is every PR from the
/// branch of each one's next attempt, which a run that stopped before
/// recording its undo left behind, open, closed or merged. Ops written.
fn refresh(
    store: &mut Store,
    forge: &dyn Reverts,
    source: &dyn Source,
    config: &Config,
    change: ChangeId,
) -> Result<usize, UndoError> {
    let repo = store
        .ledger()
        .change(change)
        .ok_or(UndoError::Unknown(change))?
        .repo
        .clone();
    let mut recorded = 0;
    let mut todo = vec![change];
    // The ledger refuses a cycle of undos, so this is a tree.
    let mut seen = BTreeSet::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        let ledger = store.ledger();
        let held: Vec<u64> = ledger
            .change(id)
            .expect("in the ledger")
            .undone_by
            .iter()
            .map(|u| ledger.change(*u).expect("in the ledger").pr)
            .collect();
        for pr in held {
            recorded += ingest::pull(store, source, config, &repo, pr)?.recorded;
        }
        recorded += stopped(store, forge, source, config, &repo, id)?;
        todo.extend(
            store
                .ledger()
                .change(id)
                .expect("in the ledger")
                .undone_by
                .iter(),
        );
    }
    Ok(recorded)
}

/// Records as `change`'s undo every PR from the branch of its next attempt:
/// a run that stopped before recording it left it. One recorded moves the
/// next attempt on to a branch of its own, which a later stopped run may
/// have left too. Ops written.
fn stopped(
    store: &mut Store,
    forge: &dyn Reverts,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    change: ChangeId,
) -> Result<usize, UndoError> {
    let mut recorded = 0;
    loop {
        let attempts = undone_by(store, change);
        let branch = undo_branch(change, attempts + 1);
        for pr in forge.pulls_from(&repo.to_string(), &branch)? {
            recorded += ingest::pull(store, source, config, repo, pr)?.recorded;
        }
        if undone_by(store, change) == attempts {
            return Ok(recorded);
        }
    }
}

fn undone_by(store: &Store, change: ChangeId) -> usize {
    store
        .ledger()
        .change(change)
        .map_or(0, |c| c.undone_by.len())
}

/// Ingests the undo's PR, whose marker says what it reverts, and makes
/// sure it's recorded as their undo: its change, and the ops written.
fn record(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    pr: u64,
    marker: &Marker,
) -> Result<(ChangeId, usize), UndoError> {
    let got = ingest::pull(store, source, config, repo, pr)?;
    let mut recorded = got.recorded;
    match store.record(Body::Undo {
        change: got.change,
        reverts: marker.reverts.clone(),
        asked: marker.asked.clone(),
    }) {
        Ok(_) => recorded += 1,
        Err(StoreError::Invalid(Invalid::Duplicate)) => {}
        Err(e) => return Err(e.into()),
    }
    Ok((got.change, recorded))
}

/// The undo's title, in the style of what it reverts: `area: sentence`
/// becomes `area: undo #N, sentence`, the house style its repo's subjects
/// keep.
fn title(x: &Change) -> String {
    let summary = x.intent.as_ref().map(|i| i.summary.as_str());
    match summary.map(|s| s.split_once(": ")) {
        Some(Some((area, sentence))) => format!("{area}: undo #{}, {sentence}", x.pr),
        Some(None) => format!("undo: #{}, {}", x.pr, summary.unwrap_or_default()),
        None => format!("undo: #{}", x.pr),
    }
}

fn body(
    reverts: &[&Change],
    left: &[&Change],
    trunk: &str,
    conflicts: bool,
    marker: &Marker,
) -> String {
    let line = |x: &Change| {
        let after = x
            .landing
            .as_ref()
            .map(|l| l.after.to_string())
            .unwrap_or_default();
        format!(
            "#{}, which landed as {after} (Conary change `{}`)",
            x.pr, x.id
        )
    };
    let mut body = match reverts {
        [x] => format!("Undoes {}, by reverting that landing.", line(x)),
        _ => {
            let mut b = "Undoes these, newest first, by reverting each one's landing:\n".to_owned();
            for x in reverts {
                b += &format!("\n- {}", line(x));
            }
            b
        }
    };
    if !left.is_empty() {
        body += "\n\nLeft out, since each one's revert conflicts with the ones above: run the \
                 undo again for one of their own.\n";
        for x in left {
            body += &format!("\n- {}", line(x));
        }
    }
    if conflicts {
        let prs: Vec<String> = reverts.iter().map(|x| format!("#{}", x.pr)).collect();
        let prs = prs.join(", ");
        body += &format!(
            "\n\nWhat came after {prs} on {trunk} builds on it, so this conflicts with {trunk}: \
             merge {trunk} in and resolve each conflict, keeping what came after and \
             taking out what {prs} brought."
        );
    }
    body + "\n\n" + &marker.line()
}

#[cfg(test)]
mod tests;
