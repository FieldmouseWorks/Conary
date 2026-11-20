//! A train repo's pass steps its train once its PRs are seen to, under the
//! repository's lock (burrow#1280): stepping is landing, so it's the landing
//! owner's, and conaryd holds the repos it lands.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::flow::*;
use crate::sim::{Call, On};
use crate::train::{Mark, train_branch};
use conary_core::ledger::State;

const GATE: &str = "fresh / check";

/// A repo whose gate runs only in its train's candidates, as remi's does.
fn train_repo() -> Case {
    let mut c = Case::new();
    c.clean_review();
    c.config.settings.train = true;
    c.config.evidence.as_mut().unwrap().require = vec![GATE.into()];
    c
}

fn repo() -> conary_core::op::Repo {
    REPO.parse().unwrap()
}

/// The candidate in flight, and the commit its gate runs on.
fn in_flight(c: &Case) -> Option<(conary_core::CandidateId, String)> {
    let store = c.store.borrow();
    let ledger = store.ledger();
    let id = ledger.train(&repo())?.in_flight()?;
    Some((id, ledger.candidate(id).unwrap().commit.to_string()))
}

fn state(c: &Case, id: conary_core::CandidateId) -> State {
    c.store
        .borrow()
        .ledger()
        .candidate(id)
        .unwrap()
        .state
        .clone()
}

fn joined(c: &Case) -> usize {
    c.store
        .borrow()
        .ledger()
        .train(&repo())
        .map_or(0, |t| t.joined().len())
}

/// The candidate's gate finished on its commit: a pass attested by its
/// producer, or a failure.
fn gate(c: &Case, commit: &str, conclusion: &str) {
    c.hub.ci(commit, &[(GATE, conclusion)]);
    if conclusion == "success" {
        for run in c.hub.checks_on(commit) {
            c.hub.forget_artifacts(Some(run.id));
            c.attest_run(commit, run.id, None);
        }
    }
}

/// Seated by the first pass, which steps the train after it: its
/// candidate is built and gating on its train branch.
fn seated_and_gating(c: &Case) -> (conary_core::CandidateId, String) {
    let now = jobs::now();
    let reports = c.pass(now, false);
    assert_eq!(reports[0].error, None);
    assert_eq!(reports[0].next.as_ref().unwrap().step, Step::JoinTrain);
    let (id, commit) = in_flight(c).expect("the pass that seated it built its candidate");
    assert_eq!(state(c, id), State::Gating);
    assert_eq!(
        c.hub.branch_at(REPO, &train_branch(id)).as_deref(),
        Some(commit.as_str())
    );
    (id, commit)
}

#[test]
fn a_held_train_repos_pass_lands_its_green_candidate() {
    let c = train_repo();
    let (id, commit) = seated_and_gating(&c);
    // Gating: nothing lands while its gate runs.
    c.pass(jobs::now() + 1, false);
    assert_eq!(c.hub.count(Call::Land), 0);

    gate(&c, &commit, "success");
    let reports = c.pass(jobs::now() + 2, false);
    assert_eq!(reports[0].error, None);
    assert_eq!(c.hub.count(Call::Land), 1);
    assert!(c.hub.merged(REPO, 3).is_some());
    assert_eq!(c.hub.marked(), [(head(3), Mark::Landing)]);
    assert!(matches!(state(&c, id), State::Finished { .. }));
    // What it changed under the PR books its next pass at once.
    let next = reports[0].next.as_ref().unwrap();
    assert!(next.wait_until.is_some_and(|at| at <= jobs::now() + 3));
    // The pass it booked finds the PR closed, and nothing left to do.
    assert_eq!(c.pass(jobs::now() + 3, false).len(), 0);
}

#[test]
fn a_held_train_repos_pass_sends_a_red_candidate_away() {
    let c = train_repo();
    let (id, commit) = seated_and_gating(&c);
    gate(&c, &commit, "failure");
    c.pass(jobs::now() + 1, false);
    assert_eq!(c.hub.count(Call::Land), 0);
    assert!(c.hub.merged(REPO, 3).is_none());
    assert_eq!(state(&c, id), State::Red);
    assert_eq!(c.hub.marked(), [(head(3), Mark::Red)]);
    assert_eq!(joined(&c), 0);
    // The pass it booked hands the red PR to a fix, as the steward does.
    let after = c.pass(jobs::now() + 2, false);
    assert_eq!(after[0].error, None);
    let next = after[0].next.as_ref().unwrap();
    assert_eq!(
        (next.stage, &next.step),
        (Stage::Fix, &Step::HandFix(Kind::Red))
    );
}

#[test]
fn a_dry_run_or_one_prs_pass_doesnt_step() {
    let c = train_repo();
    c.pass(jobs::now(), true);
    // A real pass joins, and only the full one steps.
    let mut store = c.store.borrow_mut();
    let pass = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now: jobs::now(),
        dry_run: false,
        ledger: None,
    };
    pass.run(&c.hub, &repo(), Some(3), Some(&mut store))
        .unwrap();
    drop(store);
    assert_eq!(joined(&c), 1);
    assert!(in_flight(&c).is_none());
    assert_eq!(c.hub.merges(REPO), []);
}

#[test]
fn a_pr_in_the_train_that_wasnt_seen_to_stops_the_step() {
    let c = train_repo();
    let (id, commit) = seated_and_gating(&c);
    gate(&c, &commit, "success");
    c.hub.fail(Call::FlowStage);
    let reports = c.pass(jobs::now() + 1, false);
    assert!(reports[0].error.is_some());
    // Its gate is green, but nothing read it: the train wasn't stepped.
    assert_eq!(state(&c, id), State::Gating);
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn a_pause_written_after_the_trains_prs_were_decided_keeps_the_step_off() {
    let c = train_repo();
    let (id, commit) = seated_and_gating(&c);
    gate(&c, &commit, "success");
    // Another PR, decided after the seated one: the pause comes as it's
    // labelled, once the seated PR's decision went through.
    super::flow_pass_slots::another(&c, 5);
    let root = c.dir.path().to_path_buf();
    c.hub.race(On::from(Call::FlowStage).pr(5), move |_| {
        let paused = Pause {
            scope: Scope::Repository,
            reason: "while the pass ran".into(),
            at: 0,
        };
        pause::pause(&root, &repo(), &paused).unwrap();
    });
    let reports = c.pass(jobs::now() + 1, false);
    assert_eq!((reports[0].pr, &reports[0].error), (3, &None));
    assert_eq!(state(&c, id), State::Gating);
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn a_train_with_nothing_required_fails_its_step_on_its_prs() {
    let c = train_repo();
    c.pass(jobs::now(), false);
    let mut c = c;
    c.config.evidence.as_mut().unwrap().require.clear();
    let reports = c.pass(jobs::now() + 1, false);
    let error = reports[0].error.as_deref().unwrap_or_default();
    assert!(error.starts_with("stepping the train"), "{error}");
    assert_eq!(c.hub.count(Call::Land), 0);
}
