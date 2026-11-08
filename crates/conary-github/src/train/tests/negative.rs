//! A train that asks for a negative: a PR joins only once its own negative
//! run's attestation holds for the revision joining and says its new tests
//! failed on its base's code, or that it adds no test to code. That is
//! recorded as the revision's attested evidence, and counted from the
//! ledger after.

use super::*;
use conary_core::op::{Attest, CheckKind, Outcome};

const CHECK: &str = "check / negative";

fn negatives() -> Negatives {
    Negatives {
        check: CHECK.into(),
        signer: crate::attest::testing::signer(),
        fresh: Some(FRESH.into()),
    }
}

fn join_proven(store: &mut Store, hub: &Hub, pr: u64) -> Joined {
    join(
        store,
        hub,
        hub,
        &config(),
        &repo(),
        pr,
        &head(pr),
        &[],
        &policy(),
        Some(&negatives()),
    )
    .unwrap()
}

/// Why it isn't in, when it isn't.
fn unproven(joined: Joined) -> String {
    match joined {
        Joined::Unproven(why) => why,
        other => panic!("not unproven: {other:?}"),
    }
}

/// PR `pr`'s negative ran as check run `run`, concluding `concluded`.
fn ran(hub: &Hub, pr: u64, run: u64, concluded: Option<&str>) {
    let status = if concluded.is_some() {
        "completed"
    } else {
        "in_progress"
    };
    hub.put_checks(&head(pr),
        json!([{"id": run, "name": CHECK, "status": status, "conclusion": concluded,
                "html_url": format!("https://github.com/FieldmouseWorks/remi/actions/runs/9/job/{run}"),
                "started_at": "2026-11-05T05:00:00Z"}]),
    );
}

/// What a negative run on `head` from `base` would attest, by check run
/// `run`, its token on a PR's merge rather than the head.
fn attestation(base: &str, head: &str, run: u64, result: &str) -> String {
    let statement = testing::negative_statement(base, head, result);
    let claims = testing::claims(&repo().to_string(), &"3".repeat(40), run, &statement);
    testing::file(&statement, &testing::token(&claims))
}

/// The negative evidence the ledger holds for PR `pr`, attested or not.
fn negatives_of(store: &Store, pr: u64) -> Vec<(Outcome, bool)> {
    store
        .ledger()
        .by_pr(&repo(), pr)
        .unwrap()
        .evidence
        .iter()
        .filter(|e| e.kind == CheckKind::Negative)
        .map(|e| (e.result, e.attest.is_some()))
        .collect()
}

#[test]
fn a_pr_joins_once_its_new_tests_failed_on_its_base_and_its_job_attests_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    // Nothing has run, or it's still running.
    let why = unproven(join_proven(&mut store, &hub, 3));
    assert!(why.contains("hasn't run"), "{why}");
    // A negative's result without its producer's word is no negative.
    let c = store.ledger().by_pr(&repo(), 3).unwrap();
    let (change, revision) = (c.id, c.current.unwrap());
    store
        .record(Body::Evidence {
            change,
            revision,
            kind: CheckKind::Negative,
            check: CHECK.into(),
            result: Outcome::Fail,
            source: "posted by hand".into(),
            attest: None,
        })
        .unwrap();
    let why = unproven(join_proven(&mut store, &hub, 3));
    assert!(why.contains("hasn't run"), "{why}");
    ran(&hub, 3, 70, None);
    let why = unproven(join_proven(&mut store, &hub, 3));
    assert!(why.contains("hasn't finished"), "{why}");
    // It passed, but no attestation says so.
    ran(&hub, 3, 70, Some("success"));
    let why = unproven(join_proven(&mut store, &hub, 3));
    assert!(why.contains("kept no attestation"), "{why}");
    // Its attestation is another revision's: the head on another base.
    hub.attest(70, &attestation(&"f".repeat(40), &head(3), 70, "fail"));
    let why = unproven(join_proven(&mut store, &hub, 3));
    assert!(why.contains("not"), "{why}");
    assert_eq!(negatives_of(&store, 3), [(Outcome::Fail, false)]);
    // The new tests passed on the old code: red, recorded, and not in.
    ran(&hub, 3, 71, Some("failure"));
    hub.attest(71, &attestation(TRUNK, &head(3), 71, "pass"));
    let why = unproven(join_proven(&mut store, &hub, 3));
    assert!(why.contains("pass on"), "{why}");
    assert_eq!(
        negatives_of(&store, 3),
        [(Outcome::Fail, false), (Outcome::Pass, true)]
    );
    assert!(joined(&store).is_empty());
    // A rerun that caught them, on a runner it was started fresh for.
    ran(&hub, 3, 72, Some("success"));
    let file = attestation(TRUNK, &head(3), 72, "fail");
    hub.attest(72, &file);
    hub.ran_on(72, "remi-1");
    let why = unproven(join_proven(&mut store, &hub, 3));
    assert!(why.contains("wasn't started fresh"), "{why}");
    hub.ran_on(72, &format!("{FRESH}72"));
    assert_eq!(join_proven(&mut store, &hub, 3), Joined::Joined);
    assert_eq!(joined(&store), [3]);
    let c = store.ledger().by_pr(&repo(), 3).unwrap();
    let kept = c
        .evidence
        .iter()
        .rfind(|e| e.kind == CheckKind::Negative)
        .unwrap();
    assert_eq!(kept.result, Outcome::Fail);
    assert_eq!(kept.check, CHECK);
    assert_eq!(
        kept.source,
        "https://github.com/FieldmouseWorks/remi/actions/runs/9/job/72"
    );
    let (statement, _) = file.rsplit_once('\n').unwrap().0.rsplit_once('\n').unwrap();
    assert!(matches!(
        kept.attest.as_ref(),
        Some(Attest::GithubToken { statement: s, .. }) if s == &format!("{statement}\n")
    ));
    // Out and in again, it's counted from the ledger, downloading nothing.
    leave(&mut store, &hub, &hub, &repo(), 3, Left::Unready).unwrap();
    let downloads = hub.count(Call::Artifact);
    hub.remove_checks(&head(3));
    // Only for the check asked for: another's word is no negative of this.
    let other = Negatives {
        check: "other / negative".into(),
        ..negatives()
    };
    let rejoined = join(
        &mut store,
        &hub,
        &hub,
        &config(),
        &repo(),
        3,
        &head(3),
        &[],
        &policy(),
        Some(&other),
    )
    .unwrap();
    let why = unproven(rejoined);
    assert!(why.contains("other / negative hasn't run"), "{why}");
    // Only while its signer is the one the machine names: kept from
    // another, it's no negative now.
    let moved = Negatives {
        signer: "Acme/other/.github/workflows/gate.yml".parse().unwrap(),
        ..negatives()
    };
    let rejoined = join(
        &mut store,
        &hub,
        &hub,
        &config(),
        &repo(),
        3,
        &head(3),
        &[],
        &policy(),
        Some(&moved),
    )
    .unwrap();
    let why = unproven(rejoined);
    assert!(why.contains(&format!("{CHECK} hasn't run")), "{why}");
    assert_eq!(join_proven(&mut store, &hub, 3), Joined::Joined);
    assert_eq!(hub.count(Call::Artifact), downloads);
}

#[test]
fn a_pr_that_adds_no_test_to_code_joins_on_its_attested_word_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    ran(&hub, 3, 70, Some("success"));
    hub.attest(70, &attestation(TRUNK, &head(3), 70, "none"));
    assert_eq!(join_proven(&mut store, &hub, 3), Joined::Joined);
    assert_eq!(negatives_of(&store, 3), [(Outcome::None, true)]);
    // A train that doesn't ask for one doesn't read it.
    let hub = github(&[4]);
    let dir = tempfile::tempdir().unwrap();
    let mut store = super::store(&dir);
    ran(&hub, 4, 80, None);
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4)).unwrap(),
        Joined::Joined
    );
    assert!(negatives_of(&store, 4).is_empty());
}
