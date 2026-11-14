use ed25519_dalek::SigningKey;

use super::*;
use crate::sim::{Call, Hub};

const BASE: &str = "1111111111111111111111111111111111111111";
const FORK: &str = "2222222222222222222222222222222222222222";
const HEAD: &str = "3333333333333333333333333333333333333333";
const HEAD2: &str = "4444444444444444444444444444444444444444";

/// GitHub with PR 7 open at HEAD, forked from trunk at FORK, which has
/// moved on to BASE since; and HEAD2, another head forked there. Each head
/// changes `files`.
fn github(files: &[(&str, &[&str])]) -> Hub {
    let hub = Hub::new(REPO, FORK);
    hub.change(BASE, FORK, "trunk moved");
    hub.set_branch(REPO, "main", BASE);
    for (head, paths) in files {
        hub.change(head, FORK, head);
        hub.touch(head, paths);
    }
    if !files.iter().any(|(h, _)| *h == HEAD) {
        hub.change(HEAD, FORK, HEAD);
    }
    hub.pr(REPO, 7, HEAD);
    hub
}

/// The comparisons asked for, as (base, head).
fn compared(hub: &Hub) -> Vec<(String, String)> {
    hub.asked(Call::Files)
        .into_iter()
        .map(|a| (a[1].clone(), a[2].clone()))
        .collect()
}

const REPO: &str = "FieldmouseWorks/burrow";

fn repo() -> Repo {
    REPO.parse().unwrap()
}

fn config() -> Config {
    Config {
        reviewers: vec!["steward".into()],
        own_contexts: vec!["review-settled".into()],
    }
}

fn store(dir: &tempfile::TempDir) -> Store {
    let lock = conary_core::log::Lock::take(dir.path()).unwrap();
    Store::open(lock, SigningKey::from_bytes(&[6; 32]), None).unwrap()
}

fn class(
    store: &mut Store,
    hub: &Hub,
    config: &Config,
    policy: &Policy,
    repo: &Repo,
    pr: u64,
    head: &str,
) -> Result<Classes, ClassError> {
    at_once(store, hub, config, policy, repo, pr, head)
}

fn asked(
    store: &Store,
    hub: &Hub,
    config: &Config,
    policy: &Policy,
    repo: &Repo,
    pr: u64,
    head: &str,
) -> Result<Asked, ClassError> {
    let dir = store.lock().dir().to_owned();
    ask(store.ledger(), hub, config, policy, repo, pr, head, &dir)
}

/// Every read of GitHub a class makes, by kind, in order.
fn reads(hub: &Hub) -> Vec<Call> {
    use Call::*;
    let mut all = Vec::new();
    for call in [
        Pull, MergeBase, Files, CheckRuns, Statuses, Reviews, Timeline,
    ] {
        all.extend(std::iter::repeat_n(call, hub.count(call)));
    }
    all
}

fn policy(text: &str) -> Policy {
    Policy::parse(text.as_bytes()).unwrap()
}

const POLICY: &str = r#"{"r2": ["(^|/)CODEOWNERS$"], "docs": ["^docs/"]}"#;

#[test]
fn a_revision_is_classed_from_its_own_comparison_and_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["src/a.rs", "CODEOWNERS"])]);
    let got = class(
        &mut store,
        &hub,
        &config(),
        &policy(POLICY),
        &repo(),
        7,
        HEAD,
    )
    .unwrap();
    assert!(got.recorded);
    assert_eq!(got.class(), Class::R2);
    assert_eq!(got.said.because.as_deref(), Some("CODEOWNERS"));
    // Against the revision's merge base, not the base branch's tip.
    assert_eq!(compared(&hub), [(FORK.into(), HEAD.into())]);
    let c = store.ledger().change(got.change).unwrap();
    let k = c.class_of(got.revision).unwrap();
    assert_eq!(
        (k.class, k.policy.as_str()),
        (Class::R2, policy(POLICY).digest())
    );
}

#[test]
fn asked_again_it_answers_from_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["docs/a.md"])]);
    let p = policy(POLICY);
    let first = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap();
    let ops = store.ledger().head();
    let again = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap();
    assert!(!again.recorded);
    assert_eq!(again.said, first.said);
    assert!(again.said.docs && !again.said.review());
    assert_eq!(compared(&hub).len(), 1, "compared again");
    assert_eq!(store.ledger().head(), ops, "recorded again");
}

#[test]
fn another_policy_classes_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["docs/a.md"])]);
    let docs = class(
        &mut store,
        &hub,
        &config(),
        &policy(POLICY),
        &repo(),
        7,
        HEAD,
    )
    .unwrap();
    assert_eq!(docs.class(), Class::R0);
    let stricter = policy(r#"{"r2": ["^docs/"]}"#);
    let got = class(&mut store, &hub, &config(), &stricter, &repo(), 7, HEAD).unwrap();
    assert!(got.recorded);
    assert_eq!(got.class(), Class::R2);
    let c = store.ledger().change(got.change).unwrap();
    assert_eq!(c.class_of(got.revision).unwrap().class, Class::R2);
}

#[test]
fn a_new_head_is_a_new_revision_classed_on_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["docs/a.md"]), (HEAD2, &["docs/a.md", "src/b.rs"])]);
    let p = policy(POLICY);
    let first = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap();
    hub.push(REPO, 7, HEAD2);
    let second = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD2).unwrap();
    assert_ne!(first.revision, second.revision);
    assert!(first.said.docs && !second.said.docs);
}

#[test]
fn a_head_that_moved_is_refused_and_nothing_is_classed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD2, &["CODEOWNERS"])]);
    hub.push(REPO, 7, HEAD2);
    let e = class(
        &mut store,
        &hub,
        &config(),
        &policy(POLICY),
        &repo(),
        7,
        HEAD,
    )
    .unwrap_err();
    assert!(
        matches!(&e, ClassError::Moved { head, .. } if head == HEAD2),
        "{e}"
    );
    assert!(compared(&hub).is_empty());
    let c = store.ledger().by_pr(&repo(), 7).unwrap();
    assert!(c.classes.is_empty());
}

/// Its labels raise it: `review` asks a docs change for a review round,
/// `r2` makes it R2 and says so. Each set of labels is classed once and
/// recorded, and taking a label off lowers nothing.
#[test]
fn labels_raise_a_class_and_taking_them_off_lowers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["docs/a.md"])]);
    let p = policy(POLICY);
    let ask = |store: &mut Store| class(store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap();
    let plain = ask(&mut store);
    assert!(!plain.said.review());

    hub.label(REPO, 7, &["Review", "bug"]);
    let review = ask(&mut store);
    assert!(review.recorded);
    assert_eq!(review.said.raised, ["review"]);
    assert!(review.said.docs && review.said.review());
    assert_eq!(review.class(), Class::R0);
    let again = ask(&mut store);
    assert!(!again.recorded && again.said == review.said);

    hub.label(REPO, 7, &["r2", "review"]);
    let r2 = ask(&mut store);
    assert_eq!(r2.class(), Class::R2);
    assert_eq!(r2.said.because.as_deref(), Some("labelled r2"));
    assert_eq!(r2.said.raised, ["r2", "review"]);
    let c = store.ledger().change(r2.change).unwrap();
    assert_eq!(c.class_of(r2.revision).unwrap().raised, ["r2", "review"]);

    // Taken off, they stay: an author's raise is never undone by the PR.
    hub.label(REPO, 7, &[]);
    let after = ask(&mut store);
    assert!(!after.recorded);
    assert_eq!(after.said, r2.said);
}

/// A raise stays with the change: a new head is classed raised too, labels
/// or none.
#[test]
fn a_raise_holds_for_every_later_head() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["docs/a.md"]), (HEAD2, &["docs/b.md"])]);
    let p = policy(POLICY);
    hub.label(REPO, 7, &["r2"]);
    class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap();
    hub.label(REPO, 7, &[]);
    hub.push(REPO, 7, HEAD2);
    let got = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD2).unwrap();
    assert!(got.recorded);
    assert_eq!(
        (got.class(), got.said.raised.clone()),
        (Class::R2, vec!["r2".to_owned()])
    );
}

/// `r2` on a change the policy already makes R2 keeps the policy's reason.
#[test]
fn an_r2_label_on_an_r2_change_keeps_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["CODEOWNERS"])]);
    hub.label(REPO, 7, &["r2"]);
    let got = class(
        &mut store,
        &hub,
        &config(),
        &policy(POLICY),
        &repo(),
        7,
        HEAD,
    )
    .unwrap();
    assert_eq!(
        (got.class(), got.said.because.as_deref()),
        (Class::R2, Some("CODEOWNERS"))
    );
}

/// Everything a class reads of GitHub is read before the lock: under it,
/// with the PR's change as the ledger held it then, it only records.
#[test]
fn under_the_lock_it_only_records() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["CODEOWNERS"])]);
    let p = policy(POLICY);
    let Asked::Read(read) = asked(&store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap() else {
        panic!("the ledger held nothing")
    };
    let before = reads(&hub);
    assert!(before.contains(&Call::Files), "{before:?}");
    let got = super::class(&mut store, &hub, &p, *read).unwrap();
    assert_eq!(reads(&hub), before, "read GitHub under the lock");
    assert!(got.recorded);
    assert_eq!(got.class(), Class::R2);
    let c = store.ledger().change(got.change).unwrap();
    assert_eq!(c.current, Some(got.revision));
}

/// A class the ledger holds for the PR at its head, under this policy and
/// its labels, is the answer before the lock: one read of the PR, its merge
/// base kept beside the ledger, and nothing recorded.
#[test]
fn a_class_the_ledger_holds_needs_no_lock() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["docs/a.md"])]);
    let p = policy(POLICY);
    let first = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap();
    let ops = store.ledger().head();
    let mut before = reads(&hub);
    let Asked::Held(got) = asked(&store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap() else {
        panic!("the ledger holds it")
    };
    assert_eq!(got.said, first.said);
    assert!(!got.recorded);
    before.insert(0, Call::Pull);
    assert_eq!(reads(&hub), before, "read more than the PR");
    assert_eq!(store.ledger().head(), ops);
}

/// Another writer that recorded the PR after it was read, as GitHub showed
/// it later, is never written back over: the PR is read again under the
/// lock, and recorded as it is now.
#[test]
fn a_pr_recorded_since_it_was_read_is_read_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[(HEAD, &["docs/a.md"])]);
    let p = policy(POLICY);
    let Asked::Read(read) = asked(&store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap() else {
        panic!("the ledger held nothing")
    };
    hub.edit_pull(REPO, 7, |pr| pr["title"] = "renamed".into());
    crate::ingest::pull(&mut store, &hub, &config(), &repo(), 7).unwrap();
    let pulls = hub.count(Call::Pull);
    let got = super::class(&mut store, &hub, &p, *read).unwrap();
    let c = store.ledger().change(got.change).unwrap();
    assert_eq!(c.intent.as_ref().unwrap().summary, "renamed");
    assert_eq!(hub.count(Call::Pull), pulls + 1, "not read again");
    assert_eq!(compared(&hub).len(), 1, "its files read again");
    assert_eq!(got.class(), Class::R0);
}
