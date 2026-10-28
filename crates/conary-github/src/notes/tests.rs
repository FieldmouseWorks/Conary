use std::cell::RefCell;

use conary_core::id::{ChangeId, CommitId, RevisionId};
use conary_core::log::Lock;
use conary_core::op::{Actor, ActorKind, Authority, Body, CheckKind, Outcome};
use conary_core::{Store, notes::REF};

use super::*;

/// A notes ref in memory: its files, and how many commits it has had.
#[derive(Default)]
struct Fake {
    files: RefCell<HashMap<String, String>>,
    commits: RefCell<usize>,
    /// Somebody else moves the ref before the next write.
    raced: RefCell<bool>,
}

impl Fake {
    fn kept(&self) -> Option<Kept> {
        let n = *self.commits.borrow();
        (n > 0).then(|| Kept {
            commit: format!("commit {n}"),
            tree: format!("tree {n}"),
            notes: self
                .files
                .borrow()
                .iter()
                .map(|(path, text)| (path.replace('/', ""), (path.clone(), blob_id(text))))
                .collect(),
        })
    }
}

impl Notes for Fake {
    fn get(&self, repo: &str, name: &str) -> Result<Option<Kept>, ApiError> {
        assert_eq!((repo, name), ("FieldmouseWorks/burrow", REF));
        Ok(self.kept())
    }

    fn set(
        &self,
        _: &str,
        _: &str,
        parent: Option<&Kept>,
        files: &[(String, String)],
    ) -> Result<(), ApiError> {
        if std::mem::take(&mut *self.raced.borrow_mut()) {
            *self.commits.borrow_mut() += 1;
        }
        if parent.map(|p| &p.commit) != self.kept().as_ref().map(|k| &k.commit) {
            return Err(ApiError("the ref moved".into()));
        }
        self.files.borrow_mut().extend(files.iter().cloned());
        *self.commits.borrow_mut() += 1;
        Ok(())
    }
}

fn c(n: &str) -> CommitId {
    n.repeat(40 / n.len()).parse().unwrap()
}

fn burrow() -> Repo {
    "FieldmouseWorks/burrow".parse().unwrap()
}

/// A store whose ledger has PR 1 and 2 landed on burrow, 3 open on it, and
/// 4 landed on another repository.
fn store(dir: &std::path::Path) -> (Store, ChangeId) {
    let mut store = Store::open(
        Lock::take(dir).unwrap(),
        ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
        None,
    )
    .unwrap();
    let mut first = None;
    for (repo, pr, land) in [
        (burrow(), 1, Some(("a", "b"))),
        (burrow(), 2, Some(("b", "d"))),
        (burrow(), 3, None),
        ("FieldmouseWorks/remi".parse().unwrap(), 4, Some(("e", "f"))),
    ] {
        let change = ChangeId::new().unwrap();
        first.get_or_insert(change);
        let base = land.map_or("a", |(before, _)| before);
        let mut bodies = vec![
            Body::Observe {
                change,
                repo,
                pr,
                author: Actor {
                    kind: ActorKind::Agent,
                    login: "TusanHomichi".into(),
                    session: Some("https://claude.ai/code/session_x".into()),
                },
            },
            Body::Revision {
                change,
                base: c(base),
                head: c("9"),
            },
        ];
        if let Some((before, after)) = land {
            bodies.push(Body::Land {
                change,
                revision: RevisionId::new(&c(before), &c("9")),
                before: c(before),
                after: c(after),
                by: Authority::Status {
                    login: "steward".into(),
                },
            });
        }
        for body in bodies {
            store.record(body).unwrap();
        }
    }
    (store, first.unwrap())
}

#[test]
fn blob_ids_are_gits() {
    // `git hash-object --stdin` of each.
    assert_eq!(blob_id(""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    assert_eq!(
        blob_id("hello\n"),
        "ce013625030ba8dba906f756967f9e9ca394464a"
    );
}

#[test]
fn each_landing_gets_one_note_and_writing_again_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = store(dir.path());
    let fake = Fake::default();
    let publish = || publish(&fake, store.ledger(), &burrow(), Links::Kept).unwrap();

    assert_eq!(publish(), 2);
    let files = fake.files.borrow().clone();
    let mut on: Vec<_> = files.keys().cloned().collect();
    on.sort();
    assert_eq!(on, [c("b").to_string(), c("d").to_string()]);
    let note = Note::parse(&files[&c("d").to_string()]).unwrap();
    assert_eq!(note.pr, 2);

    assert_eq!(publish(), 0);
    assert_eq!(*fake.commits.borrow(), 1, "nothing to write, no commit");
}

#[test]
fn a_note_the_ledger_has_moved_on_from_is_written_over_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, first) = store(dir.path());
    let fake = Fake::default();
    // A note git fanned out, and one somebody else keeps beside Conary's.
    let b = c("b").to_string();
    let fanned = format!("{}/{}", &b[..2], &b[2..]);
    let stale = Note::of(
        store.ledger().change(first).unwrap(),
        store
            .ledger()
            .change(first)
            .unwrap()
            .landing
            .as_ref()
            .unwrap(),
        Links::Kept,
    );
    fake.files.borrow_mut().insert(fanned.clone(), stale.text());
    fake.files
        .borrow_mut()
        .insert("someone-elses".into(), "kept\n".into());
    *fake.commits.borrow_mut() = 1;

    // New evidence on PR 1's landed revision changes its note.
    store
        .record(Body::Evidence {
            change: first,
            revision: RevisionId::new(&c("a"), &c("9")),
            kind: CheckKind::Ci,
            check: "flake".into(),
            result: Outcome::Pass,
            source: "https://github.com/FieldmouseWorks/burrow/runs/1".into(),
        })
        .unwrap();
    assert_eq!(
        publish(&fake, store.ledger(), &burrow(), Links::Kept).unwrap(),
        2
    );
    let files = fake.files.borrow();
    assert!(
        !files.contains_key(&b),
        "written where it was, not beside it"
    );
    assert_eq!(Note::parse(&files[&fanned]).unwrap().evidence.len(), 1);
    assert_eq!(files["someone-elses"], "kept\n");
}

#[test]
fn a_repo_that_keeps_session_links_out_gets_notes_without_them() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = store(dir.path());
    let fake = Fake::default();
    publish(&fake, store.ledger(), &burrow(), Links::Left).unwrap();
    for text in fake.files.borrow().values() {
        assert!(!text.contains("claude.ai"), "{text}");
    }
}

#[test]
fn a_ref_moved_since_it_was_read_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = store(dir.path());
    let fake = Fake::default();
    *fake.raced.borrow_mut() = true;
    assert!(publish(&fake, store.ledger(), &burrow(), Links::Kept).is_err());
}
