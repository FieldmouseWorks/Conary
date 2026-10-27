//! A landed change's note (`docs/DESIGN.md` section 7, `docs/notes.md`):
//! the public-safe slice of the ledger that rides on `refs/notes/conary`,
//! on the trunk commit its landing made, for any clone to read. A note is a
//! readout: it says what the ledger says, and nothing signs it.

use serde::{Deserialize, Serialize};

use crate::id::{ChangeId, CommitId, RevisionId};
use crate::ledger::{Change, Landing};
use crate::op::{Actor, Authority, CheckKind, Outcome, Repo};

/// The ref the notes are kept on, in each change's own repository.
pub const REF: &str = "refs/notes/conary";

/// What a note's `conary` field says: the format it's in.
pub const FORMAT: &str = "note v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub conary: String,
    pub change: ChangeId,
    pub repo: Repo,
    pub pr: u64,
    pub url: String,
    /// The intent's summary, the PR's title. Never its body.
    pub summary: Option<String>,
    pub task: Option<String>,
    pub author: Actor,
    pub landing: NoteLanding,
    /// The landed revision's evidence, as the ledger holds it: recorded,
    /// not counted.
    pub evidence: Vec<NoteEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteLanding {
    pub revision: RevisionId,
    pub before: CommitId,
    pub after: CommitId,
    pub by: Authority,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteEvidence {
    pub kind: CheckKind,
    pub check: String,
    pub result: Outcome,
    pub source: String,
}

/// Whether a repository's notes may link the sessions and threads its
/// changes came from. A repo whose own rules keep those links out of its
/// history keeps them out of its notes too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Links {
    Kept,
    Left,
}

impl Note {
    /// The note for `change`, landed by `landing`.
    pub fn of(change: &Change, landing: &Landing, links: Links) -> Self {
        let intent = change.intent.as_ref();
        let mut author = change.author.clone();
        // A task is an issue (`owner/name#N`) or a thread's URL.
        let mut task = intent.and_then(|i| i.task.clone());
        if links == Links::Left {
            author.session = None;
            task = task.filter(|t| !t.contains("://"));
        }
        Self {
            conary: FORMAT.to_owned(),
            change: change.id,
            repo: change.repo.clone(),
            pr: change.pr,
            url: format!("https://github.com/{}/pull/{}", change.repo, change.pr),
            summary: intent.map(|i| i.summary.clone()),
            task,
            author,
            landing: NoteLanding {
                revision: landing.revision,
                before: landing.before.clone(),
                after: landing.after.clone(),
                by: landing.by.clone(),
                at: landing.at,
            },
            evidence: change
                .evidence
                .iter()
                .filter(|e| e.revision == landing.revision)
                .map(|e| NoteEvidence {
                    kind: e.kind,
                    check: e.check.clone(),
                    result: e.result,
                    source: e.source.clone(),
                })
                .collect(),
        }
    }

    /// The note as it's kept: the same ledger always gives the same text,
    /// so a writer can tell an unchanged note by its blob.
    pub fn text(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("a note serializes");
        text.push('\n');
        text
    }

    /// A note read back from its text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let note: Self = serde_json::from_str(text).map_err(|e| format!("not a note: {e}"))?;
        if note.conary != FORMAT {
            return Err(format!(
                "a note in format {:?}, not {FORMAT:?}",
                note.conary
            ));
        }
        Ok(note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::op::{ActorKind, Body};

    fn c(n: &str) -> CommitId {
        n.repeat(40 / n.len()).parse().unwrap()
    }

    /// A ledger holding one landed change, its evidence on the landed
    /// revision and on one before it, and the note it gives.
    fn note(task: &str, links: Links) -> Note {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(
            crate::log::Lock::take(dir.path()).unwrap(),
            ed25519_dalek::SigningKey::from_bytes(&[4; 32]),
            None,
        )
        .unwrap();
        let change = ChangeId::new().unwrap();
        let (old, landed) = (
            RevisionId::new(&c("a"), &c("c")),
            RevisionId::new(&c("a"), &c("b")),
        );
        let evidence = |revision, check: &str| Body::Evidence {
            change,
            revision,
            kind: CheckKind::Ci,
            check: check.into(),
            result: Outcome::Pass,
            source: "https://github.com/o/r/runs/1".into(),
        };
        for body in [
            Body::Observe {
                change,
                repo: "o/r".parse().unwrap(),
                pr: 9,
                author: Actor {
                    kind: ActorKind::Agent,
                    login: "agent".into(),
                    session: Some("https://claude.ai/code/session_x".into()),
                },
            },
            Body::Intent {
                change,
                summary: "area: a plain sentence".into(),
                body: "the prompt-shaped body".into(),
                task: Some(task.into()),
            },
            Body::Revision {
                change,
                base: c("a"),
                head: c("c"),
            },
            evidence(old, "stale"),
            Body::Revision {
                change,
                base: c("a"),
                head: c("b"),
            },
            evidence(landed, "flake"),
            Body::Land {
                change,
                revision: landed,
                before: c("a"),
                after: c("d"),
                by: Authority::Status {
                    login: "steward".into(),
                },
            },
        ] {
            store.record(body).unwrap();
        }
        let change = store.ledger().change(change).unwrap();
        Note::of(change, change.landing.as_ref().unwrap(), links)
    }

    #[test]
    fn a_note_holds_the_landed_revision_and_never_the_body() {
        let note = note("o/r#3", Links::Kept);
        assert_eq!(note.summary.as_deref(), Some("area: a plain sentence"));
        assert_eq!(note.task.as_deref(), Some("o/r#3"));
        assert_eq!(note.url, "https://github.com/o/r/pull/9");
        assert_eq!(note.landing.after, c("d"));
        let checks: Vec<_> = note.evidence.iter().map(|e| e.check.as_str()).collect();
        assert_eq!(checks, ["flake"]);
        let text = note.text();
        assert!(!text.contains("prompt-shaped"), "{text}");
        assert_eq!(Note::parse(&text).unwrap(), note);
    }

    #[test]
    fn a_repo_that_keeps_session_links_out_gets_notes_without_them() {
        let thread = "https://claude.ai/code/project/p?thread=t";
        let kept = note(thread, Links::Kept).text();
        assert!(kept.contains("claude.ai/code/session_x"), "{kept}");
        assert!(kept.contains("thread=t"), "{kept}");
        let left = note(thread, Links::Left).text();
        assert!(!left.contains("claude.ai"), "{left}");
        // An issue isn't a session link.
        assert_eq!(note("o/r#3", Links::Left).task.as_deref(), Some("o/r#3"));
    }

    #[test]
    fn text_in_another_format_is_refused() {
        let text = note("o/r#3", Links::Kept).text().replace(FORMAT, "note v2");
        assert!(Note::parse(&text).is_err());
        assert!(Note::parse("{}").is_err());
    }
}
