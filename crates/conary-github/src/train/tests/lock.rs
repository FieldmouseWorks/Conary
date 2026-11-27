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
    let anchor = initial.head();
    conary_core::log::save_anchor(initial.writer().unwrap().lock(), anchor, "c1").unwrap();
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

// A newer-policy R2 classification appended before `land_next` merges a member
// of a mixed candidate must stop the landing and reseat the PR alone, never
// land it in the mixed candidate.
#[test]
fn a_mixed_candidate_does_not_land_a_pr_raised_to_r2_by_a_newer_policy_before_merge() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);

    let change_3 = store.ledger().by_pr(&repo(), 3).unwrap().id;
    let rev_3 = store.ledger().by_pr(&repo(), 3).unwrap().current.unwrap();
    let ledger_dir = dir.path().to_owned();
    let raced = std::rc::Rc::new(std::cell::Cell::new(false));
    let raced_in = std::rc::Rc::clone(&raced);
    // Fires after `land_next`'s initial `below_class` check and right before
    // `forge.mark(Landing)` / `forge.land`.
    hub.race(Call::CompareCommits, move |_| {
        let mut other = Store::open(
            conary_core::log::Lock::take(&ledger_dir).unwrap(),
            SigningKey::from_bytes(&[5; 32]),
            None,
        )
        .unwrap();
        other
            .record(Body::Classify {
                change: change_3,
                revision: rev_3,
                class: Class::R2,
                because: Some("newer policy makes src/ R2".into()),
                docs: false,
                policy: "b".repeat(64),
                raised: vec![],
            })
            .unwrap();
        raced_in.set(true);
    });

    let stepped = step_once(&mut store, &hub);
    assert!(raced.get());
    assert!(
        stepped.landed.is_empty(),
        "PR 3 landed in a mixed candidate despite R2 upgrade: {stepped:?}"
    );
    assert_eq!(stepped.reseated, [3]);
    assert_eq!(hub.merged(REMI, 3), None);
}

// `class::class` (used by `cn class`) serializes under the same repository's
// train lock so a class upgrade cannot land between a step's checks and its
// merge, while another repository's classification records freely.
#[test]
fn class_write_waits_for_a_running_step_of_the_same_repository_while_other_repo_classifies_freely()
{
    let dir = tempfile::tempdir().unwrap();
    let ledger_dir = dir.path().to_owned();
    let mut initial = store(&dir);
    let setup_hub = github(&[3]);
    join_all(&mut initial, &setup_hub, &[3]);
    let (candidate_id, _) = gating(&step_once(&mut initial, &setup_hub));
    let commit = commit_of(&initial, candidate_id);
    let branch = train_branch(candidate_id);
    let anchor = initial.head();
    conary_core::log::save_anchor(initial.writer().unwrap().lock(), anchor, "c1").unwrap();
    drop(initial);

    let step_active = Arc::new(AtomicBool::new(false));
    let classified_while_active = Arc::new(AtomicBool::new(false));
    let class_done = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let step_thread = {
        let dir = ledger_dir.clone();
        let step_active = Arc::clone(&step_active);
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
            let active_in = Arc::clone(&step_active);
            hub.race(Call::DefaultBranch, move |_| {
                active_in.store(true, SeqCst);
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            step(&mut s, &hub, &hub, &config(), &repo(), &settings()).unwrap();
            step_active.store(false, SeqCst);
        })
    };

    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // Thread 2: classifies PR 3 on the same repo under a newer R2 policy via
    // `class::ask` + `class::class` (the exact path `cn class` runs).
    let class_thread = {
        let dir = ledger_dir.clone();
        let step_active = Arc::clone(&step_active);
        let classified_while_active = Arc::clone(&classified_while_active);
        let class_done = Arc::clone(&class_done);
        std::thread::spawn(move || {
            let hub = github(&[3]);
            let r2_policy = Policy::parse(br#"{"r2": ["^src/"]}"#).unwrap();
            let key = SigningKey::from_bytes(&[5; 32]);
            let mut reader = conary_core::reader::Reader::open(&dir, key.verifying_key()).unwrap();
            let asked = crate::class::ask(
                &mut reader,
                &hub,
                &config(),
                &r2_policy,
                &repo(),
                3,
                &head(3),
                &dir,
            )
            .unwrap();
            drop(reader);
            let crate::class::Asked::Read(read) = asked else {
                panic!("expected fresh classification under newer policy");
            };
            let mut s =
                Store::open(conary_core::log::Lock::take(&dir).unwrap(), key, None).unwrap();
            let got = crate::class::class(&mut s, &hub, &r2_policy, *read).unwrap();
            assert_eq!(got.said.class, Class::R2);
            if step_active.load(SeqCst) {
                classified_while_active.store(true, SeqCst);
            }
            class_done.store(true, SeqCst);
        })
    };

    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !class_done.load(SeqCst),
        "same-repo class::class recorded while train::step held the repo lock"
    );

    // Meanwhile, classifying a PR in another repository proceeds freely.
    let other_repo: Repo = "FieldmouseWorks/other".parse().unwrap();
    let other_hub = Hub::new("FieldmouseWorks/other", TRUNK);
    other_hub.pr("FieldmouseWorks/other", 9, &head(9));
    let other_policy = Policy::parse(br#"{"r2": ["^src/"]}"#).unwrap();
    let key = SigningKey::from_bytes(&[5; 32]);
    let mut reader = conary_core::reader::Reader::open(&ledger_dir, key.verifying_key()).unwrap();
    let asked = crate::class::ask(
        &mut reader,
        &other_hub,
        &config(),
        &other_policy,
        &other_repo,
        9,
        &head(9),
        &ledger_dir,
    )
    .unwrap();
    drop(reader);
    let crate::class::Asked::Read(read) = asked else {
        panic!("expected fresh classification on other repo");
    };
    let mut other_store = Store::open(
        conary_core::log::Lock::take(&ledger_dir).unwrap(),
        key,
        None,
    )
    .unwrap();
    let other_got =
        crate::class::class(&mut other_store, &other_hub, &other_policy, *read).unwrap();
    assert_eq!(other_got.said.class, Class::R2);

    assert!(
        !class_done.load(SeqCst),
        "same-repo class::class finished before step ended"
    );

    release_tx.send(()).unwrap();
    step_thread.join().unwrap();
    class_thread.join().unwrap();
    assert!(class_done.load(SeqCst));
    assert!(!classified_while_active.load(SeqCst));
}
