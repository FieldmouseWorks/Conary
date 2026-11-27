//! Concurrent steps: two steps of one repository never overlap, while another
//! repository's writer and another repository's step run freely.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::mpsc;
use std::time::Duration;

use super::*;

#[test]
fn two_steps_of_one_repository_never_overlap_while_another_records_and_steps_freely() {
    let dir = tempfile::tempdir().unwrap();
    let ledger_dir = dir.path().to_owned();
    let mut initial = store(&dir);
    let setup_hub = github(&[3]);
    join_all(&mut initial, &setup_hub, &[3]);
    let (candidate_id, _) = gating(&step_once(&mut initial, &setup_hub));
    let commit = commit_of(&initial, candidate_id);
    let branch = train_branch(candidate_id);
    drop(initial);

    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let second_entered = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    // Thread 1: enters `step` on FieldmouseWorks/remi and pauses inside its
    // first GitHub read (`Call::DefaultBranch`).
    let first = {
        let dir = ledger_dir.clone();
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        let commit = commit.clone();
        let branch = branch.clone();
        std::thread::spawn(move || {
            let mut s = Store::open(
                conary_core::log::Lock::take(&dir).unwrap(),
                SigningKey::from_bytes(&[5; 32]),
                None,
            )
            .unwrap();
            let hub = github(&[3]);
            hub.put_commit(&commit, &[TRUNK], true);
            hub.set_branch(REMI, &branch, &commit);
            let active_in = Arc::clone(&active);
            let max_in = Arc::clone(&max_active);
            hub.race(Call::DefaultBranch, move |_| {
                let now = active_in.fetch_add(1, SeqCst) + 1;
                max_in.fetch_max(now, SeqCst);
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            let stepped = step(&mut s, &hub, &hub, &config(), &repo(), &settings()).unwrap();
            active.fetch_sub(1, SeqCst);
            stepped
        })
    };

    // Wait until thread 1 is paused inside its GitHub read.
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // Thread 2: starts `step` on the same repository (with different casing,
    // exercising case-insensitive lock keys). It must wait until thread 1
    // finishes its step.
    let second = {
        let dir = ledger_dir.clone();
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        let second_entered = Arc::clone(&second_entered);
        let commit = commit.clone();
        let branch = branch.clone();
        std::thread::spawn(move || {
            let mut s = Store::open(
                conary_core::log::Lock::take(&dir).unwrap(),
                SigningKey::from_bytes(&[5; 32]),
                None,
            )
            .unwrap();
            let same_repo_other_case: Repo = "fieldmouseworks/REMI".parse().unwrap();
            let hub = Hub::new("fieldmouseworks/REMI", TRUNK);
            hub.put_commit(&commit, &[TRUNK], true);
            hub.set_branch("fieldmouseworks/REMI", &branch, &commit);
            let active_in = Arc::clone(&active);
            let max_in = Arc::clone(&max_active);
            let entered = Arc::clone(&second_entered);
            hub.race(Call::DefaultBranch, move |_| {
                let now = active_in.fetch_add(1, SeqCst) + 1;
                max_in.fetch_max(now, SeqCst);
                entered.store(true, SeqCst);
            });
            let stepped = step(
                &mut s,
                &hub,
                &hub,
                &config(),
                &same_repo_other_case,
                &settings(),
            )
            .unwrap();
            active.fetch_sub(1, SeqCst);
            stepped
        })
    };

    // Give thread 2 time to reach the repository's train lock and block on it.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !second_entered.load(SeqCst),
        "second step of the same repository overlapped the first"
    );

    // Meanwhile, another repository's writer records freely AND another
    // repository's train step runs freely without waiting on thread 1.
    let other_repo: Repo = "FieldmouseWorks/other".parse().unwrap();
    let mut other_store = Store::open(
        conary_core::log::Lock::take(&ledger_dir).unwrap(),
        SigningKey::from_bytes(&[5; 32]),
        None,
    )
    .unwrap();
    other_store
        .record(Body::Observe {
            change: ChangeId::new().unwrap(),
            repo: other_repo.clone(),
            pr: 7,
            author: conary_core::op::Actor {
                kind: conary_core::op::ActorKind::Person,
                login: "someone".into(),
                session: None,
            },
        })
        .unwrap();
    let other_hub = Hub::new("FieldmouseWorks/other", TRUNK);
    let other_stepped = step(
        &mut other_store,
        &other_hub,
        &other_hub,
        &config(),
        &other_repo,
        &settings(),
    )
    .unwrap();
    assert_eq!(other_stepped.now, Now::Idle);

    // Thread 2 is still excluded while thread 1 holds the repo's train lock.
    assert!(
        !second_entered.load(SeqCst),
        "second step of the same repository entered before first finished"
    );

    // Release thread 1; thread 2 now runs to completion.
    release_tx.send(()).unwrap();
    let first_stepped = first.join().unwrap();
    let second_stepped = second.join().unwrap();
    assert!(second_entered.load(SeqCst));
    assert_eq!(max_active.load(SeqCst), 1);
    assert!(matches!(first_stepped.now, Now::Gating { .. }));
    assert!(matches!(second_stepped.now, Now::Gating { .. }));

    // Both repositories' ops are in the ledger cleanly.
    other_store.refresh().unwrap();
    assert!(other_store.ledger().by_pr(&other_repo, 7).is_some());
    assert!(
        other_store
            .ledger()
            .train(&repo())
            .and_then(|t| t.in_flight())
            .is_some()
    );
}
