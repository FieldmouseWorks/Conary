use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use super::*;
use crate::attest::testing;
use crate::sim::{Call, FRESH, Hub, On, Posted, REMI, STEWARD, tree_of};

const TRUNK: &str = "1111111111111111111111111111111111111111";

fn head(pr: u64) -> String {
    format!("{pr:0>40}")
}

fn repo() -> Repo {
    "FieldmouseWorks/remi".parse().unwrap()
}

fn config() -> Config {
    Config {
        reviewers: vec![STEWARD.into()],
        own_contexts: vec!["review-settled".into()],
    }
}

/// The machine's class policy: a PR touching `secrets/` is R2.
fn policy() -> Policy {
    Policy::parse(br#"{"r2": ["^secrets/"]}"#).unwrap()
}

fn settings() -> Settings {
    Settings {
        size: 8,
        require: vec!["check / check".into()],
        signer: testing::signer(),
        fresh: Some(FRESH.into()),
    }
}

/// GitHub with PRs `prs` open at their heads, each its own patch on trunk
/// and settled by the steward there.
fn github(prs: &[u64]) -> Hub {
    let hub = Hub::new(REMI, TRUNK);
    for pr in prs {
        hub.pr(REMI, *pr, &head(*pr));
    }
    hub
}

/// The train's statuses that `on` covers fail, until healed.
fn unmarked(mark: Mark) -> On {
    On::from(Call::Mark).mark(mark)
}

/// The train's status saying a head is landing.
fn landing() -> On {
    unmarked(Mark::Landing)
}

fn store(dir: &tempfile::TempDir) -> Store {
    let lock = conary_core::log::Lock::take(dir.path()).unwrap();
    Store::open(lock, SigningKey::from_bytes(&[5; 32]), None).unwrap()
}

fn join_all(store: &mut Store, hub: &Hub, prs: &[u64]) {
    for pr in prs {
        assert_eq!(
            join(
                store,
                hub,
                hub,
                &config(),
                &repo(),
                *pr,
                &head(*pr),
                &[],
                &policy(),
                None
            )
            .unwrap(),
            Joined::Joined
        );
    }
}

/// PR `pr` touches a secret at its head: under [`policy`], it's R2.
fn r2(hub: &Hub, pr: u64) {
    hub.touch(&head(pr), &["secrets/x.yaml"]);
}

/// Joins PR `pr` at `at`, as the steward asks once it's settled there.
fn try_join(store: &mut Store, hub: &Hub, pr: u64, at: &str) -> Result<Joined, TrainError> {
    join(
        store,
        hub,
        hub,
        &config(),
        &repo(),
        pr,
        at,
        &[],
        &policy(),
        None,
    )
}

fn joined(store: &Store) -> Vec<u64> {
    store
        .ledger()
        .train(&repo())
        .map(|t| {
            t.joined()
                .iter()
                .map(|m| store.ledger().change(m.change).unwrap().pr)
                .collect()
        })
        .unwrap_or_default()
}

fn commit_of(store: &Store, id: CandidateId) -> String {
    store.ledger().candidate(id).unwrap().commit.to_string()
}

fn step_once(store: &mut Store, hub: &Hub) -> Stepped {
    step(store, hub, hub, &config(), &repo(), &settings()).unwrap()
}

fn gating(stepped: &Stepped) -> (CandidateId, Vec<u64>) {
    match &stepped.now {
        Now::Gating { candidate, prs, .. } => (*candidate, prs.clone()),
        other => panic!("not gating: {other:?}"),
    }
}

fn waiting(stepped: &Stepped) -> Vec<String> {
    match &stepped.now {
        Now::Gating { waiting, .. } => waiting.clone(),
        other => panic!("not gating: {other:?}"),
    }
}

mod class;
mod empty;
mod gate;
mod land;
mod negative;

#[test]
fn a_ready_pr_joins_at_its_head_as_github_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    let mut again = || try_join(&mut store, &hub, 3, &head(3)).unwrap();
    assert_eq!(again(), Joined::Joined);
    assert_eq!(again(), Joined::Already);
    // Settled at a head it's since moved on from.
    hub.push(REMI, 3, &head(4));
    match try_join(&mut store, &hub, 3, &head(3)) {
        Err(TrainError::Moved { pr: 3, head: now }) => assert_eq!(now, head(4)),
        other => panic!("{other:?}"),
    }
    // The new head took it out.
    assert!(store.ledger().train(&repo()).unwrap().joined().is_empty());
    assert!(
        leave(&mut store, &hub, &hub, &repo(), 3, Left::Unready)
            .is_ok_and(|left| left == Leaving::NotIn)
    );
}

#[test]
fn a_pr_joins_at_the_policys_class_and_an_r2_one_rides_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4]);
    r2(&hub, 4);
    join_all(&mut store, &hub, &[3, 4]);
    let ledger = store.ledger();
    let train = ledger.train(&repo()).unwrap();
    let class = |pr| {
        let c = ledger.by_pr(&repo(), pr).unwrap();
        let k = c.class_of(c.current.unwrap()).unwrap();
        (train.alone(c.id), k.class, k.policy == policy().digest())
    };
    assert_eq!(class(3), (false, Class::R0, true));
    assert_eq!(class(4), (true, Class::R2, true));
    // Its class is read once a revision and policy: asked again, the
    // ledger answers.
    let read = hub.count(Call::Files);
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4)).unwrap(),
        Joined::Already
    );
    assert_eq!(hub.count(Call::Files), read);
}

#[test]
fn a_candidate_is_merged_on_its_build_branch_and_gated_on_its_train_branch() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 5, 4]);
    join_all(&mut store, &hub, &[3, 5, 4]);
    let stepped = step_once(&mut store, &hub);
    let (id, prs) = gating(&stepped);
    assert_eq!(prs, [3, 5, 4]);
    assert!(stepped.conflicts.is_empty());

    // Merged in the order they joined, onto trunk, on the build branch.
    let merges = hub.merges(REMI);
    let build = build_branch(id);
    assert_eq!(
        merges,
        [3, 5, 4].map(|pr| (build.clone(), head(pr))).to_vec()
    );
    // The build branch is gone, and the train branch holds the result.
    let train = train_branch(id);
    assert_eq!(
        hub.names(REMI),
        ["conary/train/".to_owned() + &id.to_string(), "main".into()]
    );
    // What it holds, whatever order it was merged in.
    assert_eq!(hub.held(REMI, &train), [3, 4, 5].map(head));
    let c = store.ledger().candidate(id).unwrap();
    assert_eq!(c.base.as_str(), TRUNK);
    assert_eq!(c.commit.as_str(), hub.branch_at(REMI, &train).unwrap());
    assert_eq!(c.tree.as_str(), tree_of(&[3, 5, 4].map(head)));
    assert_eq!(c.state, State::Gating);

    // Stepped again while it gates: nothing new is built.
    let again = step_once(&mut store, &hub);
    assert_eq!(gating(&again).0, id);
    assert_eq!(hub.merges(REMI).len(), 3);
}

#[test]
fn a_head_that_wont_merge_leaves_and_the_rest_are_built_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4, 5]);
    hub.clash_with_all(&head(4));
    join_all(&mut store, &hub, &[3, 4, 5]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.conflicts,
        [Conflict {
            pr: 4,
            onto: vec![3],
            base: TRUNK.parse().unwrap(),
        }]
    );
    let (id, prs) = gating(&stepped);
    assert_eq!(prs, [3, 5]);
    // Only the candidate's own branch is left.
    assert_eq!(hub.names(REMI), [train_branch(id), "main".into()]);
    assert_eq!(joined(&store), [3, 5]);
}

#[test]
fn a_train_branch_lost_before_it_was_made_is_made_at_the_recorded_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let commit = hub.remove_branch(REMI, &train_branch(id)).unwrap();
    step_once(&mut store, &hub);
    assert_eq!(hub.branch_at(REMI, &train_branch(id)), Some(commit));
}

#[test]
fn red_alone_leaves_with_the_trains_failure_and_stays_out_until_its_head_moves() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(
        &commit_of(&store, id),
        &[("check / check", "failure"), ("lint", "success")],
    );
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Red)]);
    let url = "https://github.com/FieldmouseWorks/remi/actions/runs/0".to_owned();
    assert_eq!(
        stepped.red,
        [Red {
            pr: 3,
            candidate: id,
            failed: vec![Failed {
                check: "check / check".into(),
                url: Some(url.clone()),
            }],
        }]
    );
    assert_eq!(
        hub.marks().last(),
        Some(&Posted {
            sha: head(3),
            mark: Mark::Red,
            description: format!("red alone in candidate {id}: check / check"),
            url: Some(url),
        })
    );
    assert_eq!(stepped.now, Now::Idle);
    assert_eq!(
        try_join(&mut store, &hub, 3, &head(3)).unwrap(),
        Joined::Held(Held::Red)
    );
    hub.push(REMI, 3, &head(33));
    assert_eq!(
        try_join(&mut store, &hub, 3, &head(33)).unwrap(),
        Joined::Joined
    );
}

#[test]
fn red_with_others_splits_and_builds_the_older_half_in_the_same_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4, 5]);
    join_all(&mut store, &hub, &[3, 4, 5]);
    let (whole, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, whole), &[("check / check", "failure")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(whole, Verdict::Red)]);
    assert!(stepped.red.is_empty() && hub.marked().is_empty());
    let (older, prs) = gating(&stepped);
    assert_eq!(prs, [3, 4]);
    assert_eq!(store.ledger().candidate(older).unwrap().of, Some(whole));
}

#[test]
fn red_alone_is_recorded_only_once_its_head_has_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "failure")]);
    hub.fail(unmarked(Mark::Red));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(store.ledger().candidate(id).unwrap().state, State::Gating);
    hub.heal(Call::Mark);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Red)]);
    assert_eq!(hub.marked(), [(head(3), Mark::Red)]);
}

#[test]
fn a_pr_that_wont_merge_is_held_until_what_it_was_after_moves() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4]);
    hub.clash_with_all(&head(4));
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4)).unwrap(),
        Joined::Held(Held::Conflict)
    );
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    assert_eq!(step_once(&mut store, &hub).landed, [3]);
    // Trunk moved: it goes again, and won't merge onto trunk itself now.
    join_all(&mut store, &hub, &[4]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.conflicts,
        [Conflict {
            pr: 4,
            onto: vec![],
            base: hub.branch_at(REMI, "main").unwrap().parse().unwrap(),
        }]
    );
    assert_eq!(stepped.now, Now::Idle);
    assert!(joined(&store).is_empty());
}

#[test]
fn red_splits_and_the_older_half_is_built_on_trunk_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4, 5, 6]);
    join_all(&mut store, &hub, &[3, 4, 5, 6]);
    let (whole, _) = gating(&step_once(&mut store, &hub));
    store
        .record(Body::Verdict {
            candidate: whole,
            verdict: Verdict::Red,
        })
        .unwrap();
    let stepped = step_once(&mut store, &hub);
    let (older, prs) = gating(&stepped);
    assert_eq!(prs, [3, 4]);
    assert_eq!(store.ledger().candidate(older).unwrap().of, Some(whole));
    // The red one's branch went.
    assert_eq!(hub.names(REMI), [train_branch(older), "main".into()]);
    assert_eq!(hub.held(REMI, &train_branch(older)), [3, 4].map(head));
}

#[test]
fn stray_branches_go_and_only_the_trains_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[]);
    for name in [
        "conary/build/left-by-a-stopped-step",
        "conary/train/of-a-candidate-done-with",
        "conary/notes-of-someone-else",
        "claude/x",
    ] {
        hub.set_branch(REMI, name, TRUNK);
    }
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.now, Now::Idle);
    assert_eq!(
        hub.names(REMI),
        ["claude/x", "conary/notes-of-someone-else", "main"]
    );
}

#[test]
fn a_candidate_is_dropped_only_in_its_own_repo_and_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let other: Repo = "FieldmouseWorks/burrow".parse().unwrap();
    assert!(matches!(
        drop_candidate(&mut store, &hub, &hub, &other, id),
        Err(TrainError::NotHere(_))
    ));
    let unknown = CandidateId::new().unwrap();
    assert!(matches!(
        drop_candidate(&mut store, &hub, &hub, &repo(), unknown),
        Err(TrainError::NotHere(_))
    ));
    drop_candidate(&mut store, &hub, &hub, &repo(), id).unwrap();
    assert!(drop_candidate(&mut store, &hub, &hub, &repo(), id).is_err());
    assert_eq!(joined(&store), [3]);
}
