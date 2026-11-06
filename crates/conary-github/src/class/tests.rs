use std::cell::{Cell, RefCell};

use conary_core::policy::{Paths, Touched};
use ed25519_dalek::SigningKey;
use serde_json::json;

use super::*;
use crate::api::{CheckRun, Commit, Event, PullRequest, Pulls, Review, Status};

const BASE: &str = "1111111111111111111111111111111111111111";
const FORK: &str = "2222222222222222222222222222222222222222";
const HEAD: &str = "3333333333333333333333333333333333333333";
const HEAD2: &str = "4444444444444444444444444444444444444444";

/// GitHub with one open PR, whose head can move, and the files each
/// comparison lists.
struct Hub {
    head: RefCell<&'static str>,
    files: Vec<(&'static str, Paths)>,
    /// The comparisons asked for, as (base, head).
    compared: RefCell<Vec<(String, String)>>,
    pulled: Cell<usize>,
}

impl Hub {
    fn new(files: Vec<(&'static str, Paths)>) -> Self {
        Self {
            head: RefCell::new(HEAD),
            files,
            compared: RefCell::default(),
            pulled: Cell::new(0),
        }
    }
}

impl Source for Hub {
    fn pull(&self, _: &str, number: u64) -> Result<PullRequest, ApiError> {
        self.pulled.set(self.pulled.get() + 1);
        Ok(serde_json::from_value(json!({
            "number": number,
            "state": "open",
            "title": "area: a change",
            "body": null,
            "user": {"login": "TusanHomichi", "type": "User"},
            "base": {"sha": BASE, "ref": "main", "repo": {"default_branch": "main"}},
            "head": {"sha": *self.head.borrow(), "ref": "claude/x"},
        }))
        .unwrap())
    }
    fn pulls(&self, _: &str, _: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        unimplemented!()
    }
    fn check_runs(&self, _: &str, _: &str) -> Result<Vec<CheckRun>, ApiError> {
        Ok(Vec::new())
    }
    fn statuses(&self, _: &str, _: &str) -> Result<Vec<Status>, ApiError> {
        Ok(Vec::new())
    }
    fn reviews(&self, _: &str, _: u64) -> Result<Vec<Review>, ApiError> {
        Ok(Vec::new())
    }
    fn commit(&self, _: &str, _: &str) -> Result<Commit, ApiError> {
        unimplemented!()
    }
    fn commit_pulls(&self, _: &str, _: &str) -> Result<Vec<u64>, ApiError> {
        Ok(Vec::new())
    }
    fn timeline(&self, _: &str, _: u64) -> Result<Vec<Event>, ApiError> {
        Ok(Vec::new())
    }
    fn merge_base(&self, _: &str, _: &str, _: &str) -> Result<String, ApiError> {
        Ok(FORK.into())
    }
    fn files(&self, _: &str, base: &str, head: &str) -> Result<Paths, ApiError> {
        self.compared.borrow_mut().push((base.into(), head.into()));
        self.files
            .iter()
            .find(|(h, _)| *h == head)
            .map(|(_, p)| p.clone())
            .ok_or_else(|| ApiError::new(format!("no comparison to {head}")))
    }
}

fn paths(files: &[&str]) -> Paths {
    Paths {
        files: files
            .iter()
            .map(|p| Touched {
                path: (*p).into(),
                from: None,
                added: false,
            })
            .collect(),
        cut: false,
    }
}

fn repo() -> Repo {
    "FieldmouseWorks/burrow".parse().unwrap()
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

fn policy(text: &str) -> Policy {
    Policy::parse(text.as_bytes()).unwrap()
}

const POLICY: &str = r#"{"r2": ["(^|/)CODEOWNERS$"], "docs": ["^docs/"]}"#;

#[test]
fn a_revision_is_classed_from_its_own_comparison_and_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(vec![(HEAD, paths(&["src/a.rs", "CODEOWNERS"]))]);
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
    assert_eq!(*hub.compared.borrow(), [(FORK.into(), HEAD.into())]);
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
    let hub = Hub::new(vec![(HEAD, paths(&["docs/a.md"]))]);
    let p = policy(POLICY);
    let first = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap();
    let ops = store.ledger().head();
    let again = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap();
    assert!(!again.recorded);
    assert_eq!(again.said, first.said);
    assert!(again.said.docs && !again.said.review());
    assert_eq!(hub.compared.borrow().len(), 1, "compared again");
    assert_eq!(store.ledger().head(), ops, "recorded again");
}

#[test]
fn another_policy_classes_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(vec![(HEAD, paths(&["docs/a.md"]))]);
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
    let hub = Hub::new(vec![
        (HEAD, paths(&["docs/a.md"])),
        (HEAD2, paths(&["docs/a.md", "src/b.rs"])),
    ]);
    let p = policy(POLICY);
    let first = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD).unwrap();
    *hub.head.borrow_mut() = HEAD2;
    let second = class(&mut store, &hub, &config(), &p, &repo(), 7, HEAD2).unwrap();
    assert_ne!(first.revision, second.revision);
    assert!(first.said.docs && !second.said.docs);
}

#[test]
fn a_head_that_moved_is_refused_and_nothing_is_classed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(vec![(HEAD2, paths(&["CODEOWNERS"]))]);
    *hub.head.borrow_mut() = HEAD2;
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
    assert!(hub.compared.borrow().is_empty());
    let c = store.ledger().by_pr(&repo(), 7).unwrap();
    assert!(c.classes.is_empty());
}
