//! A landed change's note (`docs/DESIGN.md` section 7, `docs/notes.md`):
//! the public-safe slice of the ledger that rides on `refs/notes/conary`,
//! on the trunk commit its landing made, for any clone to read. A note is a
//! readout: it says what the ledger says, and nothing signs it.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::id::{ChangeId, CommitId, RevisionId};
use crate::ledger::{Change, Landing, Ledger};
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
/// history keeps them out of its notes too: with `Left`, a note links
/// nowhere but GitHub, in any of its fields.
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
        let text = |t: &str| match links {
            Links::Kept => t.to_owned(),
            Links::Left => without_links(t),
        };
        if links == Links::Left {
            author.session = None;
            task = task.filter(|t| without_links(t) == *t);
        }
        Self {
            conary: FORMAT.to_owned(),
            change: change.id,
            repo: change.repo.clone(),
            pr: change.pr,
            url: change.url(),
            summary: intent.map(|i| text(&i.summary)),
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
                    check: text(&e.check),
                    result: e.result,
                    source: text(&e.source),
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

/// The note of every change in `repo` that `ledger` holds as landed, oldest
/// first.
pub fn landed<'a>(
    ledger: &'a Ledger,
    repo: &'a Repo,
    links: Links,
) -> impl Iterator<Item = Note> + 'a {
    ledger
        .changes()
        .filter(move |c| &c.repo == repo)
        .filter_map(move |c| Some(Note::of(c, c.landing.as_ref()?, links)))
}

/// What a field that links somewhere other than GitHub reads as in a note
/// that leaves links out.
const LEFT_OUT: &str = "[left out: it links off GitHub]";

/// A link to github.com, through to the first character a URL path on
/// GitHub doesn't need: what a note that leaves links out may keep.
static GITHUB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)https://github\.com/[A-Za-z0-9._~/#%?=&+-]*").unwrap());

/// Anything that may be a link: a scheme or a protocol-relative `//`, or a
/// host followed by a path, the host dotted (maybe with DNS's trailing dot)
/// or given a port (`claude.ai/code/...`, `claude.ai.:443/...`,
/// `localhost:8080/...`).
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    // A label is any letters or digits, a dot any of the ones IDNA reads as
    // one (`claude。ai` is `claude.ai` to a browser).
    let label = r"[\p{L}\p{N}_-]+";
    let dot = r"[.\x{3002}\x{FF0E}\x{FF61}]";
    Regex::new(&format!(
        r"//|{label}(?:(?:{dot}{label})+{dot}?(?::[0-9]*)?|:[0-9]+)/"
    ))
    .unwrap()
});

/// `text`, or [`LEFT_OUT`] when anything in it but GitHub's own links may
/// link somewhere else. The whole field goes, not the link alone: telling
/// where a link ends in free text is guesswork, and a guess that's short
/// leaks the rest.
fn without_links(text: &str) -> String {
    if LINK.is_match(&GITHUB.replace_all(text, "")) {
        LEFT_OUT.to_owned()
    } else {
        text.to_owned()
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
        let evidence = |revision, check: &str, source: &str| Body::Evidence {
            change,
            revision,
            kind: CheckKind::Ci,
            check: check.into(),
            result: Outcome::Pass,
            source: source.into(),
        };
        let run = "https://github.com/o/r/runs/1";
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
                summary: "area: a plain sentence from claude.ai/code/session_y".into(),
                body: "the prompt-shaped body".into(),
                task: Some(task.into()),
            },
            Body::Revision {
                change,
                base: c("a"),
                head: c("c"),
            },
            evidence(old, "stale", run),
            Body::Revision {
                change,
                base: c("a"),
                head: c("b"),
            },
            evidence(landed, "flake", run),
            evidence(landed, "status", "https://claude.ai/code/session_z"),
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
        assert_eq!(
            note.summary.as_deref(),
            Some("area: a plain sentence from claude.ai/code/session_y")
        );
        assert_eq!(note.task.as_deref(), Some("o/r#3"));
        assert_eq!(note.url, "https://github.com/o/r/pull/9");
        assert_eq!(note.landing.after, c("d"));
        let checks: Vec<_> = note.evidence.iter().map(|e| e.check.as_str()).collect();
        assert_eq!(checks, ["flake", "status"]);
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
        let left = note(thread, Links::Left);
        let text = left.text();
        assert!(!text.contains("claude.ai"), "{text}");
        assert!(left.task.is_none() && left.author.session.is_none());
        assert_eq!(left.summary.as_deref(), Some(LEFT_OUT));
        // GitHub's own links stay.
        assert_eq!(left.evidence[0].source, "https://github.com/o/r/runs/1");
        // An issue isn't a session link.
        assert_eq!(note("o/r#3", Links::Left).task.as_deref(), Some("o/r#3"));
    }

    #[test]
    fn a_note_without_links_links_nowhere_but_github() {
        for kept in [
            "area: a plain sentence",
            "https://github.com/o/r/actions/runs/1/job/2",
            "https://github.com/o/r.js/pull/1 and https://github.com/o/r/issues/2",
            "lib/project.nix and docs/notes.md, why.rs:40",
            "o/r#3",
        ] {
            assert_eq!(without_links(kept), kept);
        }
        for left in [
            "fix it (https://claude.ai/code/session_x) now",
            "see claude.ai/code/session_x",
            "chatgpt.com/c/abc",
            "https://github.com/o/r,https://claude.ai/code/session_x",
            "https://github.com/o/r,claude.ai/code/session_x",
            "//claude.ai/code/session_x",
            "[the session](https://claude.ai/code/session_x)",
            "https://github.com.evil.example/x",
            "https://github.com@claude.ai/code/session_x",
            "http://github.com/o/r",
            "http://localhost:8080/x",
            "claude.ai:443/code/session_x",
            "claude.ai.:443/code/session_x",
            "claude.ai./code/session_x",
            "(claude.ai:443/code/session_x)",
            "[s](claude.ai:/code/session_x)",
            "localhost:8080/x",
            "127.0.0.1:8080/x",
            "claude\u{3002}ai/code/session_x",
            "claude\u{FF0E}ai/code/session_x",
            "CLAUDE.AI/code/session_x",
        ] {
            assert_eq!(without_links(left), LEFT_OUT, "{left}");
        }
    }

    #[test]
    fn text_in_another_format_is_refused() {
        let text = note("o/r#3", Links::Kept).text().replace(FORMAT, "note v2");
        assert!(Note::parse(&text).is_err());
        assert!(Note::parse("{}").is_err());
    }
}
