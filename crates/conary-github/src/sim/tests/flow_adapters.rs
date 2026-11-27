//! Flow simulator tests for configurable reviewer adapters (`settings.review_adapter`)
//! and red-fix branch prefix (`settings.red_fix_branch`), verifying non-Codex adapters
//! and custom prefixes work end to end and old hardcoded constants are not relied on (#618).
use super::flow_pass_case::{Case, REPO};
use super::flow_pass_slots::{another, prepared, two};
use super::{TRUNK, head};
use crate::api::Source;
use crate::branches::Branches;
use crate::flow::{
    self, CustomReviewAdapter, DEFAULT_RED_FIX_BRANCH, Disputed, Kind, Level, Pass, Rank,
    ReviewAdapter, ReviewAsk, Stage, Step,
};
use crate::sim::STEWARD;
use conary_core::plugin::{Problem, Severity, Task};
use serde_json::json;

const ACME_BOT: &str = "acme-reviewer[bot]";

fn post_marker_summary(c: &Case, author: &str, marker: &str, status: &str, findings: Option<bool>) {
    let sha = head(3);
    let payload = json!({
        "commit_sha": sha,
        "status": status,
        "has_any_findings": findings,
    });
    c.hub.flow_comment(
        REPO,
        3,
        author,
        &format!("<!-- {marker} {payload} -->\n\nReview Summary"),
    );
}

fn post_marker_receipt(c: &Case, author: &str, marker: &str, commit: &str) {
    let payload = json!({"commit_sha": commit});
    c.hub.person_review(
        REPO,
        3,
        author,
        "Bot",
        commit,
        &format!("<!-- {marker} {payload} -->\n\nAutomated findings."),
        "COMMENTED",
    );
}

#[test]
fn neutral_conary_reviewer_adapter_asks_reads_hands_findings_and_lands() {
    let mut c = Case::new();
    c.config.settings.review_adapter = ReviewAdapter::Conary;
    c.config.settings.review_authors = [ACME_BOT.into()].into();

    let now = flow::jobs::now();
    // 1. Codex-formatted summary from the configured review author is IGNORED under Conary adapter.
    let codex_state = json!({
        "code_review": {"commit_sha": head(3), "status": "completed", "has_any_findings": false}
    });
    c.hub.flow_comment(
        REPO,
        3,
        ACME_BOT,
        &format!("<!-- codex-review-state:v1 {codex_state} -->"),
    );
    // First pass sees the PR unreviewed; after quiet period, AskReview posts `/conary review`.
    let r = c.pass(now, false);
    assert_eq!(r[0].next.as_ref().unwrap().stage, Stage::Review);
    let r = c.pass(now + flow::QUIET, false);
    assert!(matches!(
        r[0].next.as_ref().unwrap().step,
        Step::AskReview(_)
    ));
    let comments: Vec<String> = c
        .hub
        .comments(REPO, 3)
        .unwrap()
        .into_iter()
        .filter_map(|cm| cm.body)
        .collect();
    assert!(
        comments.iter().any(|b| b == "/conary review"),
        "expected neutral /conary review trigger: {comments:?}"
    );
    assert!(
        !comments.iter().any(|b| b.contains("@codex")),
        "hardcoded @codex trigger must not be posted: {comments:?}"
    );

    // 2. Neutral running summary puts PR in Stage::Review waiting.
    post_marker_summary(&c, ACME_BOT, "conary-review-state:v1", "running", None);
    let r = c.pass(now + flow::QUIET + 10, false);
    assert_eq!(r[0].next.as_ref().unwrap().stage, Stage::Review);
    assert_eq!(r[0].next.as_ref().unwrap().step, Step::Wait { until: None });

    // 3. Findings review + open threads: neutral `[P2]` tag is parsed as P2, while a Codex
    //    `![P2 Badge]` is NOT recognized under Conary and defaults to blocking P1.
    post_marker_receipt(&c, ACME_BOT, "conary-review-receipt:v1", &head(3));
    c.hub.flow_thread_at(
        REPO,
        3,
        "T_NEUTRAL",
        ("src/lib.rs", 10),
        (ACME_BOT, "[P2] Prefer explicit error type."),
    );
    c.hub.flow_thread_at(
        REPO,
        3,
        "T_CODEX_BADGE",
        ("src/lib.rs", 20),
        (
            ACME_BOT,
            "![P2 Badge](https://img.shields.io/badge/P2-yellow) Nit.",
        ),
    );
    let r = c.pass(now + flow::QUIET + 20, false);
    assert_eq!(
        r[0].next.as_ref().unwrap().step,
        Step::HandFix(Kind::Findings)
    );
    let inv_path = c.deferred.0.borrow()[0].clone();
    let inv: flow::jobs::Invocation =
        serde_json::from_slice(&std::fs::read(&inv_path).unwrap()).unwrap();
    let Task::Fix { problems, .. } = inv.task else {
        panic!("expected Fix task");
    };
    let severities: Vec<(u64, Severity)> = problems
        .iter()
        .map(|p| match p {
            Problem::Finding { finding } => (finding.line, finding.severity),
            other => panic!("unexpected problem: {other:?}"),
        })
        .collect();
    assert!(
        severities.contains(&(10, Severity::P2)),
        "neutral [P2] tag must parse as P2: {severities:?}"
    );
    assert!(
        severities.contains(&(20, Severity::P1)),
        "Codex badge must not parse as P2 under Conary adapter: {severities:?}"
    );

    // Clear the in-flight fix and resolve threads, then post a neutral clean summary and land.
    c.deferred.0.borrow_mut().clear();
    c.hub.flow_thread(REPO, 3, "T_NEUTRAL", true);
    c.hub.flow_thread(REPO, 3, "T_CODEX_BADGE", true);
    // Use a fresh Case with clean neutral review so no in-flight job blocks landing.
    let mut clean = Case::new();
    clean.config.settings.review_adapter = ReviewAdapter::Conary;
    clean.config.settings.review_authors = [ACME_BOT.into()].into();
    post_marker_summary(
        &clean,
        ACME_BOT,
        "conary-review-state:v1",
        "completed",
        Some(false),
    );
    let r = clean.pass(now, false);
    assert_eq!(r[0].error, None);
    assert!(clean.hub.pull(REPO, 3).unwrap().is_merged());
    let store = clean.store.borrow();
    let change = store.ledger().by_pr(&REPO.parse().unwrap(), 3).unwrap();
    let checks: Vec<_> = change
        .evidence
        .iter()
        .filter(|e| e.kind == conary_core::op::CheckKind::Review)
        .map(|e| (e.check.as_str(), e.result, e.attest.is_some()))
        .collect();
    assert_eq!(
        checks,
        [("conary", conary_core::op::Outcome::Pass, false)],
        "ledger review evidence must record the neutral adapter's check name, not codex"
    );
}

#[test]
fn custom_reviewer_adapter_uses_its_own_trigger_markers_and_check_name() {
    let mut c = Case::new();
    c.config.settings.review_adapter = ReviewAdapter::Custom(CustomReviewAdapter {
        check_name: "acme-review".into(),
        ask_comment: Some("@acme-bot please review".into()),
        state_marker: "acme-review-state:v1".into(),
        receipt_marker: "acme-review-receipt:v1".into(),
    });
    c.config.settings.review_authors = [ACME_BOT.into()].into();

    let now = flow::jobs::now();
    // Neither Codex nor default Conary markers count for this custom adapter.
    let codex_state = json!({
        "code_review": {"commit_sha": head(3), "status": "completed", "has_any_findings": false}
    });
    c.hub.flow_comment(
        REPO,
        3,
        ACME_BOT,
        &format!("<!-- codex-review-state:v1 {codex_state} -->"),
    );
    post_marker_summary(
        &c,
        ACME_BOT,
        "conary-review-state:v1",
        "completed",
        Some(false),
    );
    c.pass(now, false);
    let r = c.pass(now + flow::QUIET, false);
    assert!(matches!(
        r[0].next.as_ref().unwrap().step,
        Step::AskReview(_)
    ));
    let comments: Vec<String> = c
        .hub
        .comments(REPO, 3)
        .unwrap()
        .into_iter()
        .filter_map(|cm| cm.body)
        .collect();
    assert!(
        comments.iter().any(|b| b == "@acme-bot please review"),
        "expected custom trigger comment: {comments:?}"
    );
    assert!(
        !comments
            .iter()
            .any(|b| b == "@codex review" || b == "/conary review"),
        "built-in triggers must not be posted: {comments:?}"
    );

    // Malformed custom marker degrades Mark::Review and blocks landing.
    let bad_idx = c.hub.comments(REPO, 3).unwrap().len();
    c.hub
        .flow_comment(REPO, 3, ACME_BOT, "<!-- acme-review-state:v1 {bad} -->");
    let r = c.pass(now + flow::QUIET + 1, false);
    assert!(
        r[0].next
            .as_ref()
            .unwrap()
            .degraded
            .iter()
            .any(|d| matches!(
                d.what,
                flow::Degraded::Mark {
                    mark: flow::Mark::Review
                }
            )),
        "malformed custom review mark must degrade Mark::Review: {:?}",
        r[0].next
    );
    assert!(!c.hub.pull(REPO, 3).unwrap().is_merged());

    // Fix the comment to a clean custom marker: PR settles and lands under check name `acme-review`.
    let clean_payload = json!({
        "commit_sha": head(3),
        "status": "completed",
        "has_any_findings": false,
    });
    c.hub.edit_flow_comment(
        REPO,
        3,
        bad_idx,
        &format!("<!-- acme-review-state:v1 {clean_payload} -->\n\nLooks good."),
    );
    let r = c.pass(now + flow::QUIET + 2, false);
    assert_eq!(r[0].error, None);
    assert!(c.hub.pull(REPO, 3).unwrap().is_merged());
    let store = c.store.borrow();
    let change = store.ledger().by_pr(&REPO.parse().unwrap(), 3).unwrap();
    let checks: Vec<_> = change
        .evidence
        .iter()
        .filter(|e| e.kind == conary_core::op::CheckKind::Review)
        .map(|e| (e.check.as_str(), e.result))
        .collect();
    assert_eq!(checks, [("acme-review", conary_core::op::Outcome::Pass)]);
}

#[test]
fn custom_adapter_without_ask_comment_uses_review_ask_door_or_fails_closed() {
    let mut c = Case::new();
    c.config.settings.review_adapter = ReviewAdapter::Custom(CustomReviewAdapter {
        check_name: "door-only".into(),
        ask_comment: None,
        state_marker: "door-state:v1".into(),
        receipt_marker: "door-receipt:v1".into(),
    });
    c.config.settings.review_authors = [ACME_BOT.into()].into();
    let now = flow::jobs::now();
    c.pass(now, false);
    let r = c.pass(now + flow::QUIET, false);
    assert!(
        r[0].error
            .as_deref()
            .is_some_and(|e| e.contains("no comment trigger")),
        "missing ask_comment with no covering door must fail closed: {:?}",
        r[0]
    );

    // With a covering review_ask door, AskReview succeeds once retry backoff expires.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("door");
    std::fs::write(&script, "exit 0\n").unwrap();
    c.config.review_ask = Some(ReviewAsk {
        command: vec!["/bin/sh".into(), script.display().to_string()],
        repos: vec![REPO.parse().unwrap()],
    });
    let mut t = now + flow::QUIET;
    let asked_step = loop {
        t += 600;
        assert!(t < now + flow::QUIET + 3600, "door ask never ran");
        let r = c.pass(t, false);
        assert_eq!(r[0].error, None);
        if matches!(r[0].next.as_ref().unwrap().step, Step::AskReview(_)) {
            break r[0].next.as_ref().unwrap().step.clone();
        }
    };
    assert!(matches!(asked_step, Step::AskReview(_)));
    assert_eq!(c.record().memory.asked, Some((head(3), t)));
}

#[test]
fn disputed_p2_uses_configured_review_adapters_severity_parser() {
    let mut c = Case::new();
    c.config.settings.review_adapter = ReviewAdapter::Conary;
    c.config.settings.review_authors = [ACME_BOT.into()].into();
    c.config.settings.disputed_p2 = Disputed::Judge;
    post_marker_receipt(&c, ACME_BOT, "conary-review-receipt:v1", &head(3));
    let body = "[P2] Rename helper for clarity.";
    // Earlier resolved thread answered by flow_authors, raised again unchanged.
    c.hub
        .flow_thread_at(REPO, 3, "T_OLD", ("src/lib.rs", 5), (ACME_BOT, body));
    c.hub
        .flow_thread_comments(REPO, 3, "T_OLD", &[(ACME_BOT, body), (STEWARD, "Kept.")]);
    c.hub.flow_thread(REPO, 3, "T_OLD", true);
    c.hub
        .flow_thread_at(REPO, 3, "T_NEW", ("src/lib.rs", 5), (ACME_BOT, body));

    let r = c.pass(flow::jobs::now(), true);
    assert_eq!(
        r[0].next.as_ref().unwrap().step,
        Step::Concede,
        "neutral [P2] repeated finding must be conceded when disputed_p2 = judge"
    );

    // A Codex badge repeated finding is NOT treated as P2 under Conary (defaults to P1, so HandFix).
    let mut c2 = Case::new();
    c2.config.settings.review_adapter = ReviewAdapter::Conary;
    c2.config.settings.review_authors = [ACME_BOT.into()].into();
    c2.config.settings.disputed_p2 = Disputed::Judge;
    post_marker_receipt(&c2, ACME_BOT, "conary-review-receipt:v1", &head(3));
    let codex_body = "![P2 Badge](https://img.shields.io/badge/P2-yellow) Nit.";
    c2.hub
        .flow_thread_at(REPO, 3, "T_OLD", ("src/lib.rs", 5), (ACME_BOT, codex_body));
    c2.hub.flow_thread_comments(
        REPO,
        3,
        "T_OLD",
        &[(ACME_BOT, codex_body), (STEWARD, "Kept.")],
    );
    c2.hub.flow_thread(REPO, 3, "T_OLD", true);
    c2.hub
        .flow_thread_at(REPO, 3, "T_NEW", ("src/lib.rs", 5), (ACME_BOT, codex_body));
    let now = flow::jobs::now();
    c2.pass(now, false);
    let r2 = c2.pass(now + 10, true);
    assert_eq!(
        r2[0].next.as_ref().unwrap().step,
        Step::HandFix(Kind::Findings),
        "Codex badge must not parse as P2 under neutral Conary adapter"
    );
}

#[test]
fn red_fix_branch_defaults_to_neutral_conary_prefix_and_rejects_old_hardcoded_prefix() {
    let mut c = Case::new();
    c.config.settings.red_fix_branch = DEFAULT_RED_FIX_BRANCH.into();
    let trusted = &c.config.trusted_authors;

    // Neutral default `conary/red-` is recognized as red-fix (High).
    c.hub.edit_pull(REPO, 3, |p| {
        p["head"]["ref"] = json!("conary/red-cccccccccccc");
        p["head"]["repo"]["full_name"] = json!(REPO);
        p["base"]["ref"] = json!("main");
        p["user"]["login"] = json!("owner");
    });
    let pull = c.hub.pull(REPO, 3).unwrap();
    assert!(flow::red_fix(&c.config.settings, trusted, &pull));
    assert_eq!(
        Rank::of(&c.config.settings, trusted, &pull).level,
        Level::High
    );

    // Old hardcoded `autoland/red-` is NOT recognized under the neutral default.
    c.hub.edit_pull(REPO, 3, |p| {
        p["head"]["ref"] = json!("autoland/red-cccccccccccc");
    });
    let pull = c.hub.pull(REPO, 3).unwrap();
    assert!(!flow::red_fix(&c.config.settings, trusted, &pull));
    assert_eq!(
        Rank::of(&c.config.settings, trusted, &pull).level,
        Level::Medium
    );
}

#[test]
fn custom_red_fix_branch_prefix_drives_priority_slots_train_and_base_hold() {
    // Proof slot ordering with a custom prefix `ops/trunk-fix-`:
    // PR 5 (newer) on `ops/trunk-fix-1` beats older PR 3 on `autoland/red-1`.
    let mut c = two(1);
    c.config.settings.red_fix_branch = "ops/trunk-fix-".into();
    c.hub.edit_pull(REPO, 3, |p| {
        p["created_at"] = json!("2099-01-01T00:00:00Z");
        p["head"]["ref"] = json!("autoland/red-111111111111");
    });
    c.hub.edit_pull(REPO, 5, |p| {
        p["created_at"] = json!("2099-02-01T00:00:00Z");
        p["head"]["ref"] = json!("ops/trunk-fix-222222222222");
    });
    c.hub.refresh_flow(REPO, 3);
    c.hub.refresh_flow(REPO, 5);
    c.hub.edit_flow(REPO, 5, |s| s.default_branch = None);
    let trunk = c.hub.branch(REPO, "main").unwrap().unwrap();
    c.hub.ci(&trunk, &[("main-ci", "failure")]);

    let now = flow::jobs::now();
    prepared(&c, now, true);
    let reports = c.pass(now + 1, false);
    let five = reports.iter().find(|r| r.pr == 5).unwrap();
    let three = reports.iter().find(|r| r.pr == 3).unwrap();
    assert_eq!(five.next.as_ref().unwrap().priority, Level::High);
    assert_eq!(five.next.as_ref().unwrap().step, Step::Prove);
    assert_eq!(
        three.next.as_ref().unwrap().priority,
        Level::Medium,
        "old autoland/red- prefix must not get High priority when custom prefix is set"
    );
    assert_eq!(three.next.as_ref().unwrap().stage, Stage::ProofSlot);

    // Base hold applies to `ops/trunk-fix-` when trunk is green, and NOT to `conary/red-` or `autoland/red-`.
    let mut c2 = Case::new();
    c2.config.settings.red_fix_branch = "ops/trunk-fix-".into();
    c2.clean_review();
    c2.hub.ci(TRUNK, &[("main-ci", "success")]);
    for (branch, expected_base_hold) in [
        ("ops/trunk-fix-abc", true),
        ("conary/red-abc", false),
        ("autoland/red-abc", false),
    ] {
        c2.hub.edit_pull(REPO, 3, |p| {
            p["head"]["ref"] = json!(branch);
            p["head"]["repo"]["full_name"] = json!(REPO);
        });
        c2.hub.refresh_flow(REPO, 3);
        c2.hub.edit_flow(REPO, 3, |s| s.default_branch = None);
        let reports = c2.pass(now, true);
        let stage = reports[0].next.as_ref().unwrap().stage;
        assert_eq!(
            stage == Stage::Base,
            expected_base_hold,
            "branch {branch} -> stage {stage:?}"
        );
    }

    // Train admission puts custom `ops/trunk-fix-` ahead of older `autoland/red-`.
    let mut c3 = Case::new();
    c3.config.settings.train = true;
    c3.config.settings.red_fix_branch = "ops/trunk-fix-".into();
    c3.clean_review();
    another(&c3, 5);
    for n in [3, 5] {
        c3.hub
            .post_status(&head(n), json!({"context": "machine", "state": "success"}));
    }
    c3.hub.edit_pull(REPO, 3, |p| {
        p["created_at"] = json!("2099-01-01T00:00:00Z");
        p["head"]["ref"] = json!("autoland/red-111111111111");
    });
    c3.hub.edit_pull(REPO, 5, |p| {
        p["created_at"] = json!("2099-02-01T00:00:00Z");
        p["head"]["ref"] = json!("ops/trunk-fix-222222222222");
    });
    c3.hub.refresh_flow(REPO, 3);
    c3.hub.refresh_flow(REPO, 5);
    c3.hub.edit_flow(REPO, 5, |s| s.default_branch = None);
    let trunk = c3.hub.branch(REPO, "main").unwrap().unwrap();
    c3.hub.ci(&trunk, &[("main-ci", "failure")]);
    let ledger = tempfile::tempdir().unwrap();
    let lock = conary_core::log::Lock::take(ledger.path()).unwrap();
    let mut store =
        conary_core::Store::open(lock, ed25519_dalek::SigningKey::from_bytes(&[5; 32]), None)
            .unwrap();
    let reports = Pass {
        config: &c3.config,
        policy: &c3.policy,
        root: c3.dir.path(),
        launcher: &c3.deferred,
        now,
        dry_run: false,
        ledger: None,
    }
    .run(&c3.hub, &REPO.parse().unwrap(), None, Some(&mut store))
    .unwrap();
    assert!(reports.iter().all(|r| r.error.is_none()), "{reports:?}");
    let joined: Vec<u64> = store
        .ledger()
        .train(&REPO.parse().unwrap())
        .unwrap()
        .joined()
        .iter()
        .map(|m| store.ledger().change(m.change).unwrap().pr)
        .collect();
    assert_eq!(joined, vec![5, 3]);
}
