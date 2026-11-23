//! GitHub changes while a pass waits for the ledger's lock, which another
//! writer holds: whatever the pass saw before the wait permits nothing after
//! it. The other writer records nothing, so only GitHub can tell.
//!
//! The change is made exactly when the pass is blocked, as the syscall
//! `/proc` shows its thread in says, so before anything the pass does next.
use super::flow_pass_case::{Case, REPO};
use crate::flow::{SETTLED, jobs};
use crate::sim::{Call, Hub, unblocked};
use conary_core::log;
use conary_core::op::Body;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::time::Duration;

const GATE: &str = "fresh / check";

/// What the pass does once it's permitted.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Effect {
    Merge,
    Join,
    /// The step at the pass's end, landing a green candidate.
    Step,
}
const EFFECTS: [Effect; 3] = [Effect::Merge, Effect::Join, Effect::Step];

/// What changes on GitHub while the pass waits.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Change {
    R2,
    Hold,
    ChangesRequested,
    Thread,
    BaseMoved,
    EvidenceGone,
    /// The settled status, withdrawn by someone else.
    Withdrawn,
    /// The PR can't be read any more.
    Unreadable,
}

impl Change {
    fn make(self, hub: &Hub) {
        let head = super::head(3);
        match self {
            Self::R2 => hub.label(REPO, 3, &[conary_core::policy::R2_LABEL]),
            Self::Hold => hub.label(REPO, 3, &[&crate::sim::settings().hold_label]),
            Self::ChangesRequested => hub.request_changes(REPO, 3, "maintainer"),
            Self::Thread => hub.flow_thread_at(
                REPO,
                3,
                "PRRT_waited",
                ("src/lib.rs", 1),
                ("maintainer", "P1: this loses data"),
            ),
            Self::BaseMoved => {
                let tip = "e".repeat(40);
                hub.put_commit(&tip, &[super::TRUNK], true);
                hub.set_branch(REPO, "main", &tip);
            }
            Self::EvidenceGone => hub.forget_artifacts(None),
            Self::Withdrawn => hub.post_status(
                &head,
                json!({"context":SETTLED, "state":"pending", "created_at":hub.now(),
                    "description":"withdrawn", "creator":{"login":crate::sim::STEWARD,"type":"Bot"}}),
            ),
            Self::Unreadable => hub.fail(Call::FlowSnapshot),
        }
    }
}

/// A case whose next pass makes `effect`, and its ledger's directory.
fn ready(effect: Effect) -> (Case, PathBuf) {
    let mut c = Case::new();
    c.clean_review();
    if effect != Effect::Merge {
        c.config.settings.train = true;
        c.config.evidence.as_mut().unwrap().require = vec![GATE.into()];
    }
    if effect == Effect::Step {
        c.pass(jobs::now(), false);
        let commit = {
            let store = c.store.borrow();
            let ledger = store.ledger();
            let train = ledger.train(&REPO.parse().unwrap()).unwrap();
            let id = train.in_flight().expect("the first pass built a candidate");
            ledger.candidate(id).unwrap().commit.to_string()
        };
        c.hub.ci(&commit, &[(GATE, "success")]);
        for run in c.hub.checks_on(&commit) {
            c.hub.forget_artifacts(Some(run.id));
            c.attest_run(&commit, run.id, None);
        }
    }
    let dir = c.store.borrow().dir().to_owned();
    (c, dir)
}

fn ops(dir: &Path) -> Vec<conary_core::op::Op> {
    let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]).verifying_key();
    log::read(dir, &key, None).unwrap()
}

/// Whether `effect` is made.
fn done(effect: Effect, hub: &Hub, dir: &Path) -> bool {
    match effect {
        Effect::Merge | Effect::Step => hub.count(Call::Land) > 0,
        Effect::Join => ops(dir)
            .iter()
            .any(|o| matches!(o.op.body, Body::Join { .. })),
    }
}

/// How often PR 3's head was given a settled success.
fn settled(hub: &Hub) -> usize {
    hub.statuses_json(&super::head(3))
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["context"] == SETTLED && s["state"] == "success")
        .count()
}

/// `flock`'s number among the syscalls.
#[cfg(target_arch = "x86_64")]
const FLOCK: &str = "73";
#[cfg(target_arch = "aarch64")]
const FLOCK: &str = "32";

/// Where `/proc` says what the calling thread is doing.
fn this_thread() -> PathBuf {
    Path::new("/proc")
        .join(std::fs::read_link("/proc/thread-self").unwrap())
        .join("syscall")
}

/// Whether the thread `syscall` is of waits for a lock: in `flock`, asked
/// for it exclusive (`LOCK_EX`), waiting.
fn awaited(syscall: &Path) -> bool {
    std::fs::read_to_string(syscall).is_ok_and(|said| {
        let mut said = said.split_whitespace();
        said.next() == Some(FLOCK) && said.nth(1) == Some("0x2")
    })
}

/// The calls of a pass left alone at which another writer could take the
/// lock and the effect isn't made yet.
fn free(effect: Effect) -> Vec<usize> {
    let (c, dir) = ready(effect);
    let free: Rc<RefCell<Vec<usize>>> = Rc::default();
    let (seen, at, calls) = (Rc::clone(&free), dir.clone(), Cell::new(0));
    c.hub.watch(move |hub| {
        let call = calls.replace(calls.get() + 1);
        let lock = std::fs::File::open(at.join("lock")).unwrap();
        if lock.try_lock().is_ok() {
            if !done(effect, hub, &at) {
                seen.borrow_mut().push(call);
            }
            // Now, not when a child another test forks closes its copy.
            let _ = lock.unlock();
        }
    });
    c.pass(jobs::now(), false);
    assert!(done(effect, &c.hub, &dir), "{effect:?}, left alone");
    free.take()
}

/// What a pass did after `change` was made as it waited.
#[derive(Debug)]
struct Waited {
    /// The call the pass made first after its wait, the change made just before it.
    after: usize,
    /// Whether the effect was made after the change.
    acted: bool,
    /// Whether a settled success was written after it.
    granted: bool,
    error: Option<String>,
    /// PR 3's settled state at the pass's end.
    status: Option<String>,
    degraded: bool,
}

/// A pass on which another writer takes the lock just before call `at`, and
/// lets it go once the pass waits for it, `change` made by then. `None` when
/// the pass didn't wait, or made the effect before it did.
fn waited(effect: Effect, change: Change, at: usize) -> Option<Waited> {
    let (c, dir) = ready(effect);
    let (blocked, over) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    // Set when the change is made before the effect: the call it's made
    // before, and the settled successes then.
    let before: Rc<Cell<Option<(usize, usize)>>> = Rc::default();
    let (wait, ended, seen, calls) = (
        Arc::clone(&blocked),
        Arc::clone(&over),
        Rc::clone(&before),
        Cell::new(0),
    );
    let (ledger, changed) = (dir.clone(), Cell::new(false));
    c.hub.watch(move |hub| {
        let call = calls.replace(calls.get() + 1);
        if call == at {
            let lock = std::fs::File::open(ledger.join("lock")).unwrap();
            if lock.try_lock().is_err() {
                return;
            }
            let pass = this_thread();
            let (wait, ended) = (Arc::clone(&wait), Arc::clone(&ended));
            std::thread::spawn(move || {
                while !ended.load(SeqCst) {
                    if awaited(&pass) {
                        wait.store(true, SeqCst);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                let _ = lock.unlock();
            });
        }
        if wait.load(SeqCst) && !changed.replace(true) && !done(effect, hub, &ledger) {
            seen.set(Some((call, settled(hub))));
            change.make(hub);
        }
    });
    let reports = c.pass(jobs::now(), false);
    over.store(true, SeqCst);
    let (after, successes) = before.get()?;
    Some(Waited {
        after,
        acted: done(effect, &c.hub, &dir),
        granted: settled(&c.hub) > successes,
        error: reports
            .into_iter()
            .find(|r| r.pr == 3)
            .and_then(|r| r.error),
        status: crate::api::Source::statuses(&c.hub, REPO, &super::head(3))
            .unwrap()
            .into_iter()
            .find(|s| s.context == SETTLED)
            .map(|s| s.state),
        degraded: ops(&dir)
            .iter()
            .any(|o| matches!(o.op.body, Body::Degraded { .. })),
    })
}

/// `effect` with each change made as the pass waits after its last call
/// outside the lock: the wait for the hold the effect is made under.
fn refused(effect: Effect, changes: &[Change]) {
    let last = *free(effect).last().expect("a call outside the lock");
    let mut wrong = Vec::new();
    for &change in changes {
        let got = waited(effect, change, last).expect("the pass waited before its effect");
        // The train itself takes out a PR whose settled status was withdrawn.
        let left = effect == Effect::Step && change == Change::Withdrawn;
        if got.acted
            || got.granted
            || got.status.as_deref() != Some("pending")
            || (got.error.is_none() && !left)
            || (change == Change::Unreadable && !got.degraded)
        {
            wrong.push(format!("{effect:?}, {change:?}: {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_merge_isnt_made_on_what_the_pass_saw_before_it_waited() {
    unblocked(120, || {
        refused(
            Effect::Merge,
            &[
                Change::R2,
                Change::Hold,
                Change::ChangesRequested,
                Change::Thread,
                Change::BaseMoved,
                Change::EvidenceGone,
                Change::Withdrawn,
                Change::Unreadable,
            ],
        );
    });
}

#[test]
fn a_join_isnt_made_on_what_the_pass_saw_before_it_waited() {
    unblocked(120, || {
        refused(
            Effect::Join,
            &[
                Change::R2,
                Change::Hold,
                Change::ChangesRequested,
                Change::Thread,
                Change::BaseMoved,
                Change::EvidenceGone,
                Change::Unreadable,
            ],
        );
    });
}

#[test]
fn a_train_isnt_stepped_on_what_the_pass_saw_before_it_waited() {
    unblocked(120, || {
        refused(
            Effect::Step,
            &[
                Change::R2,
                Change::Hold,
                Change::ChangesRequested,
                Change::Thread,
                Change::BaseMoved,
                Change::EvidenceGone,
                Change::Withdrawn,
                Change::Unreadable,
            ],
        );
    });
}

// The rule at every wait a pass makes before its effect, not only the last:
// changes the looks read, made at each. Changes made anywhere between two
// waits are made at the second, so one pass each is enough.
#[test]
fn no_wait_of_a_pass_comes_between_a_look_and_what_it_permits() {
    unblocked(300, || {
        let mut wrong = Vec::new();
        for effect in EFFECTS {
            let free = free(effect);
            // What the scope reads, and what only the evidence's look does.
            for change in [Change::Hold, Change::EvidenceGone] {
                let mut waits = 0;
                let mut at = free[0];
                while let Some(&next) = free.iter().find(|&&call| call >= at) {
                    let Some(got) = waited(effect, change, next) else {
                        break;
                    };
                    waits += 1;
                    if got.acted || got.granted {
                        wrong.push(format!("{effect:?}, {change:?}, at {next}: {got:?}"));
                    }
                    at = got.after.max(next + 1);
                }
                assert!(waits > 1, "{effect:?}, {change:?}: {waits} waits");
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    });
}
