//! A sync of several repos on one open of the ledger ([`sync_all`]): the
//! log read once, each repo synced under a lock taken again and checked
//! against the anchor, and each repo's failure its own.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use conary_core::Ledger;
use conary_core::log;
use conary_core::notes::Links;
use conary_core::op::{Actor, ActorKind};

use super::*;
use crate::anchor::{self, AnchorError, REF};

const ANCHOR: &str = "FieldmouseWorks/ledger";
const REPOS: [&str; 3] = [
    "FieldmouseWorks/remi",
    "FieldmouseWorks/burrow",
    "FieldmouseWorks/Conary",
];
/// Ops in the log before a pass.
const BEFORE: usize = 200;

/// GitHub with three repos, each with PR 1 open, and a ledger anchored in
/// [`ANCHOR`] holding [`BEFORE`] ops.
fn world() -> (tempfile::TempDir, Hub, SigningKey) {
    let hub = Hub::new(REPOS[0], &format!("{:040}", 1));
    for (i, repo) in REPOS.iter().enumerate() {
        if i > 0 {
            hub.add_repo(repo, "main", &format!("{:040}", i + 1));
        }
        hub.pr(repo, 1, &format!("{:040x}", 0xa0 + i));
    }
    let dir = tempfile::tempdir().unwrap();
    anchor::init(dir.path(), &log::key_path(dir.path()), &hub, ANCHOR).unwrap();
    let key = log::load_signing_key(&log::key_path(dir.path())).unwrap();
    let mut a = anchor::open(dir.path(), key.clone(), &hub, ANCHOR).unwrap();
    for pr in 0..BEFORE as u64 {
        observe(&mut a.store, pr);
    }
    a.publish(&hub).unwrap();
    (dir, hub, key)
}

/// An op about a PR in another repo.
fn observe(store: &mut Store, pr: u64) {
    store
        .record(Body::Observe {
            change: ChangeId::new().unwrap(),
            repo: "FieldmouseWorks/lok".parse().unwrap(),
            pr,
            author: Actor {
                kind: ActorKind::Person,
                login: "TusanHomichi".into(),
                session: None,
            },
        })
        .unwrap();
}

fn targets() -> Vec<Target> {
    REPOS
        .iter()
        .map(|r| Target {
            repo: r.parse().unwrap(),
            links: Links::Left,
        })
        .collect()
}

/// What each repo's sync ended as, by name: `done`, `unavailable` or
/// `failed`.
type Ends = Vec<(String, &'static str)>;

fn word(ended: &Ended) -> &'static str {
    match ended {
        Ended::Done(_) => "done",
        Ended::Unavailable(_) => "unavailable",
        Ended::Failed(_) => "failed",
    }
}

/// Syncs [`REPOS`], and returns how each ended, in order.
fn pass(dir: &Path, hub: &Hub) -> (Ends, Result<(), Stopped>) {
    let mut ends = Vec::new();
    let passed = sync_all(dir, ANCHOR, hub, &config(), &targets(), |r, e| {
        ends.push((r.to_string(), word(&e)));
    });
    (ends, passed)
}

fn all_done() -> Ends {
    REPOS.iter().map(|r| (r.to_string(), "done")).collect()
}

/// The ledger as the log holds it, and the log's head.
fn ledger(dir: &Path, key: &SigningKey) -> (Ledger, Option<log::Anchor>) {
    let ops = log::read(dir, &key.verifying_key(), None).unwrap();
    let head = ops.last().map(|o| log::Anchor {
        seq: ops.len(),
        op: o.id,
    });
    let ledger = Ledger::replay(&ops).unwrap();
    (ledger, head)
}

/// Whether the ledger records PR 1 of `repo`.
fn recorded(ledger: &Ledger, repo: &str) -> bool {
    ledger.by_pr(&repo.parse().unwrap(), 1).is_some()
}

/// Asserts the log's head is anchored, on GitHub and beside the log.
fn anchored(dir: &Path, hub: &Hub, key: &SigningKey) {
    let (_, head) = ledger(dir, key);
    let on_github = anchor::verify(dir, &key.verifying_key(), hub, ANCHOR).unwrap();
    assert_eq!(on_github, head);
    assert_eq!(log::load_anchor(dir).unwrap(), head);
}

#[test]
fn a_pass_reads_the_log_once_however_many_repos_it_syncs() {
    let (dir, hub, key) = world();
    let read = log::lines_read();
    let (ends, passed) = pass(dir.path(), &hub);
    let read = log::lines_read() - read;
    passed.unwrap();
    assert_eq!(ends, all_done());
    let (ledger, head) = ledger(dir.path(), &key);
    assert!(REPOS.iter().all(|r| recorded(&ledger, r)));
    let appended = head.unwrap().seq - BEFORE;
    assert!(appended >= REPOS.len(), "{appended}");
    // The log once, and none of the ops the pass wrote itself: an open per
    // repo would read it once each, and each one's ops after.
    assert_eq!(read, BEFORE as u64);
    anchored(dir.path(), &hub, &key);
}

/// Whether the ledger's lock in `dir` is free now.
fn free(dir: &Path) -> bool {
    std::fs::File::open(dir.join("lock")).is_ok_and(|f| {
        let free = f.try_lock().is_ok();
        let _ = f.unlock();
        free
    })
}

#[test]
fn another_writer_records_during_a_later_gather_and_the_pass_catches_up_on_it() {
    let (dir, hub, key) = world();
    let (at, other) = (dir.path().to_owned(), key.clone());
    hub.race(Call::Pulls.repo(REPOS[1]), move |_| {
        assert!(free(&at), "the lock was held through a gather");
        // Its own thread, so its open of the log isn't counted as the pass's.
        std::thread::spawn(move || {
            let lock = log::Lock::take(&at).unwrap();
            let mut store = Store::open(lock, other, None).unwrap();
            observe(&mut store, 9999);
        })
        .join()
        .unwrap();
    });
    let read = log::lines_read();
    let (ends, passed) = pass(dir.path(), &hub);
    let read = log::lines_read() - read;
    passed.unwrap();
    assert_eq!(ends, all_done());
    let (ledger, _) = ledger(dir.path(), &key);
    let lok = "FieldmouseWorks/lok".parse().unwrap();
    assert!(ledger.by_pr(&lok, 9999).is_some());
    assert!(REPOS.iter().all(|r| recorded(&ledger, r)));
    // The log once, then only the other writer's op.
    assert_eq!(read, BEFORE as u64 + 1);
    anchored(dir.path(), &hub, &key);
}

/// What a repo's sync can change: the log, the copy of the anchor beside
/// it, the anchor's ref and the notes refs.
#[derive(Debug, PartialEq)]
struct Written {
    /// The log's length and a hash of its bytes.
    log: (usize, u64),
    copy: String,
    ref_moves: usize,
    notes: usize,
}

fn written(dir: &Path, hub: &Hub) -> Written {
    Written {
        log: {
            let bytes = std::fs::read(log::log_path(dir)).unwrap();
            let mut hash = std::hash::DefaultHasher::new();
            std::hash::Hash::hash(&bytes, &mut hash);
            (bytes.len(), std::hash::Hasher::finish(&hash))
        },
        copy: std::fs::read_to_string(log::anchor_path(dir)).unwrap(),
        ref_moves: hub.count(Call::SetRef),
        notes: hub.count(Call::SetNotes),
    }
}

/// The calls listing `repo`'s PRs.
fn listed(hub: &Hub, repo: &str) -> usize {
    hub.asked(Call::Pulls)
        .iter()
        .filter(|a| a.first().map(String::as_str) == Some(repo))
        .count()
}

#[test]
fn a_ref_put_back_between_two_repos_is_refused_before_anything_is_written() {
    let (dir, hub, key) = world();
    // Where the ref was before the first repo moved it on: a move whose op
    // the log holds, but not the one the first repo checked and made.
    let was = hub.ref_at(ANCHOR, REF).unwrap();
    let seen: Rc<RefCell<Option<Written>>> = Rc::default();
    let (at, then) = (dir.path().to_owned(), Rc::clone(&seen));
    hub.race(Call::Pulls.repo(REPOS[1]), move |hub| {
        hub.put_back_ref(ANCHOR, REF, &was);
        *then.borrow_mut() = Some(written(&at, hub));
    });
    let (ends, passed) = pass(dir.path(), &hub);
    let stopped = passed.unwrap_err();
    assert!(matches!(stopped.error, AnchorError::Lost(_)), "{stopped}");
    let unsynced: Vec<String> = stopped.unsynced.iter().map(Repo::to_string).collect();
    assert_eq!(unsynced, REPOS[1..]);
    assert_eq!(ends, all_done()[..1]);
    let before = seen.take().expect("the second repo was gathered");
    // Nothing written since its gather began, and its PRs listed only by
    // that gather, never again under the lock.
    assert_eq!(written(dir.path(), &hub), before);
    assert_eq!(listed(&hub, REPOS[1]), 1);
    let (ledger, _) = ledger(dir.path(), &key);
    assert!(recorded(&ledger, REPOS[0]));
    assert!(!REPOS[1..].iter().any(|r| recorded(&ledger, r)));
}

#[test]
fn github_unavailable_for_the_anchor_stops_the_pass_as_unavailable() {
    let (dir, hub, key) = world();
    hub.race(Call::Pulls.repo(REPOS[1]), |hub| hub.fail(Call::Ref));
    let (ends, passed) = pass(dir.path(), &hub);
    let stopped = passed.unwrap_err();
    assert!(crate::api::unavailable(&stopped), "{stopped}");
    let unsynced: Vec<String> = stopped.unsynced.iter().map(Repo::to_string).collect();
    assert_eq!(unsynced, REPOS[1..]);
    assert_eq!(ends, all_done()[..1]);
    hub.heal(Call::Ref);
    let (ledger, _) = ledger(dir.path(), &key);
    assert!(recorded(&ledger, REPOS[0]));
    assert!(!REPOS[1..].iter().any(|r| recorded(&ledger, r)));
    anchored(dir.path(), &hub, &key);
}

#[test]
fn an_unavailable_and_a_failed_repo_leave_the_third_synced() {
    let (dir, hub, key) = world();
    hub.fail(Call::Pulls.repo(REPOS[0]));
    // Read again under the lock, after its gather couldn't.
    hub.refuse_with(Call::Timeline.repo(REPOS[1]), "Resource not accessible");
    let (ends, passed) = pass(dir.path(), &hub);
    passed.unwrap();
    assert_eq!(
        ends,
        [
            (REPOS[0].to_owned(), "unavailable"),
            (REPOS[1].to_owned(), "failed"),
            (REPOS[2].to_owned(), "done"),
        ]
    );
    let (ledger, _) = ledger(dir.path(), &key);
    assert!(recorded(&ledger, REPOS[2]));
    anchored(dir.path(), &hub, &key);
}

#[test]
fn a_pass_killed_after_its_first_repo_leaves_it_recorded_and_anchored() {
    let (dir, hub, key) = world();
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sync_all(dir.path(), ANCHOR, &hub, &config(), &targets(), |_, _| {
            panic!("killed")
        })
    }));
    assert!(killed.is_err());
    let (ledger, _) = ledger(dir.path(), &key);
    assert!(recorded(&ledger, REPOS[0]));
    assert!(!REPOS[1..].iter().any(|r| recorded(&ledger, r)));
    anchored(dir.path(), &hub, &key);
}
