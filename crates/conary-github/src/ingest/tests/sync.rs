//! A sync reads GitHub before it takes the ledger's lock, and records under
//! it only what's still so.

use std::cell::RefCell;
use std::fs::File;
use std::path::{Path, PathBuf};

use conary_core::policy::Paths;

use super::*;
use crate::api::{Comment, History};

/// GitHub through `hub`, noting each of a PR's own reads (its timeline,
/// check runs, statuses and reviews) made while another holds the ledger's
/// lock in `dir`, the way a writer waiting on it would find it.
struct Watch<'a> {
    hub: &'a Hub,
    dir: PathBuf,
    locked: RefCell<Vec<String>>,
}

impl<'a> Watch<'a> {
    fn new(hub: &'a Hub, dir: &Path) -> Self {
        Self {
            hub,
            dir: dir.to_owned(),
            locked: RefCell::default(),
        }
    }

    fn note(&self, read: &str) {
        let held = File::open(self.dir.join("lock")).is_ok_and(|f| f.try_lock().is_err());
        if held {
            self.locked.borrow_mut().push(read.to_owned());
        }
    }

    fn locked(&self) -> Vec<String> {
        self.locked.take()
    }
}

impl Source for Watch<'_> {
    fn diff(&self, repo: &str, base: &str, head: &str) -> Result<String, ApiError> {
        self.hub.diff(repo, base, head)
    }
    fn comments(&self, repo: &str, pr: u64) -> Result<Vec<Comment>, ApiError> {
        self.hub.comments(repo, pr)
    }
    fn compare_commits(&self, repo: &str, base: &str, head: &str) -> Result<History, ApiError> {
        self.hub.compare_commits(repo, base, head)
    }
    fn pull(&self, repo: &str, number: u64) -> Result<PullRequest, ApiError> {
        self.hub.pull(repo, number)
    }
    fn pulls(&self, repo: &str, which: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        self.hub.pulls(repo, which)
    }
    fn check_runs(&self, repo: &str, sha: &str) -> Result<Vec<CheckRun>, ApiError> {
        self.note(&format!("check runs {sha}"));
        self.hub.check_runs(repo, sha)
    }
    fn statuses(&self, repo: &str, sha: &str) -> Result<Vec<CommitStatus>, ApiError> {
        self.note(&format!("statuses {sha}"));
        self.hub.statuses(repo, sha)
    }
    fn reviews(&self, repo: &str, number: u64) -> Result<Vec<Review>, ApiError> {
        self.note(&format!("reviews {number}"));
        self.hub.reviews(repo, number)
    }
    fn commit(&self, repo: &str, sha: &str) -> Result<Commit, ApiError> {
        self.hub.commit(repo, sha)
    }
    fn commit_pulls(&self, repo: &str, sha: &str) -> Result<Vec<u64>, ApiError> {
        self.hub.commit_pulls(repo, sha)
    }
    fn timeline(&self, repo: &str, number: u64) -> Result<Vec<Event>, ApiError> {
        self.note(&format!("timeline {number}"));
        self.hub.timeline(repo, number)
    }
    fn merge_base(&self, repo: &str, a: &str, b: &str) -> Result<String, ApiError> {
        self.hub.merge_base(repo, a, b)
    }
    fn files(&self, repo: &str, base: &str, head: &str) -> Result<Paths, ApiError> {
        self.hub.files(repo, base, head)
    }
}

fn open_in(dir: &Path) -> Store {
    let lock = conary_core::log::Lock::take(dir).unwrap();
    Store::open(lock, SigningKey::from_bytes(&[5; 32]), None).unwrap()
}

// The minutes a sync spends reading each open PR's results are spent
// before the lock: every other writer waits only for the recording, and
// for each PR's timeline, read under it.
#[test]
fn sync_reads_open_prs_before_the_lock_and_records_them_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let f = fake();
    f.put_pulls(
        REPO,
        [pr(9, HEAD, "open", false), pr(10, HEAD2, "open", false)],
    );
    let watch = Watch::new(&f, dir.path());

    let gathered = gather(&watch, &config(), &repo(), dir.path()).unwrap();
    let mut store = open_in(dir.path());
    // Read now, it would be read under the lock.
    watch.timeline(REPO, 9).unwrap();
    assert_eq!(watch.locked(), ["timeline 9"]);

    let got = sync(&mut store, &watch, gathered).unwrap();
    assert_eq!(watch.locked(), ["timeline 9", "timeline 10"]);
    // Recorded from what was gathered: 9's evidence as reading it now gets.
    assert_eq!(got.len(), 2);
    let c = store.ledger().by_pr(&repo(), 9).unwrap();
    assert_eq!(c.evidence.len(), 5);
}

// Another writer may record a PR as it is after the sync gathered it and
// before the sync takes the lock: the sync records what it lists under the
// lock, reading again what changed, and never the older head.
#[test]
fn sync_reads_again_under_the_lock_what_changed_since_it_gathered() {
    let dir = tempfile::tempdir().unwrap();
    let f = fake();
    f.put_pulls(
        REPO,
        [pr(9, HEAD, "open", false), pr(10, HEAD2, "open", false)],
    );
    let watch = Watch::new(&f, dir.path());
    let gathered = gather(&watch, &config(), &repo(), dir.path()).unwrap();

    // 9 is pushed to and recorded so by another writer; 11 opens.
    f.put_pulls(
        REPO,
        [
            pr(9, HEAD2, "open", false),
            pr(10, HEAD2, "open", false),
            pr(11, HEAD, "open", false),
        ],
    );
    let mut store = open_in(dir.path());
    pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    watch.locked();

    let got = sync(&mut store, &watch, gathered).unwrap();
    assert_eq!(got.len(), 3);
    let c = store.ledger().by_pr(&repo(), 9).unwrap();
    let heads: Vec<_> = c.revisions.iter().map(|r| r.head.as_str()).collect();
    assert_eq!(heads, [HEAD2]);
    // Only what changed was read under the lock; 10 came from the gather.
    let mut locked = watch.locked();
    locked.sort();
    assert_eq!(
        locked,
        [
            format!("check runs {HEAD}"),
            format!("check runs {HEAD2}"),
            "reviews 11".into(),
            "reviews 9".into(),
            format!("statuses {HEAD}"),
            format!("statuses {HEAD2}"),
            "timeline 10".into(),
            "timeline 11".into(),
            "timeline 9".into(),
        ]
    );
}

/// The evidence on `revision` of 9, by check.
fn checks_on(store: &Store, revision: RevisionId) -> Vec<String> {
    let c = store.ledger().by_pr(&repo(), 9).unwrap();
    c.evidence
        .iter()
        .filter(|e| e.revision == revision)
        .map(|e| e.check.clone())
        .collect()
}

fn run(id: u64, name: &str, started: &str) -> Value {
    json!({"id": id, "name": name, "status": "completed", "conclusion": "success",
           "html_url": format!("https://github.com/x/y/actions/runs/{id}/job/{id}"),
           "started_at": started})
}

// Retargeted away and back after the gather, 9 is at the same head, branch
// and base, but what ran on the other base isn't this revision's: the
// timeline read under the lock leaves out what came before the retarget,
// as reading it all now would.
#[test]
fn sync_leaves_out_gathered_results_from_before_a_retarget_since() {
    let dir = tempfile::tempdir().unwrap();
    let f = fake();
    f.put_checks(
        HEAD,
        json!([
            run(4, "before", "2026-10-27T12:59:59Z"),
            run(5, "after", "2026-10-27T13:00:01Z"),
        ]),
    );
    let watch = Watch::new(&f, dir.path());
    let gathered = gather(&watch, &config(), &repo(), dir.path()).unwrap();

    f.put_event(REPO, 9, "base_ref_changed", "2026-10-27T12:59:00Z");
    f.put_event(REPO, 9, "base_ref_changed", "2026-10-27T13:00:00Z");
    let mut store = open_in(dir.path());
    sync(&mut store, &watch, gathered).unwrap();
    assert_eq!(watch.locked(), ["timeline 9"]);
    let revision = RevisionId::new(&sha(FORK), &sha(HEAD));
    assert_eq!(checks_on(&store, revision), ["after"]);

    let (_fresh_dir, mut fresh) = super::open();
    pull(&mut fresh, &f, &config(), &repo(), 9).unwrap();
    assert_eq!(checks_on(&fresh, revision), ["after"]);
}

// 9 at TIP, stacked on MID, which forked at FORK. Its base moves on to
// take in MID, or is rewritten onto DEV_FORK, after the gather: the same
// head and branch, but another merge base, so another revision. What was
// gathered stays the old revision's and 9 is read again.
#[test]
fn sync_reads_again_a_pr_whose_merge_base_changed_since_it_gathered() {
    const MID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const MOVED: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const REWRITTEN: &str = "dddddddddddddddddddddddddddddddddddddddd";
    for (moved, fork) in [(MOVED, MID), (REWRITTEN, DEV_FORK)] {
        let dir = tempfile::tempdir().unwrap();
        let f = fake();
        f.change(MID, FORK, "mid");
        f.change(TIP, MID, "tip");
        f.put_commit(MOVED, &[BASE, MID], false);
        f.change(REWRITTEN, DEV_FORK, "rewritten");
        f.put_pulls(REPO, [pr(9, TIP, "open", false)]);
        f.put_checks(TIP, json!([run(4, "first", "2026-10-27T12:00:00Z")]));
        let watch = Watch::new(&f, dir.path());
        let gathered = gather(&watch, &config(), &repo(), dir.path()).unwrap();

        let mut on = pr(9, TIP, "open", false);
        on["base"]["sha"] = json!(moved);
        f.set_branch(REPO, "main", moved);
        f.put_pulls(REPO, [on]);
        f.put_checks(
            TIP,
            json!([
                run(4, "first", "2026-10-27T12:00:00Z"),
                run(5, "second", "2026-10-27T12:10:00Z"),
            ]),
        );
        let mut store = open_in(dir.path());
        sync(&mut store, &watch, gathered).unwrap();

        let c = store.ledger().by_pr(&repo(), 9).unwrap();
        let revision = RevisionId::new(&sha(fork), &sha(TIP));
        assert_eq!(c.current, Some(revision), "{moved}");
        assert_eq!(checks_on(&store, revision), ["first", "second"], "{moved}");
        let mut locked = watch.locked();
        locked.sort();
        assert_eq!(
            locked,
            [
                format!("check runs {TIP}"),
                "reviews 9".into(),
                format!("statuses {TIP}"),
                "timeline 9".into(),
            ],
            "{moved}"
        );
    }
}
