use std::collections::HashMap;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use super::*;
use crate::api::{Commit, Pulls};

const BASE: &str = "1111111111111111111111111111111111111111";
const FORK: &str = "2222222222222222222222222222222222222222";
const HEAD: &str = "3333333333333333333333333333333333333333";
const HEAD2: &str = "4444444444444444444444444444444444444444";
const MERGE: &str = "5555555555555555555555555555555555555555";
const STEWARD: &str = "fieldmouse-steward";

/// GitHub as recorded answers, in the shapes the REST API sends.
#[derive(Default)]
struct Fake {
    pulls: Vec<Value>,
    checks: HashMap<String, Value>,
    statuses: HashMap<String, Value>,
    reviews: HashMap<u64, Value>,
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap()
}

impl Source for Fake {
    fn pull(&self, _: &str, number: u64) -> Result<PullRequest, ApiError> {
        self.pulls
            .iter()
            .map(parse::<PullRequest>)
            .find(|p| p.number == number)
            .ok_or_else(|| ApiError(format!("no PR {number}")))
    }

    fn pulls(&self, _: &str, which: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        let all = self.pulls.iter().map(parse::<PullRequest>);
        Ok(match which {
            Pulls::All => all.collect(),
            Pulls::Open => all.filter(|p| p.state == "open").collect(),
            Pulls::RecentlyClosed => all.filter(|p| p.state == "closed").rev().collect(),
        })
    }

    fn check_runs(&self, _: &str, sha: &str) -> Result<Vec<CheckRun>, ApiError> {
        Ok(self.checks.get(sha).map_or_else(Vec::new, parse))
    }

    fn statuses(&self, _: &str, sha: &str) -> Result<Vec<CommitStatus>, ApiError> {
        Ok(self.statuses.get(sha).map_or_else(Vec::new, parse))
    }

    fn reviews(&self, _: &str, number: u64) -> Result<Vec<Review>, ApiError> {
        Ok(self.reviews.get(&number).map_or_else(Vec::new, parse))
    }

    fn commit(&self, _: &str, sha: &str) -> Result<Commit, ApiError> {
        assert_eq!(sha, MERGE);
        Ok(parse(&json!({"sha": MERGE, "parents": [{"sha": BASE}]})))
    }

    fn merge_base(&self, _: &str, a: &str, _: &str) -> Result<String, ApiError> {
        // The base branch has moved on since the head branched off it.
        assert_eq!(a, BASE);
        Ok(FORK.into())
    }
}

fn repo() -> Repo {
    Repo {
        owner: "FieldmouseWorks".into(),
        name: "burrow".into(),
    }
}

fn config() -> Config {
    Config {
        reviewers: vec![STEWARD.into()],
        own_contexts: vec!["review-settled".into()],
    }
}

fn pr(number: u64, head: &str, state: &str, merged: bool) -> Value {
    json!({
        "number": number,
        "state": state,
        "title": "autoland: merge only the head that was checked",
        "body": "Refs #61\n\nhttps://claude.ai/code/session_01Abc",
        "user": {"login": "TusanHomichi", "type": "User"},
        "merged_at": if merged { json!("2026-10-27T12:00:00Z") } else { Value::Null },
        "merge_commit_sha": if merged { json!(MERGE) } else { Value::Null },
        "base": {"sha": BASE, "ref": "main"},
        "head": {"sha": head, "ref": "claude/x"},
    })
}

fn marker(reviewer: &str, head: &str, base: &str, findings: u64) -> String {
    format!(
        "**Codex review**: nothing found.\n\n<!-- {reviewer}-review:v1 &#123;\"commit_sha\": \"{head}\", \"base\": \"{base}\", \"findings\": {findings}&#125; -->"
    )
}

fn review(id: u64, login: &str, head: &str, body: String) -> Value {
    json!({
        "user": {"login": login, "type": "User"},
        "body": body,
        "commit_id": head,
        "html_url": format!("https://github.com/x/y/pull/9#pullrequestreview-{id}"),
    })
}

fn fake() -> Fake {
    let mut f = Fake {
        pulls: vec![pr(9, HEAD, "open", false)],
        ..Fake::default()
    };
    f.checks.insert(
        HEAD.into(),
        json!([
            {"name": "check / check", "status": "completed", "conclusion": "success",
             "html_url": "https://github.com/x/y/actions/runs/1/job/1"},
            {"name": "refused", "status": "completed", "conclusion": "skipped", "html_url": null},
            {"name": "slow", "status": "in_progress", "conclusion": null, "html_url": null},
        ]),
    );
    f.statuses.insert(
        HEAD.into(),
        json!([
            {"context": "gate", "state": "success", "target_url": "https://remi/gate/2"},
            {"context": "review-settled", "state": "success", "target_url": null},
            {"context": "gate", "state": "failure", "target_url": "https://remi/gate/1"},
            {"context": "gate", "state": "pending", "target_url": null},
        ]),
    );
    f.reviews.insert(
        9,
        json!([
            review(1, STEWARD, HEAD, marker("codex", HEAD, "main", 2)),
            review(2, STEWARD, HEAD, marker("codex", HEAD, "main", 0)),
            // Not the steward: text in a comment, whatever it says.
            review(3, "someone", HEAD, marker("codex", HEAD, "main", 0)),
            // About the PR on its old base, before it was retargeted.
            review(4, STEWARD, HEAD, marker("codex", HEAD, "dev", 0)),
        ]),
    );
    f
}

fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), SigningKey::from_bytes(&[5; 32]), None).unwrap();
    (dir, store)
}

fn sha(s: &str) -> CommitId {
    s.parse().unwrap()
}

#[test]
fn an_open_pr_is_recorded_with_its_intent_revision_and_evidence() {
    let (_dir, mut store) = open();
    let got = pull(&mut store, &fake(), &config(), &repo(), 9).unwrap();
    let c = store.ledger().change(got.change).unwrap();

    assert_eq!(c.pr, 9);
    assert_eq!(c.author.kind, ActorKind::Agent);
    assert_eq!(c.author.login, "TusanHomichi");
    assert_eq!(
        c.author.session.as_deref(),
        Some("https://claude.ai/code/session_01Abc")
    );
    let intent = c.intent.as_ref().unwrap();
    assert_eq!(
        intent.summary,
        "autoland: merge only the head that was checked"
    );
    assert_eq!(intent.task.as_deref(), Some("FieldmouseWorks/burrow#61"));

    assert_eq!(c.revisions.len(), 1);
    assert_eq!(
        (c.revisions[0].base.as_str(), c.revisions[0].head.as_str()),
        (FORK, HEAD)
    );
    let revision = RevisionId::new(&sha(FORK), &sha(HEAD));
    assert_eq!(c.revisions[0].id, revision);

    let seen: Vec<_> = c
        .evidence
        .iter()
        .map(|e| (e.kind, e.check.as_str(), e.result))
        .collect();
    assert_eq!(
        seen,
        [
            (CheckKind::Ci, "check / check", Outcome::Pass),
            (CheckKind::Ci, "gate", Outcome::Fail),
            (CheckKind::Ci, "gate", Outcome::Pass),
            (CheckKind::Review, "codex", Outcome::Fail),
            (CheckKind::Review, "codex", Outcome::Pass),
        ]
    );
    assert!(c.evidence.iter().all(|e| e.revision == revision));
    assert_eq!(c.status(), Status::Open);
    // Observe, intent, revision and five pieces of evidence.
    assert_eq!(got.recorded, 8);
}

#[test]
fn ingesting_again_records_nothing_new() {
    let (_dir, mut store) = open();
    let f = fake();
    let first = pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    let again = pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    assert_eq!(
        again,
        Ingested {
            recorded: 0,
            ..first
        }
    );
}

#[test]
fn a_new_head_is_a_new_revision_and_the_old_evidence_stays_on_the_old_one() {
    let (_dir, mut store) = open();
    let mut f = fake();
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    f.pulls = vec![pr(9, HEAD2, "open", false)];
    let got = pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    assert_eq!(got.change, change);
    // The new revision only: the old head's markers don't name this one.
    assert_eq!(got.recorded, 1);
    let c = store.ledger().change(change).unwrap();
    assert_eq!(c.revisions.len(), 2);
    let old = RevisionId::new(&sha(FORK), &sha(HEAD));
    assert!(c.evidence.iter().all(|e| e.revision == old));
}

#[test]
fn a_merged_pr_lands_on_the_commit_it_made() {
    let (_dir, mut store) = open();
    let mut f = fake();
    f.pulls = vec![pr(9, HEAD, "closed", true)];
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    let c = store.ledger().landed_as(&repo(), &sha(MERGE)).unwrap();
    assert_eq!(c.id, change);
    let landing = c.landing.as_ref().unwrap();
    assert_eq!(landing.before, sha(BASE));
    assert_eq!(landing.revision, RevisionId::new(&sha(FORK), &sha(HEAD)));
    assert_eq!(
        pull(&mut store, &f, &config(), &repo(), 9)
            .unwrap()
            .recorded,
        0
    );
}

#[test]
fn closing_and_reopening_are_recorded_and_a_reopened_pr_can_land() {
    let (_dir, mut store) = open();
    let mut f = fake();
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    let status = |s: &Store| s.ledger().change(change).unwrap().status();

    f.pulls = vec![pr(9, HEAD, "closed", false)];
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    assert_eq!(status(&store), Status::Closed);
    f.pulls = vec![pr(9, HEAD, "open", false)];
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    assert_eq!(status(&store), Status::Open);

    // Closed, then reopened and merged between two looks: both are recorded.
    f.pulls = vec![pr(9, HEAD, "closed", false)];
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    f.pulls = vec![pr(9, HEAD, "closed", true)];
    assert_eq!(
        pull(&mut store, &f, &config(), &repo(), 9)
            .unwrap()
            .recorded,
        2
    );
    assert_eq!(status(&store), Status::Landed);
}

#[test]
fn backfill_takes_every_pr_and_who_made_it() {
    let (_dir, mut store) = open();
    let mut f = fake();
    let mut bot = pr(10, HEAD2, "open", false);
    bot["user"] = json!({"login": "renovate[bot]", "type": "Bot"});
    let mut person = pr(11, HEAD2, "open", false);
    person["body"] = Value::Null;
    f.pulls.extend([bot, person]);

    let got = backfill(&mut store, &f, &config(), &repo()).unwrap();
    assert_eq!(got.len(), 3);
    let kind = |n| store.ledger().by_pr(&repo(), n).unwrap().author.kind;
    assert_eq!(
        (kind(9), kind(10), kind(11)),
        (ActorKind::Agent, ActorKind::Bot, ActorKind::Person)
    );
    let person = store.ledger().by_pr(&repo(), 11).unwrap();
    assert_eq!(person.intent.as_ref().unwrap().task, None);
}

#[test]
fn tasks_are_read_from_the_body() {
    let r = repo();
    assert_eq!(
        task(&r, "Closes #7").as_deref(),
        Some("FieldmouseWorks/burrow#7")
    );
    assert_eq!(
        task(&r, "fixes FieldmouseWorks/remi#37 and refs #2").as_deref(),
        Some("FieldmouseWorks/remi#37")
    );
    let thread = "https://claude.ai/code/project/chan_01Ad?thread=cmsg_01Ad";
    let body = format!("_Requested by **Peter** · [project thread]({thread})_\n\nRefs #3");
    assert_eq!(task(&r, &body).as_deref(), Some("FieldmouseWorks/burrow#3"));
    let body = format!("_Requested by **Peter** · [project thread]({thread})_");
    assert_eq!(task(&r, &body).as_deref(), Some(thread));
    assert_eq!(task(&r, "issue #7, see #8"), None);
}

#[test]
fn sync_takes_open_prs_and_closed_ones_it_hasnt_finished() {
    let (_dir, mut store) = open();
    let mut f = fake();
    f.pulls = vec![pr(9, HEAD, "open", false), pr(10, HEAD2, "open", false)];
    assert_eq!(sync(&mut store, &f, &config(), &repo()).unwrap().len(), 2);

    // 9 merged and 11 opened and closed between two syncs; 10 is still open.
    f.pulls = vec![
        pr(9, HEAD, "closed", true),
        pr(10, HEAD2, "open", false),
        pr(11, HEAD2, "closed", false),
    ];
    let got = sync(&mut store, &f, &config(), &repo()).unwrap();
    let prs: Vec<u64> = got
        .iter()
        .map(|g| store.ledger().change(g.change).unwrap().pr)
        .collect();
    assert_eq!(prs, [10, 9, 11]);
    let status = |n| store.ledger().by_pr(&repo(), n).unwrap().status();
    assert_eq!(
        (status(9), status(10), status(11)),
        (Status::Landed, Status::Open, Status::Closed)
    );

    // Finished PRs aren't read again.
    let got = sync(&mut store, &f, &config(), &repo()).unwrap();
    assert_eq!(got.len(), 1);
}
