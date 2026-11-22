//! A failed gate's second try, and the release of a change red alone once
//! its gate passes after all (Conary#554).

use super::*;

const GATE: &str = "check / check";

/// PR #3 gating with its gate's first run ended in `conclusion`.
fn ended(dir: &tempfile::TempDir, conclusion: &str) -> (Store, Hub, CandidateId, String) {
    let mut store = store(dir);
    let hub = github(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let commit = commit_of(&store, id);
    hub.ci(&commit, &[(GATE, conclusion)]);
    (store, hub, id, commit)
}

/// PR #3 red alone, both its tries failed.
fn red(dir: &tempfile::TempDir) -> (Store, Hub, CandidateId, String) {
    let (mut store, hub, id, commit) = ended(dir, "failure");
    hub.fails_again(&commit);
    assert_eq!(step_once(&mut store, &hub).verdicts, [(id, Verdict::Red)]);
    (store, hub, id, commit)
}

fn gets_a_second_try() -> Vec<String> {
    vec![format!("{GATE}: it failed, and gets a second try")]
}

#[test]
fn a_gates_first_failure_is_run_again_once_however_often_its_stepped() {
    for conclusion in [
        "failure",
        "cancelled",
        "timed_out",
        "startup_failure",
        "stale",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, hub, id, _) = ended(&dir, conclusion);
        let stepped = step_once(&mut store, &hub);
        assert_eq!(waiting(&stepped), gets_a_second_try(), "{conclusion}");
        assert!(stepped.verdicts.is_empty() && hub.marked().is_empty());
        assert_eq!(hub.count(Call::FlowRerun), 1);
        // The rerun is queued, and is the check's word now.
        assert_eq!(waiting(&step_once(&mut store, &hub)), [GATE]);
        assert_eq!(hub.count(Call::FlowRerun), 1);
        assert_eq!(store.ledger().candidate(id).unwrap().state, State::Gating);
    }
}

#[test]
fn a_second_trys_pass_counts_by_its_own_attestation_not_the_first_trys_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, id, commit) = ended(&dir, "failure");
    step_once(&mut store, &hub);
    hub.finish_again(&commit, "success");
    let runs = hub.checks_on(&commit);
    let (first, second) = (runs[0].id, runs[1].id);
    // A run keeps both attempts' artifacts: the first's, signed a failure.
    let statement = testing::statement(&commit, &hub.tree_at(&commit), "fail");
    let claims = testing::claims(REMI, &commit, first, &statement);
    hub.attest(second, &testing::file(&statement, &testing::token(&claims)));
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, []);
    assert!(
        waiting(&stepped)[0].starts_with(GATE),
        "the failure counted"
    );
    hub.attest(second, &hub.attestation(&commit, second));
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
    assert_eq!(stepped.landed, [3]);
}

#[test]
fn a_second_failure_is_red_and_isnt_run_a_third_time() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, id, commit) = ended(&dir, "failure");
    step_once(&mut store, &hub);
    hub.finish_again(&commit, "failure");
    assert_eq!(step_once(&mut store, &hub).verdicts, [(id, Verdict::Red)]);
    assert_eq!(hub.count(Call::FlowRerun), 1);
}

#[test]
fn a_second_try_github_wont_make_leaves_the_failure_red() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, id, _) = ended(&dir, "failure");
    hub.refuse_with(Call::FlowRerun, "Unable to re-run this workflow run");
    assert_eq!(step_once(&mut store, &hub).verdicts, [(id, Verdict::Red)]);
}

#[test]
fn a_second_try_refused_as_already_running_is_waited_for() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, _, commit) = ended(&dir, "failure");
    let suite = hub.checks_on(&commit)[0].check_suite.as_ref().unwrap().id;
    hub.race(Call::FlowRerun, move |hub| hub.run_again(suite));
    hub.refuse_with(Call::FlowRerun, "This workflow is already running");
    assert_eq!(waiting(&step_once(&mut store, &hub)), gets_a_second_try());
}

#[test]
fn github_out_of_reach_or_behind_gives_no_verdict_and_no_second_ask() {
    // The ask's answer is lost: the step fails, and the next finds it made.
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, id, _) = ended(&dir, "failure");
    hub.lose(Call::FlowRerun);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(store.ledger().candidate(id).unwrap().state, State::Gating);
    assert_eq!(waiting(&step_once(&mut store, &hub)), [GATE]);
    assert_eq!(hub.count(Call::FlowRerun), 1);

    // The check runs lag the rerun: the old failure is no second one.
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, _, commit) = ended(&dir, "failure");
    hub.stale(Call::CheckRuns);
    step_once(&mut store, &hub);
    hub.finish_again(&commit, "success");
    assert_eq!(waiting(&step_once(&mut store, &hub)), gets_a_second_try());
    assert_eq!(hub.count(Call::FlowRerun), 1);
}

#[test]
fn a_run_still_going_is_run_again_once_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, _, commit) = ended(&dir, "failure");
    let suite = hub.checks_on(&commit)[0].check_suite.as_ref().unwrap().id;
    let other = hub.run_id();
    hub.add_check(
        &commit,
        json!({"id": other, "name": "other", "status": "in_progress", "conclusion": null,
               "html_url": null, "check_suite": {"id": suite}}),
    );
    assert_eq!(waiting(&step_once(&mut store, &hub)), gets_a_second_try());
    assert_eq!(hub.count(Call::FlowRerun), 0);
    hub.remove_check(&commit, "other");
    step_once(&mut store, &hub);
    assert_eq!(hub.count(Call::FlowRerun), 1);
}

#[test]
fn a_gate_that_ends_twice_with_no_verdict_is_never_red_and_can_be_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, id, commit) = ended(&dir, "cancelled");
    step_once(&mut store, &hub);
    hub.finish_again(&commit, "cancelled");
    for _ in 0..2 {
        let stepped = step_once(&mut store, &hub);
        assert!(stepped.verdicts.is_empty());
        assert_eq!(
            waiting(&stepped),
            [format!("{GATE}: it ended with no verdict and no try left")]
        );
    }
    assert_eq!(hub.count(Call::FlowRerun), 1);
    drop_candidate(&mut store, &hub, &hub, &repo(), id).unwrap();
    assert_eq!(joined(&store), [3]);
}

#[test]
fn red_alone_is_free_to_join_again_once_its_gate_passes_attested() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, id, commit) = red(&dir);
    // Nothing new, then a pass nobody attests, then one on another commit.
    assert!(step_once(&mut store, &hub).released.is_empty());
    hub.rerun_again(&commit, "success");
    assert!(step_once(&mut store, &hub).released.is_empty());
    let elsewhere = format!("{:0>40}", "e15e");
    hub.change(&elsewhere, TRUNK, "elsewhere");
    hub.ci(&elsewhere, &[(GATE, "success")]);
    assert!(step_once(&mut store, &hub).released.is_empty());
    assert_eq!(hub.latest(&head(3)), Some(Mark::Red));
    assert_eq!(
        try_join(&mut store, &hub, 3, &head(3)).unwrap(),
        Joined::Held(Held::Red)
    );

    hub.attest_passes(&commit);
    // The status goes first: failing, it releases nothing.
    hub.fail(unmarked(Mark::Waiting));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(store.ledger().red_alone(&repo()), [id]);
    hub.heal(Call::Mark);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.released, [3]);
    assert_eq!(hub.latest(&head(3)), Some(Mark::Waiting));
    assert!(step_once(&mut store, &hub).released.is_empty());

    // The verdict stays, and it's gated again from nothing.
    assert_eq!(store.ledger().candidate(id).unwrap().state, State::Red);
    join_all(&mut store, &hub, &[3]);
    let stepped = step_once(&mut store, &hub);
    let (again, prs) = gating(&stepped);
    assert_ne!(again, id);
    assert_eq!(prs, [3]);
    assert!(store.ledger().candidate(again).unwrap().evidence.is_empty());
}

#[test]
fn a_red_pr_with_a_new_head_isnt_read_for_a_release() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, _, commit) = red(&dir);
    hub.push(REMI, 3, &head(33));
    ingest::pulled(&mut store, &hub, &config(), &repo(), 3).unwrap();
    hub.rerun_again(&commit, "success");
    hub.attest_passes(&commit);
    let before = hub.count(Call::CheckRuns);
    assert!(step_once(&mut store, &hub).released.is_empty());
    assert_eq!(hub.count(Call::CheckRuns), before);
}

#[test]
fn a_gate_that_times_out_twice_is_red_and_splits_its_batch() {
    for (conclusion, noted) in [("timed_out", false), ("cancelled", true)] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github(&[3, 4, 5]);
        join_all(&mut store, &hub, &[3, 4, 5]);
        let (whole, _) = gating(&step_once(&mut store, &hub));
        let commit = commit_of(&store, whole);
        hub.ci(&commit, &[(GATE, conclusion)]);
        step_once(&mut store, &hub);
        hub.finish_again(&commit, conclusion);
        if noted {
            let last = hub.checks_on(&commit).last().unwrap().id;
            let said = "The job has exceeded the maximum execution time of 90 minutes.";
            hub.annotations(last, &[said]);
        }
        let stepped = step_once(&mut store, &hub);
        assert_eq!(stepped.verdicts, [(whole, Verdict::Red)], "{conclusion}");
        assert_eq!(gating(&stepped).1, [3, 4]);
    }
}

#[test]
fn a_red_pr_whose_gate_cant_be_read_doesnt_stop_the_train() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, hub, id, _) = red(&dir);
    hub.pr(REMI, 4, &head(4));
    hub.settle(&head(4));
    join_all(&mut store, &hub, &[4]);
    let (next, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, next), &[(GATE, "success")]);

    // Out of reach is the step's to fail, as anywhere in it.
    hub.fail(Call::CheckRuns);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    hub.heal(Call::CheckRuns);

    // GitHub refuses the parked one's commit, and nothing after it.
    hub.refuse_with(Call::CheckRuns, "No commit found for SHA");
    hub.race(Call::Branch, |hub| hub.heal(Call::CheckRuns));
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [4]);
    assert_eq!(stepped.unread.len(), 1);
    assert_eq!(stepped.unread[0].pr, 3);
    assert!(stepped.unread[0].why.contains("No commit found"));
    assert_eq!(store.ledger().red_alone(&repo()), [id]);
}
