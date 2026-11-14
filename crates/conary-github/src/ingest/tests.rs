use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use super::*;
use crate::sim::{Call, Hub};

const BASE: &str = "1111111111111111111111111111111111111111";
const FORK: &str = "2222222222222222222222222222222222222222";
const HEAD: &str = "3333333333333333333333333333333333333333";
const HEAD2: &str = "4444444444444444444444444444444444444444";
const MERGE: &str = "5555555555555555555555555555555555555555";
const REBASED: &str = "6666666666666666666666666666666666666666";
const DEV: &str = "7777777777777777777777777777777777777777";
const DEV_FORK: &str = "8888888888888888888888888888888888888888";
const STEWARD: &str = "fieldmouse-steward";

const REPO: &str = "FieldmouseWorks/burrow";

fn repo() -> Repo {
    REPO.parse().unwrap()
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
        "base": {"sha": BASE, "ref": "main", "repo": {"default_branch": "main"}},
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

/// GitHub as the tests find it: PR 9 open at HEAD, forked from trunk at
/// FORK, which 8 has moved on to BASE since, and dev forked from trunk
/// before that at DEV_FORK; HEAD2 another head forked at FORK; and 9
/// squashed onto trunk as MERGE, after 8, as if it had merged.
fn fake() -> Hub {
    let f = Hub::new(REPO, DEV_FORK);
    f.change(FORK, DEV_FORK, "fork");
    f.change(DEV, DEV_FORK, "dev");
    f.change(BASE, FORK, "8");
    f.set_branch(REPO, "main", BASE);
    f.set_branch(REPO, "dev", DEV);
    for head in [HEAD, HEAD2] {
        f.change(head, FORK, head);
    }
    f.put_commit(MERGE, &[BASE], false);
    f.link(REPO, MERGE, &[9]);
    f.link(REPO, BASE, &[8]);
    f.put_pulls(REPO, [pr(9, HEAD, "open", false)]);
    f.put_checks(HEAD,
        json!([
            {"id": 1, "name": "check / check", "status": "completed", "conclusion": "success",
             "html_url": "https://github.com/x/y/actions/runs/1/job/1"},
            {"id": 2, "name": "refused", "status": "completed", "conclusion": "skipped", "html_url": null},
            {"id": 3, "name": "slow", "status": "in_progress", "conclusion": null, "html_url": null},
        ]),
    );
    f.put_statuses(
        HEAD,
        json!([
            // The train's word that it's landing this head: not a check.
            {"context": "conary/train", "state": "success", "target_url": null,
             "creator": {"login": STEWARD, "type": "User"}, "created_at": "2026-10-27T11:59:30Z"},
            {"context": "gate", "state": "success", "target_url": "https://remi/gate/2"},
            {"context": "review-settled", "state": "success", "target_url": null,
             "creator": {"login": STEWARD, "type": "User"}, "created_at": "2026-10-27T11:59:00Z"},
            {"context": "gate", "state": "failure", "target_url": "https://remi/gate/1"},
            {"context": "gate", "state": "pending", "target_url": null},
        ]),
    );
    f.put_reviews(
        REPO,
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
    let lock = conary_core::log::Lock::take(dir.path()).unwrap();
    let store = Store::open(lock, SigningKey::from_bytes(&[5; 32]), None).unwrap();
    (dir, store)
}

/// A sync as `cn sync` runs one: gathered, then recorded.
fn synced(store: &mut Store, f: &Hub) -> Result<Vec<Ingested>, IngestError> {
    let gathered = gather(f, &config(), &repo(), store.lock().dir())?;
    sync(store, f, gathered)
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
    let f = fake();
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    f.put_pulls(REPO, [pr(9, HEAD2, "open", false)]);
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
fn a_head_pushed_back_to_an_older_one_is_that_revision_again_and_current() {
    let (_dir, mut store) = open();
    let f = fake();
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    let old = RevisionId::new(&sha(FORK), &sha(HEAD));
    f.put_pulls(REPO, [pr(9, HEAD2, "open", false)]);
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    f.put_pulls(REPO, [pr(9, HEAD, "open", false)]);
    assert_eq!(
        pull(&mut store, &f, &config(), &repo(), 9)
            .unwrap()
            .recorded,
        1
    );
    let c = store.ledger().change(change).unwrap();
    assert_eq!(c.revisions.len(), 2);
    assert_eq!(c.current, Some(old));
    assert_eq!(
        pull(&mut store, &f, &config(), &repo(), 9)
            .unwrap()
            .recorded,
        0
    );
}

#[test]
fn a_merged_pr_lands_on_the_commit_it_made() {
    let (_dir, mut store) = open();
    let f = fake();
    f.put_pulls(REPO, [pr(9, HEAD, "closed", true)]);
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    let c = store.ledger().landed_as(&repo(), &sha(MERGE)).unwrap();
    assert_eq!(c.id, change);
    let landing = c.landing.as_ref().unwrap();
    assert_eq!(landing.before, sha(BASE));
    assert_eq!(landing.revision, RevisionId::new(&sha(FORK), &sha(HEAD)));
    // What it landed on, and no more: a status GitHub shows from that login.
    assert_eq!(
        landing.by,
        Authority::Status {
            login: STEWARD.into()
        }
    );
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
    let f = fake();
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    let status = |s: &Store| s.ledger().change(change).unwrap().status();

    f.put_pulls(REPO, [pr(9, HEAD, "closed", false)]);
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    assert_eq!(status(&store), Status::Closed);
    f.put_pulls(REPO, [pr(9, HEAD, "open", false)]);
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    assert_eq!(status(&store), Status::Open);

    // Closed, then reopened and merged between two looks: both are recorded.
    f.put_pulls(REPO, [pr(9, HEAD, "closed", false)]);
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    f.put_pulls(REPO, [pr(9, HEAD, "closed", true)]);
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
    let f = fake();
    let mut bot = pr(10, HEAD2, "open", false);
    bot["user"] = json!({"login": "renovate[bot]", "type": "Bot"});
    let mut person = pr(11, HEAD2, "open", false);
    person["body"] = Value::Null;
    f.put_pull(REPO, bot);
    f.put_pull(REPO, person);

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
    let f = fake();
    f.put_pulls(
        REPO,
        [pr(9, HEAD, "open", false), pr(10, HEAD2, "open", false)],
    );
    assert_eq!(synced(&mut store, &f).unwrap().len(), 2);

    // 9 merged and 11 opened and closed between two syncs; 10 is still open.
    f.put_pulls(
        REPO,
        [
            pr(9, HEAD, "closed", true),
            pr(10, HEAD2, "open", false),
            pr(11, HEAD2, "closed", false),
        ],
    );
    let got = synced(&mut store, &f).unwrap();
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
    let got = synced(&mut store, &f).unwrap();
    assert_eq!(got.len(), 1);
}

#[test]
fn sync_asks_an_open_pr_s_merge_base_once_until_it_moves() {
    let (_dir, mut store) = open();
    let f = fake();
    f.put_pulls(REPO, [pr(9, HEAD, "open", false)]);
    synced(&mut store, &f).unwrap();
    let asked = f.count(Call::MergeBase);
    assert_eq!(asked, 1);
    // Every event on the repo syncs it again; its base and head haven't moved.
    synced(&mut store, &f).unwrap();
    synced(&mut store, &f).unwrap();
    assert_eq!(f.count(Call::MergeBase), asked);
    // A push is a new pair.
    f.put_pulls(REPO, [pr(9, HEAD2, "open", false)]);
    synced(&mut store, &f).unwrap();
    assert_eq!(f.count(Call::MergeBase), asked + 1);
}

#[test]
fn a_squash_github_hasnt_linked_to_its_pr_yet_is_recorded_once_it_has() {
    // GitHub shows the PR merged a moment before it links its squash to
    // the PR: nothing is said of the merge until then, and then it lands.
    let (_dir, mut store) = open();
    let f = fake();
    f.put_pulls(REPO, [pr(9, HEAD, "closed", true)]);
    f.link(REPO, MERGE, &[]);
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    let ledger = store.ledger();
    assert_eq!(ledger.change(change).unwrap().status(), Status::Open);
    assert!(ledger.outside().is_empty());
    assert!(ledger.landed_as(&repo(), &sha(MERGE)).is_none());
    f.link(REPO, MERGE, &[9]);
    let synced = synced(&mut store, &f).unwrap();
    assert_eq!(synced.len(), 1);
    let ledger = store.ledger();
    assert_eq!(ledger.landed_as(&repo(), &sha(MERGE)).unwrap().id, change);
    assert!(ledger.outside().is_empty());
}

#[test]
fn a_rebase_lands_from_trunk_before_its_first_commit() {
    let (_dir, mut store) = open();
    let f = fake();
    f.put_pulls(REPO, [pr(9, HEAD, "closed", true)]);
    // 9's two commits rebased onto 8: REBASED, then MERGE on top of it.
    f.put_commit(MERGE, &[REBASED], false);
    f.put_commit(REBASED, &[BASE], false);
    f.link(REPO, REBASED, &[9]);
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    let landing = store.ledger().by_pr(&repo(), 9).unwrap().landing.clone();
    assert_eq!(landing.unwrap().before, sha(BASE));
}

#[test]
fn a_merge_commit_lands_from_its_first_parent() {
    let (_dir, mut store) = open();
    let f = fake();
    f.put_pulls(REPO, [pr(9, HEAD, "closed", true)]);
    // Linked to 9 or not, a merge's first parent is trunk before it.
    f.put_commit(MERGE, &[BASE, HEAD], true);
    f.link(REPO, BASE, &[9]);
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    let landing = store.ledger().by_pr(&repo(), 9).unwrap().landing.clone();
    assert_eq!(landing.unwrap().before, sha(BASE));
}

#[test]
fn a_merge_the_steward_didnt_settle_is_outside_not_a_landing() {
    // An admin's bypass: GitHub merged a head the steward never settled.
    // A push straight to trunk: the merge commit is the PR's own head.
    let bypass = |f: &Hub| {
        f.put_statuses(HEAD, json!([]));
    };
    let pushed = |f: &Hub| {
        f.edit_pull(REPO, 9, |p| p["merge_commit_sha"] = json!(HEAD));
        f.put_commit(HEAD, &[BASE], false);
        f.link(REPO, HEAD, &[9]);
    };
    // A push is no merge GitHub has yet to link: its head isn't linked.
    let pushed_unlinked = |f: &Hub| {
        pushed(f);
        f.link(REPO, HEAD, &[]);
    };
    // Its status posted by someone other than the steward, or only before
    // the PR was retargeted onto trunk.
    let forged = |f: &Hub| {
        f.put_statuses(
            HEAD,
            json!([{"context": "review-settled", "state": "success", "target_url": null,
                 "creator": {"login": "TusanHomichi", "type": "User"}}]),
        );
    };
    let stale = |f: &Hub| {
        f.put_statuses(
            HEAD,
            json!([{"context": "review-settled", "state": "success", "target_url": null,
                 "creator": {"login": STEWARD, "type": "User"},
                 "created_at": "2026-10-27T11:00:00Z"}]),
        );
        f.put_event(REPO, 9, "base_ref_changed", "2026-10-27T11:30:00Z");
    };
    // Merged while the steward was still reviewing, and settled after: it
    // wasn't the steward's merge, and an undated word can't be placed.
    let late = |f: &Hub| {
        f.put_statuses(
            HEAD,
            json!([{"context": "review-settled", "state": "success", "target_url": null,
                 "creator": {"login": STEWARD, "type": "User"},
                 "created_at": "2026-10-27T12:05:00Z"},
                {"context": "review-settled", "state": "pending", "target_url": null,
                 "creator": {"login": STEWARD, "type": "User"},
                 "created_at": "2026-10-27T11:50:00Z"}]),
        );
    };
    let undated = |f: &Hub| {
        f.put_statuses(
            HEAD,
            json!([{"context": "review-settled", "state": "success", "target_url": null,
                 "creator": {"login": STEWARD, "type": "User"}}]),
        );
    };
    // Merged locally and pushed: a merge commit of another head, or of this
    // one that GitHub didn't make, though the steward settled the head.
    let local = |f: &Hub| {
        f.put_commit(MERGE, &[BASE, HEAD2], true);
        f.link(REPO, MERGE, &[]);
    };
    let local_same_head = |f: &Hub| {
        f.put_commit(MERGE, &[BASE, HEAD], false);
        f.link(REPO, MERGE, &[]);
    };
    for (how, change_it) in [
        ("bypass", &bypass as &dyn Fn(&Hub)),
        ("pushed", &pushed),
        ("pushed, unlinked", &pushed_unlinked),
        ("forged", &forged),
        ("stale", &stale),
        ("late", &late),
        ("undated", &undated),
        ("local", &local),
        ("local same head", &local_same_head),
    ] {
        let (_dir, mut store) = open();
        let f = fake();
        f.put_pulls(REPO, [pr(9, HEAD, "closed", true)]);
        change_it(&f);
        let after = f.pull_json(REPO, 9)["merge_commit_sha"]
            .as_str()
            .unwrap()
            .to_owned();
        let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
        let ledger = store.ledger();
        assert_eq!(
            ledger.change(change).unwrap().status(),
            Status::Closed,
            "{how}"
        );
        assert!(ledger.landed_as(&repo(), &sha(&after)).is_none());
        let outside = ledger.outside();
        assert_eq!(outside.len(), 1);
        // GitHub doesn't say where trunk was before, so it isn't guessed.
        assert_eq!(
            (outside[0].before.as_ref(), &outside[0].after),
            (None, &sha(&after))
        );
        assert_eq!(
            pull(&mut store, &f, &config(), &repo(), 9)
                .unwrap()
                .recorded,
            0
        );
        // And sync leaves it be.
        assert!(synced(&mut store, &f).unwrap().is_empty());
    }
}

#[test]
fn a_pr_merged_in_another_prs_landing_closes_without_a_second_one() {
    // 10's head came onto trunk in 9's landing, which GitHub links to 9.
    // Either read first, 9 lands and 10 closes.
    for order in [[9, 10], [10, 9]] {
        let (_dir, mut store) = open();
        let f = fake();
        f.put_pulls(
            REPO,
            [pr(9, HEAD, "closed", true), pr(10, HEAD, "closed", true)],
        );
        for n in order {
            pull(&mut store, &f, &config(), &repo(), n).unwrap();
        }
        let ledger = store.ledger();
        assert_eq!(ledger.landed_as(&repo(), &sha(MERGE)).unwrap().pr, 9);
        let ten = ledger.by_pr(&repo(), 10).unwrap();
        assert_eq!(ten.status(), Status::Closed, "{order:?}");
        assert!(ledger.outside().is_empty());
    }
}

#[test]
fn ingest_refuses_to_run_without_the_steward_configured() {
    let (_dir, mut store) = open();
    let f = fake();
    for config in [
        Config {
            reviewers: vec![],
            ..config()
        },
        Config {
            own_contexts: vec![],
            ..config()
        },
    ] {
        assert!(matches!(
            pull(&mut store, &f, &config, &repo(), 9),
            Err(IngestError::Config(_))
        ));
        assert!(matches!(
            gather(&f, &config, &repo(), store.lock().dir()),
            Err(IngestError::Config(_))
        ));
        assert!(matches!(
            backfill(&mut store, &f, &config, &repo()),
            Err(IngestError::Config(_))
        ));
    }
    assert!(store.head().is_none());
}

#[test]
fn a_pr_merged_into_another_branch_closes_without_landing() {
    let (_dir, mut store) = open();
    let f = fake();
    let mut into_dev = pr(9, HEAD, "closed", true);
    into_dev["base"] = json!({"sha": DEV, "ref": "dev", "repo": {"default_branch": "main"}});
    f.put_pulls(REPO, [into_dev]);
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    let ledger = store.ledger();
    assert_eq!(ledger.change(change).unwrap().status(), Status::Closed);
    assert!(ledger.landed_as(&repo(), &sha(MERGE)).is_none() && ledger.outside().is_empty());
    assert!(synced(&mut store, &f).unwrap().is_empty());
}

/// 9 retargeted from main to dev with its head unchanged: a new revision.
fn retarget(f: &Hub) {
    let mut on_dev = pr(9, HEAD, "open", false);
    on_dev["base"] = json!({"sha": DEV, "ref": "dev", "repo": {"default_branch": "dev"}});
    f.put_pulls(REPO, [on_dev]);
}

#[test]
fn results_from_before_a_retarget_stay_on_the_old_revision() {
    let (_dir, mut store) = open();
    let f = fake();
    let change = pull(&mut store, &f, &config(), &repo(), 9).unwrap().change;
    let old = RevisionId::new(&sha(FORK), &sha(HEAD));
    let new = RevisionId::new(&sha(DEV_FORK), &sha(HEAD));

    // Seen on the old revision already: not bound to the new one, though
    // nothing says when they came. The marker about 9 on dev is the new
    // revision's own.
    retarget(&f);
    assert_eq!(
        pull(&mut store, &f, &config(), &repo(), 9)
            .unwrap()
            .recorded,
        2
    );
    let c = store.ledger().change(change).unwrap();
    assert!(c.revision(new).is_some());
    let on = |c: &conary_core::ledger::Change, r| -> Vec<String> {
        c.evidence
            .iter()
            .filter(|e| e.revision == r)
            .map(|e| e.check.clone())
            .collect()
    };
    assert_eq!(on(c, new), ["codex"]);
    assert_eq!(on(c, old).len(), 5);

    // Never seen, from before the retarget: left out. After it: bound.
    f.put_event(REPO, 9, "base_ref_changed", "2026-10-27T13:00:00Z");
    f.put_checks(
        HEAD,
        json!([
            {"id": 4, "name": "before", "status": "completed", "conclusion": "success",
             "html_url": "https://github.com/x/y/actions/runs/2/job/2",
             "started_at": "2026-10-27T12:59:59Z"},
            {"id": 5, "name": "unknown", "status": "completed", "conclusion": "success",
             "html_url": "https://github.com/x/y/actions/runs/3/job/3"},
            {"id": 6, "name": "after", "status": "completed", "conclusion": "success",
             "html_url": "https://github.com/x/y/actions/runs/4/job/4",
             "started_at": "2026-10-27T13:00:01Z"},
        ]),
    );
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    let c = store.ledger().change(change).unwrap();
    assert_eq!(on(c, new), ["codex", "after"]);
}

#[test]
fn sync_reads_a_closed_pr_again_when_it_changed_since() {
    let (_dir, mut store) = open();
    let f = fake();
    f.put_pulls(REPO, [pr(9, HEAD, "closed", false)]);
    synced(&mut store, &f).unwrap();
    let status = |s: &Store| s.ledger().by_pr(&repo(), 9).unwrap().status();
    assert_eq!(status(&store), Status::Closed);
    assert!(synced(&mut store, &f).unwrap().is_empty());

    // Reopened, pushed to and closed again between two syncs.
    f.put_pulls(REPO, [pr(9, HEAD2, "closed", false)]);
    assert_eq!(synced(&mut store, &f).unwrap().len(), 1);
    let c = store.ledger().by_pr(&repo(), 9).unwrap();
    assert_eq!(c.revisions.len(), 2);

    // Reopened, settled and merged between two syncs.
    f.put_statuses(
        HEAD2,
        json!([{"context": "review-settled", "state": "success", "target_url": null,
             "creator": {"login": STEWARD, "type": "User"}, "created_at": "2026-10-27T11:59:00Z"}]),
    );
    f.put_pulls(REPO, [pr(9, HEAD2, "closed", true)]);
    assert_eq!(synced(&mut store, &f).unwrap().len(), 1);
    assert_eq!(status(&store), Status::Landed);
    assert!(synced(&mut store, &f).unwrap().is_empty());
}

#[test]
fn sync_reads_a_closed_pr_again_when_it_went_back_to_a_revision_it_had() {
    let (_dir, mut store) = open();
    let f = fake();
    let current = |s: &Store| s.ledger().by_pr(&repo(), 9).unwrap().current;
    let first = RevisionId::new(&sha(FORK), &sha(HEAD));
    f.put_pulls(REPO, [pr(9, HEAD, "closed", false)]);
    synced(&mut store, &f).unwrap();
    f.put_pulls(REPO, [pr(9, HEAD2, "closed", false)]);
    synced(&mut store, &f).unwrap();

    // Reopened, pushed back to its first head and closed again between two
    // syncs: that revision is current again, and nothing new is added.
    f.put_pulls(REPO, [pr(9, HEAD, "closed", false)]);
    assert_eq!(synced(&mut store, &f).unwrap().len(), 1);
    assert_eq!(current(&store), Some(first));
    assert_eq!(store.ledger().by_pr(&repo(), 9).unwrap().revisions.len(), 2);
    assert!(synced(&mut store, &f).unwrap().is_empty());

    // Retargeted, then back onto main, the same way.
    let mut on_dev = pr(9, HEAD, "closed", false);
    on_dev["base"] = json!({"sha": DEV, "ref": "dev", "repo": {"default_branch": "main"}});
    f.put_pulls(REPO, [on_dev]);
    synced(&mut store, &f).unwrap();
    f.put_pulls(REPO, [pr(9, HEAD, "closed", false)]);
    assert_eq!(synced(&mut store, &f).unwrap().len(), 1);
    assert_eq!(current(&store), Some(first));
    assert!(synced(&mut store, &f).unwrap().is_empty());
}

#[test]
fn sync_reads_a_closed_pr_again_when_its_base_changed_and_asks_github_once() {
    let (_dir, mut store) = open();
    let f = fake();
    f.put_pulls(REPO, [pr(9, HEAD, "closed", false)]);
    synced(&mut store, &f).unwrap();
    // Each closed PR's merge base is asked for once, then kept.
    let asked = f.count(Call::MergeBase);
    assert!(synced(&mut store, &f).unwrap().is_empty());
    assert_eq!(f.count(Call::MergeBase), asked);

    // Reopened, retargeted and closed again, its head and intent the same.
    let mut on_dev = pr(9, HEAD, "closed", false);
    on_dev["base"] = json!({"sha": DEV, "ref": "dev", "repo": {"default_branch": "main"}});
    f.put_pulls(REPO, [on_dev]);
    assert_eq!(synced(&mut store, &f).unwrap().len(), 1);
    let c = store.ledger().by_pr(&repo(), 9).unwrap();
    assert!(
        c.revision(RevisionId::new(&sha(DEV_FORK), &sha(HEAD)))
            .is_some()
    );
    assert!(synced(&mut store, &f).unwrap().is_empty());
}

mod sync;
mod undo;
