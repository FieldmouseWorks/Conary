//! Proof slots and the train take PRs by their priority level, then age,
//! then number, in full passes, dry runs and targeted kicks alike (#415).
use super::flow_pass_case::{Case, REPO};
use super::flow_pass_slots::{WAITING, another, kick, prepared, two};
use crate::api::Source;
use crate::branches::Branches;
use crate::flow::{self, *};
use serde_json::json;

/// The ways a PR goes in line as `high`: an owner's label, or being the fix
/// for a red default branch, which nobody labels (#507).
#[derive(Debug, Clone, Copy)]
enum High {
    Label,
    RedFix,
}
const HIGH: [High; 2] = [High::Label, High::RedFix];
fn raise(c: &Case, n: u64, how: High) {
    match how {
        High::Label => c.hub.label(REPO, n, &["priority: high"]),
        High::RedFix => red_fix(c, n),
    }
}
/// PR `n` is the fix for the repository's red default branch: pushed from
/// the red's branch in the repository itself, into the default branch, with
/// the default branch's head red.
fn red_fix(c: &Case, n: u64) {
    c.hub.edit_pull(REPO, n, |p| {
        p["head"]["ref"] = json!("autoland/red-cccccccccccc")
    });
    c.hub.refresh_flow(REPO, n);
    // The default branch as the hub sees it at each read, not as it was
    // when the snapshot was copied.
    c.hub.edit_flow(REPO, n, |s| s.default_branch = None);
    let trunk = c.hub.branch(REPO, "main").unwrap().unwrap();
    c.hub.ci(&trunk, &[("main-ci", "failure")]);
}

fn opened(c: &Case, n: u64, at: &str) {
    c.hub.edit_pull(REPO, n, |p| p["created_at"] = json!(at));
}
fn close(c: &Case, n: u64) {
    c.hub.edit_pull(REPO, n, |p| p["state"] = json!("closed"));
}
/// Each report's PR and its decision's stage and step.
fn decided(reports: &[Report]) -> Vec<(u64, Stage, Step)> {
    reports
        .iter()
        .map(|r| {
            let next = r.next.as_ref().unwrap_or_else(|| panic!("{r:?}"));
            (r.pr, next.stage, next.step.clone())
        })
        .collect()
}
fn proving(reports: &[Report]) -> Vec<u64> {
    decided(reports)
        .into_iter()
        .filter(|(_, _, step)| *step == Step::Prove)
        .map(|(pr, _, _)| pr)
        .collect()
}
fn of(reports: &[Report], pr: u64) -> &Next {
    reports
        .iter()
        .find(|r| r.pr == pr)
        .and_then(|r| r.next.as_ref())
        .unwrap()
}

/// One proof slot and three waiting PRs: an old unlabelled one (3), a newer
/// `high` one (7), raised `how`, and a newest `priority: low` one (5).
fn three(how: High) -> Case {
    let c = two(1);
    another(&c, 7);
    opened(&c, 3, "2099-01-01T00:00:00Z");
    opened(&c, 7, "2099-02-01T00:00:00Z");
    opened(&c, 5, "2099-03-01T00:00:00Z");
    raise(&c, 7, how);
    c.hub.label(REPO, 5, &["priority: low"]);
    c
}

#[test]
fn the_high_level_goes_first_then_the_old_one_then_the_low_level() {
    for how in HIGH {
        high_goes_first(how);
    }
}
fn high_goes_first(how: High) {
    let c = three(how);
    let now = flow::jobs::now();
    prepared(&c, now, true);
    let dry = c.pass(now + 1, true);
    assert_eq!(proving(&dry), vec![7], "{how:?}: {:?}", decided(&dry));
    let real = c.pass(now + 1, false);
    assert_eq!(proving(&real), vec![7], "{how:?}: {:?}", decided(&real));
    assert_eq!(of(&real, 7).priority, Level::High, "{how:?}");
    assert_eq!(c.deferred.0.borrow().len(), 1);
    for waiting in [3, 5] {
        let next = of(&real, waiting);
        assert_eq!((next.stage, next.step.clone()), WAITING);
    }
    // The high one is overdue at its level's deadline, the others at theirs.
    assert_eq!(of(&real, 7).overdue_at, Some(now + 1 + 1800));
    assert_eq!(of(&real, 3).overdue_at, Some(now + 1 + 7200));
    // Its slot freed, the old unlabelled one goes, and the low one still waits.
    close(&c, 7);
    let real = c.pass(now + 2, false);
    assert_eq!(proving(&real), vec![3], "{:?}", decided(&real));
    close(&c, 3);
    let real = c.pass(now + 3, false);
    assert_eq!(proving(&real), vec![5], "{:?}", decided(&real));
}

#[test]
fn within_a_level_the_older_pr_goes_first_whatever_its_number() {
    let c = two(1);
    opened(&c, 5, "2098-12-01T00:00:00Z");
    let now = flow::jobs::now();
    prepared(&c, now, true);
    for dry in [true, false] {
        let reports = c.pass(now + 1, dry);
        assert_eq!(proving(&reports), vec![5], "{:?}", decided(&reports));
    }
}

#[test]
fn a_targeted_kick_waits_behind_a_higher_level_queued_on_another_pr() {
    for how in HIGH {
        let c = three(how);
        let now = flow::jobs::now();
        prepared(&c, now, true);
        for dry in [true, false] {
            let report = kick(&c, 3, dry, now + 1);
            let waits = vec![(3, WAITING.0, WAITING.1)];
            assert_eq!(decided(&report), waits, "{how:?}");
        }
        assert!(c.deferred.0.borrow().is_empty(), "{how:?}");
    }
}

#[test]
fn the_train_admits_the_high_level_ahead_of_an_older_pr() {
    for how in HIGH {
        train_admits_high_first(how);
    }
}
fn train_admits_high_first(how: High) {
    let mut c = Case::new();
    c.config.settings.train = true;
    c.clean_review();
    another(&c, 5);
    for n in [3, 5] {
        c.hub.post_status(
            &super::head(n),
            json!({"context":"machine","state":"success"}),
        );
    }
    opened(&c, 5, "2099-02-01T00:00:00Z");
    raise(&c, 5, how);
    let ledger = tempfile::tempdir().unwrap();
    let lock = conary_core::log::Lock::take(ledger.path()).unwrap();
    let mut store =
        conary_core::Store::open(lock, ed25519_dalek::SigningKey::from_bytes(&[5; 32]), None)
            .unwrap();
    let reports = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now: flow::jobs::now(),
        dry_run: false,
        ledger: None,
    }
    .run(&c.hub, &REPO.parse().unwrap(), None, Some(&mut store))
    .unwrap();
    for report in &reports {
        assert_eq!(report.error, None, "{how:?}: {report:?}");
        let step = &report.next.as_ref().unwrap().step;
        assert_eq!(*step, Step::JoinTrain, "{how:?}: {report:?}");
    }
    let repo = REPO.parse().unwrap();
    let ledger = store.ledger();
    let joined: Vec<u64> = ledger
        .train(&repo)
        .unwrap()
        .joined()
        .iter()
        .map(|m| ledger.change(m.change).unwrap().pr)
        .collect();
    assert_eq!(joined, vec![5, 3], "{how:?}");
}

/// A change to a PR as GitHub lists it.
type Edit = dyn Fn(&mut serde_json::Value);

/// Only the pipeline's own fix for its red default branch goes first: the
/// same branch name on a fork, from someone the flow doesn't trust, or into
/// another branch is any PR, and so is a trusted PR from any other branch.
#[test]
fn only_a_trusted_fix_from_the_red_branch_into_trunk_is_ranked_high() {
    let c = Case::new();
    let trusted = &c.config.trusted_authors;
    let level = |edit: &Edit| {
        c.hub.edit_pull(REPO, 3, |p| {
            p["head"]["ref"] = json!("autoland/red-cccccccccccc");
            p["head"]["repo"]["full_name"] = json!(REPO);
            p["base"]["ref"] = json!("main");
            p["user"]["login"] = json!("owner");
            edit(p);
        });
        let pull = c.hub.pull(REPO, 3).unwrap();
        assert_eq!(flow::red_fix(&c.config.settings, trusted, &pull), {
            Rank::of(&c.config.settings, trusted, &pull).level == Level::High
        });
        Rank::of(&c.config.settings, trusted, &pull).level
    };
    assert_eq!(level(&|_| {}), Level::High);
    let any: [(&str, &Edit); 4] = [
        ("a fork", &|p| {
            p["head"]["repo"]["full_name"] = json!("someone/fork")
        }),
        ("untrusted", &|p| p["user"]["login"] = json!("someone")),
        ("not into trunk", &|p| p["base"]["ref"] = json!("dev")),
        ("another branch", &|p| p["head"]["ref"] = json!("fix-red")),
    ];
    for (why, edit) in any {
        assert_eq!(level(edit), Level::Medium, "{why}");
    }
}
