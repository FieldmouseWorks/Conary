//! A label is a queue signal, never a person's authorization.
use super::persist::Result;
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The authorized person posts this exact scope in an APPROVED GitHub review.
/// GitHub authenticates its account and commit; the body also binds intent
/// and target, which a head-only review cannot establish after a retarget.
pub fn owner_approval(repo: &conary_core::op::Repo, pr: u64, revision: &Revision) -> String {
    format!(
        "<!-- conary-owner:v1 {} -->",
        serde_json::json!({
            "repo":repo.to_string().to_lowercase(),"pr":pr,"revision":revision
        })
    )
}
pub(crate) fn required(f: &Facts) -> bool {
    f.classified.class == conary_core::Class::R2
        || f.judgment
            .as_ref()
            .is_some_and(|j| j.revision == f.revision && j.verdict == Judgment::Owner)
}
pub(crate) fn approved<G: Read>(pass: &Pass<'_>, github: &G, f: &Facts) -> Result<bool> {
    if super::read::excluded(f) || !required(f) {
        return Ok(false);
    }
    let records = github.reviews(&f.repo.to_string(), f.number)?;
    Ok(from_reviews(
        &f.repo,
        f.number,
        &f.revision,
        &records,
        &pass.config.settings.people,
    ))
}
pub(crate) fn from_reviews(
    repo: &conary_core::op::Repo,
    pr: u64,
    revision: &Revision,
    records: &[crate::api::Review],
    people: &std::collections::BTreeSet<String>,
) -> bool {
    let mut latest = BTreeMap::new();
    for review in records {
        let Some(user) = &review.user else {
            continue;
        };
        if user.kind != "User" || !people.contains(&user.login) {
            continue;
        }
        let Some(id) = review.id else {
            continue;
        };
        if latest.get(&user.login).is_none_or(|(old, _)| *old < id) {
            latest.insert(user.login.clone(), (id, review));
        }
    }
    let marker = owner_approval(repo, pr, revision);
    latest.values().any(|(_, r)| {
        r.state.as_deref() == Some("APPROVED")
            && r.commit_id.as_deref() == Some(&revision.head)
            && r.body
                .as_deref()
                .is_some_and(|b| b.lines().any(|line| line == marker))
    })
}

/// The one-way doors the flow asks the owner about: a closed set. A
/// decision with no door doesn't list the owner; the flow makes it itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Door {
    /// An R2 change: a person decides before it lands.
    R2,
    /// An outside contribution: the owner reviews and merges it.
    Outside,
    /// A judge's `owner` verdict, which the request carries the reason of.
    Judge,
    /// The owner label, set by a person rather than the flow.
    Label,
}
impl std::fmt::Display for Door {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::R2 => "R2: a person must decide before it lands",
            Self::Outside => "Outside contribution: the owner reviews and merges it",
            Self::Judge => "The judge sent it to the owner",
            Self::Label => "The owner decides (the owner label)",
        })
    }
}

/// What one owner request decides: its door, repository, PR and the scope
/// it binds, the intent fingerprint. A new head that keeps the intent keeps
/// the key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Subject {
    pub door: Door,
    pub repo: String,
    pub pr: u64,
    pub scope: String,
}

/// The one open request for a [`Subject`]. A later wait on the same key
/// joins it: no new listing or notice, and its marker moves to the head
/// now. Joining never widens an approval, which still names that exact head
/// ([`owner_approval`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub key: Subject,
    /// When the owner was first asked.
    pub since: u64,
    /// Every PR waiting on it. The key names one PR, so that's the one.
    pub prs: std::collections::BTreeSet<u64>,
    /// The approval marker for the PR as it is now, when its door takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    /// A judge's reason for its `owner` verdict, as its answer gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The owner label was put on for it: a request is listed once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub listed: bool,
}

/// Open the request `step` lists the owner for, or join the one open for
/// its key. A request for another scope is moot: a new intent is a new
/// decision, so it's closed.
pub(super) fn ask(f: &Facts, m: &mut Memory, step: &Step, now: u64) -> Option<Request> {
    m.owner.retain(|r| r.key.scope == f.revision.description);
    let Step::ListOwner(door) = step else {
        return None;
    };
    let key = Subject {
        door: *door,
        repo: f.repo.to_string().to_lowercase(),
        pr: f.number,
        scope: f.revision.description.clone(),
    };
    let at = match m.owner.iter().position(|r| r.key == key) {
        Some(at) => at,
        None => {
            m.owner.push(Request {
                key,
                since: now,
                prs: [f.number].into(),
                marker: None,
                reason: None,
                listed: false,
            });
            m.owner.len() - 1
        }
    };
    let request = &mut m.owner[at];
    request.marker = required(f).then(|| owner_approval(&f.repo, f.number, &f.revision));
    if *door == Door::Judge && f.judge_reason.is_some() {
        request.reason.clone_from(&f.judge_reason);
    }
    Some(request.clone())
}

/// Who put a label on, by the account GitHub says did: a person's
/// (`User`) or an App's (`Bot`), the flow's or the steward's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Labeller {
    Person,
    App,
}

/// The owner label as a person's door ([`person_label`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersonLabel {
    /// A person put it on.
    Person,
    /// It's on and not the flow's own, and who put it on can't be told:
    /// the read failed, or no `labeled` event names a person or an App.
    Untold,
}

/// The owner label, when it's on.
fn owner_label(f: &Facts) -> Option<&String> {
    f.settings
        .owner_label
        .as_ref()
        .filter(|l| f.labels.contains(*l))
}

/// The owner label as a person's door: on, not the flow's own listing
/// ([`Memory::labelled`]), and put on by a person, or by someone the read
/// couldn't tell. A label an App put on, the flow's before it kept its
/// mark or the steward's, is no door.
pub(crate) fn person_label(f: &Facts, m: &Memory) -> Option<PersonLabel> {
    if m.labelled {
        return None;
    }
    owner_label(f)?;
    match f.labelled_by {
        Some(Labeller::Person) => Some(PersonLabel::Person),
        Some(Labeller::App) => None,
        None => Some(PersonLabel::Untold),
    }
}

/// Whether the owner label may be a person's door ([`person_label`]).
pub(crate) fn person_labelled(f: &Facts, m: &Memory) -> bool {
    person_label(f, m).is_some()
}

/// Reads who put the owner label on into the facts, by the PR's timeline's
/// newest `labeled` event for it, only while the label is on and not the
/// flow's own: one read, named `owner_label` when it fails, which leaves
/// it untold. A PR outside the pipeline isn't read for it.
pub(crate) fn labeller<S: crate::api::Source + ?Sized>(source: &S, f: &mut Facts, m: &Memory) {
    let Some(label) = owner_label(f).cloned() else {
        return;
    };
    if m.labelled || super::read::excluded(f) {
        return;
    }
    let repo = f.repo.to_string();
    f.labelled_by = f
        .reads
        .take(Reading::OwnerLabel, source.timeline(&repo, f.number))
        .and_then(|events| {
            events.into_iter().rev().find(|e| {
                e.event.as_deref() == Some("labeled")
                    && e.label.as_ref().is_some_and(|l| l.name == label)
            })
        })
        .and_then(|e| e.actor)
        .and_then(|a| match a.kind.as_str() {
            "User" => Some(Labeller::Person),
            "Bot" => Some(Labeller::App),
            _ => None,
        });
}

/// Why the flow's own judge answered `owner` on this revision, from its
/// worker's answer, read without changing it. The reason is the judge's
/// own words: it goes in the request and the status, never the ledger.
pub(super) fn judge_reason(
    record: &super::Record,
    revision: &Revision,
    now: u64,
) -> Option<String> {
    record
        .jobs
        .iter()
        .rev()
        .filter(|w| matches!(w.purpose, super::jobs::Purpose::Judge) && w.job.revision == *revision)
        .find_map(
            |w| match super::jobs::inspect(w, now).ok()?.result?.ok()?.answer {
                conary_core::plugin::Answer::Judge {
                    verdict: Judgment::Owner,
                    why,
                } => Some(why),
                _ => None,
            },
        )
}

#[cfg(test)]
mod tests {
    use super::super::jobs::{Completion, Purpose, Work};
    use super::*;

    fn work(dir: &std::path::Path, id: &str, revision: &Revision) -> Work {
        Work {
            purpose: Purpose::Judge,
            job: Job {
                id: id.into(),
                revision: revision.clone(),
                state: JobState::Done,
                started: 1,
                repeated: false,
            },
            path: dir.join(format!("{id}.job")),
            deadline: 10_000,
            posted: true,
            recipes: Default::default(),
            tree: None,
            slotless: false,
            handed: vec![],
            sent: Default::default(),
        }
    }
    fn answer(work: &Work, verdict: Judgment, why: &str) {
        let completion = Completion {
            at: 2,
            result: Some(Ok(conary_core::plugin::Response {
                contract: conary_core::plugin::CONTRACT.into(),
                by: conary_core::plugin::Identity {
                    engine: "fixture".into(),
                    session: None,
                },
                execution: None,
                answer: conary_core::plugin::Answer::Judge {
                    verdict,
                    why: why.into(),
                },
            })),
        };
        std::fs::write(
            work.path.with_extension("answer"),
            serde_json::to_vec(&completion).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn the_reason_is_the_owner_verdict_of_this_revisions_judge() {
        let dir = tempfile::tempdir().unwrap();
        let revision = Revision {
            head: "head".into(),
            base: "main".into(),
            description: "intent".into(),
        };
        let mut earlier = revision.clone();
        earlier.head = "earlier".into();
        let mut record = super::super::Record::default();
        let stale = work(dir.path(), "j-0", &earlier);
        answer(&stale, Judgment::Owner, "an earlier head's reason");
        let passed = work(dir.path(), "j-1", &revision);
        answer(&passed, Judgment::Pass, "not the owner's");
        record.jobs = vec![stale, passed, work(dir.path(), "j-2", &revision)];
        // One with no answer yet says nothing.
        assert_eq!(judge_reason(&record, &revision, 3), None);
        let owner = work(dir.path(), "j-3", &revision);
        answer(&owner, Judgment::Owner, "it rewrites who may land");
        record.jobs.push(owner);
        assert_eq!(
            judge_reason(&record, &revision, 3).as_deref(),
            Some("it rewrites who may land")
        );
    }
}
