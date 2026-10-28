use std::path::PathBuf;
use std::process::Command;

use super::*;
use crate::Store;
use crate::id::{ChangeId, RevisionId};
use crate::notes::{FORMAT, NoteLanding};
use crate::op::{Actor, ActorKind, Authority, Body};

fn c(n: &str) -> CommitId {
    n.repeat(40 / n.len()).parse().unwrap()
}

fn set(commits: &[&str]) -> HashSet<String> {
    commits.iter().map(|n| c(n).to_string()).collect()
}

fn burrow() -> Repo {
    "FieldmouseWorks/burrow".parse().unwrap()
}

#[test]
fn a_line_is_found_by_the_landing_that_brought_its_commit_onto_trunk() {
    let repo = burrow();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(
        crate::log::Lock::take(dir.path()).unwrap(),
        ed25519_dalek::SigningKey::from_bytes(&[4; 32]),
        None,
    )
    .unwrap();
    // Trunk: a, then 1 merged as m, then 2 rebased as r1 r2, then 3
    // squashed as s.
    let mut land = |pr, before: &str, after: &str| {
        let change = ChangeId::new().unwrap();
        for body in [
            Body::Observe {
                change,
                repo: repo.clone(),
                pr,
                author: Actor {
                    kind: ActorKind::Person,
                    login: "TusanHomichi".into(),
                    session: None,
                },
            },
            Body::Revision {
                change,
                base: c(before),
                head: c("f"),
            },
            Body::Land {
                change,
                revision: RevisionId::new(&c(before), &c("f")),
                before: c(before),
                after: c(after),
                by: Authority::Status {
                    login: "steward".into(),
                },
            },
        ] {
            store.record(body).unwrap();
        }
    };
    // The ledger can hold them in any order.
    land(3, "e2", "e3");
    land(2, "e0", "e2");
    land(1, "a", "e0");
    let ledger = store.ledger();
    let notes = || {
        ledger
            .changes()
            .map(|c| Note::of(c, c.landing.as_ref().unwrap(), Links::Kept))
    };
    let pr = |after: &[&str]| landing(&repo, notes(), &set(after)).map(|n| n.pr);

    // A commit of 1's, seen from a branch that merged trunk in later (b):
    // the landing is 1's, not the branch's merge.
    assert_eq!(pr(&["d", "e0", "e1", "e2", "e3", "b"]), Some(1));
    // The first of 2's rebased commits, and the squash itself.
    assert_eq!(pr(&["e1", "e2", "e3"]), Some(2));
    assert_eq!(pr(&["e3"]), Some(3));
    // A commit on a branch that hasn't landed.
    assert_eq!(pr(&["b"]), None);
    // Another repository's landings answer nothing here.
    let elsewhere: Repo = "FieldmouseWorks/remi".parse().unwrap();
    assert!(landing(&elsewhere, notes(), &set(&["e3"])).is_none());
}

/// Git in `dir`, as a person with no config of their own would run it.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// An origin that is a bare repository on disk, reached as burrow on
/// GitHub, and a checkout of it with two commits on main.
fn checkout(root: &Path) -> (PathBuf, String, String) {
    let origin = root.join("origin.git");
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(
        root,
        &["init", "--quiet", "--bare", origin.to_str().unwrap()],
    );
    git(&work, &["init", "--quiet", "-b", "main"]);
    let github = "https://github.com/FieldmouseWorks/burrow";
    let rewrite = format!("url.{}.insteadOf", origin.display());
    git(&work, &["config", &rewrite, github]);
    git(&work, &["remote", "add", "origin", github]);
    let commit = |text: &str| {
        std::fs::write(work.join("f"), text).unwrap();
        git(&work, &["add", "f"]);
        git(&work, &["commit", "--quiet", "-m", text]);
        git(&work, &["rev-parse", "HEAD"])
    };
    let a = commit("one\n");
    let b = commit("one\ntwo\n");
    git(&work, &["push", "--quiet", "origin", "main"]);
    (work, a, b)
}

fn note(before: &str, after: &str) -> Note {
    Note {
        conary: FORMAT.into(),
        change: ChangeId::new().unwrap(),
        repo: burrow(),
        pr: 7,
        url: "https://github.com/FieldmouseWorks/burrow/pull/7".into(),
        summary: Some("area: the second line".into()),
        task: None,
        author: Actor {
            kind: ActorKind::Agent,
            login: "TusanHomichi".into(),
            session: None,
        },
        landing: NoteLanding {
            revision: RevisionId::new(&c(before), &c(after)),
            before: before.parse().unwrap(),
            after: after.parse().unwrap(),
            by: Authority::Status {
                login: "steward".into(),
            },
            at: 1,
        },
        evidence: Vec::new(),
    }
}

/// Puts `note` on `on` and pushes it to origin, keeping no copy here, so
/// only a fetch finds it.
fn push_note(work: &Path, on: &str, text: &str) {
    std::fs::write(work.join("note"), text).unwrap();
    let file = work.join("note");
    let file = file.to_str().unwrap();
    git(
        work,
        &["notes", "--ref", notes::REF, "add", "-f", "-F", file, on],
    );
    git(work, &["push", "--quiet", "--force", "origin", notes::REF]);
    git(work, &["update-ref", "-d", notes::REF]);
}

#[test]
fn a_checkout_answers_from_the_notes_on_its_origin() {
    let root = tempfile::tempdir().unwrap();
    let (work, a, b) = checkout(root.path());
    let at = |line| why_from_notes(&work, Path::new("f"), line).unwrap();

    // No notes on origin yet: nothing landed, and that isn't an error.
    let why = at(2);
    assert_eq!(
        (why.source, why.commit.as_str()),
        (Source::Notes, b.as_str())
    );
    assert_eq!(why.repo, burrow());
    assert!(why.landed.is_none());

    push_note(&work, &b, &note(&a, &b).text());
    assert_eq!(at(2).landed.map(|n| n.pr), Some(7));
    // The first line came before that landing, which doesn't answer for it.
    assert!(at(1).landed.is_none());
}

#[test]
fn a_note_that_isnt_one_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (work, a, b) = checkout(root.path());
    push_note(&work, &b, "not a note\n");
    let e = why_from_notes(&work, Path::new("f"), 2).unwrap_err();
    assert!(matches!(e, GitError::NotANote { .. }), "{e}");
    // A note that names another commit than the one it's on.
    push_note(&work, &b, &note(&a, &a).text());
    let e = why_from_notes(&work, Path::new("f"), 2).unwrap_err();
    assert!(matches!(e, GitError::NotANote { .. }), "{e}");
}
