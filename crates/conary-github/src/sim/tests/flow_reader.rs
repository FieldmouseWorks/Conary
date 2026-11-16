//! The GitHub-readable flow facts come from Hub, not assignments to Facts.
use super::{TRUNK, head};
use crate::api::Source;
use crate::flow::*;
use crate::sim::{Call, Hub, REMI};
use serde_json::json;

fn ready() -> (Hub, Memory) {
    let (hub, _, memory) = super::flow::setup();
    (hub, memory)
}
fn facts(hub: &Hub, settings: Settings) -> Facts {
    read(
        hub,
        &REMI.parse().unwrap(),
        3,
        &conary_core::Policy::parse(br#"{"r2":["^secrets/"],"docs":["^docs/"]}"#).unwrap(),
        settings,
        &["owner".into()],
    )
    .unwrap()
}
fn decision(hub: &Hub, memory: &Memory, settings: Settings) -> Next {
    next(&facts(hub, settings), memory, 4000).0
}

#[test]
fn engine_trailer_anywhere_in_compared_history_requires_judgment() {
    // codex_heads.py:23-41, only the whole trailer counts, not quoted prose.
    for (message, judge) in [
        ("change: code\n\nEngine: codex\n", true),
        ("change: code\n\neNgInE: codex\n", true),
        ("change: code\n\nOriginal-engine: codex\n", false),
        ("change: code\n\nEngine: codex, once\n", false),
        (
            "change: prose\n\nEngine: codex\n\nThat was a quoted line.\n",
            false,
        ),
        ("change: code\n", false),
    ] {
        let (hub, m) = ready();
        let earlier = head(2);
        hub.change(&earlier, TRUNK, "earlier");
        hub.commit_details(
            &earlier,
            Some(message),
            Some("owner@example.org"),
            Some("owner@example.org"),
        );
        hub.change(&head(3), &earlier, "later");
        assert_eq!(
            decision(&hub, &m, crate::sim::settings()).step,
            if judge {
                Step::Judge(vec![Trigger::AuthorEngine])
            } else {
                Step::Merge
            },
            "{message}"
        );
    }
}

#[test]
fn history_past_the_pr_endpoints_commit_cap_is_read_whole() {
    // codex_heads.py:43-56: 251st commit's engine trailer still counts.
    for judge in [true, false] {
        let (hub, m) = ready();
        let mut parent = TRUNK.to_string();
        for i in 10..260 {
            let sha = head(i);
            hub.change(&sha, &parent, &sha);
            parent = sha;
        }
        hub.change(&head(3), &parent, "last");
        hub.commit_details(
            &head(3),
            Some(if judge {
                "change: code\n\nEngine: codex\n"
            } else {
                "change: code"
            }),
            Some("owner@example.org"),
            Some("owner@example.org"),
        );
        let f = facts(&hub, crate::sim::settings());
        assert!(f.authorship_known);
        assert_eq!(
            next(&f, &m, 4000).0.step,
            if judge {
                Step::Judge(vec![Trigger::AuthorEngine])
            } else {
                Step::Merge
            }
        );
        assert_eq!(hub.count(Call::CompareCommits), 2); // initial setup plus this pass
    }
}

#[test]
fn unreadable_history_waits_then_a_later_read_can_land() {
    // codex_heads.py:58-82: failed reads are not cached as permission to land.
    let (hub, m) = ready();
    hub.fail(Call::CompareCommits);
    let f = facts(&hub, crate::sim::settings());
    assert!(!f.authorship_known);
    assert_eq!(next(&f, &m, 4000).0.stage, Stage::Review);
    hub.heal(Call::CompareCommits);
    assert_eq!(decision(&hub, &m, crate::sim::settings()).step, Step::Merge);
    // This malformed GitHub answer is served as such, not turned into a
    // syntactically valid commit by the test fixture.
    let raw = json!({"commit":{"message":null,"author":{"email":"a"},"committer":{"email":"a"},"verification":null}});
    hub.commit_response(&head(3), raw);
    assert!(!facts(&hub, crate::sim::settings()).authorship_known);
}

#[test]
fn bot_history_counts_every_author_committer_and_coauthor() {
    // codex_heads.py:120-145 / touches.py: edited histories lose CI-only review.
    let bot = "49699333+dependabot[bot]@users.noreply.github.com";
    for (author, committer, trailer, edited) in [
        (Some(bot), Some(bot), "", false),
        (Some(bot), Some("noreply@github.com"), "", false),
        (Some("owner@example.org"), Some(bot), "", true),
        (Some(bot), Some("owner@example.org"), "", true),
        (
            Some(bot),
            Some(bot),
            "\n\nCo-authored-by: Person <owner@example.org>\n",
            true,
        ),
        (Some(bot), Some(bot), "\n\nCo-authored-by: Person\n", true),
        (None, Some(bot), "", true),
        (Some(bot), None, "", true),
    ] {
        let (hub, m) = ready();
        hub.edit_pull(REMI, 3, |p| {
            p["head"]["ref"] = json!("dependabot/npm/pkg-2")
        });
        hub.refresh_flow(REMI, 3);
        hub.remove_flow_reviews(REMI, 3);
        hub.commit_details(
            &head(3),
            Some(&format!("deps: bump{trailer}")),
            author,
            committer,
        );
        let f = facts(&hub, crate::sim::settings());
        assert_eq!(f.bot_edited, Some(edited));
        assert_eq!(
            next(&f, &m, 4000).0.step,
            if edited {
                Step::QueueReview("reviewer".into())
            } else {
                Step::Merge
            }
        );
    }
}

#[test]
fn renovate_email_is_machine_configuration_not_the_branchs_claim() {
    // touches.py / deps.py: absent Renovate email never grants bot-only review.
    let (hub, m) = ready();
    hub.edit_pull(REMI, 3, |p| p["head"]["ref"] = json!("renovate/pkg"));
    hub.refresh_flow(REMI, 3);
    hub.commit_details(
        &head(3),
        Some("deps: bump"),
        Some("renovate@example.org"),
        Some("renovate@example.org"),
    );
    hub.remove_flow_reviews(REMI, 3);
    assert_eq!(
        decision(&hub, &m, crate::sim::settings()).step,
        Step::QueueReview("reviewer".into())
    );
    let settings = Settings {
        renovate_email: Some("renovate@example.org".into()),
        ..crate::sim::settings()
    };
    assert_eq!(decision(&hub, &m, settings).step, Step::Settle);
}

#[test]
fn merge_emptiness_uses_a_validated_test_merge_without_writing() {
    // landed.py: the merge's result tree, not head-vs-base equality, decides.
    let (hub, m) = ready();
    assert_eq!(facts(&hub, crate::sim::settings()).empty, Some(false));
    let landed = hub.change(&head(50), TRUNK, &head(3));
    let unrelated = hub.add(&landed, "unrelated");
    hub.set_branch(REMI, "main", &unrelated);
    let f = facts(&hub, crate::sim::settings());
    assert_eq!(f.empty, Some(true));
    assert_eq!(next(&f, &m, 4000).0.step, Step::CloseAlreadyLanded);
    assert_eq!(hub.count(Call::Merge), 0);
    assert_eq!(hub.count(Call::CreateBranch), 0);
}

#[test]
fn stale_test_merge_cannot_prove_the_current_base_or_head() {
    // landed.py: old merge previews prove nothing after either parent moves.
    let (hub, m) = ready();
    let sha = hub.pull(REMI, 3).unwrap().merge_commit_sha.unwrap();
    // A known ordinary commit is not the test merge of these exact parents.
    hub.edit_pull(REMI, 3, |p| p["merge_commit_sha"] = json!(TRUNK));
    let f = facts(&hub, crate::sim::settings());
    assert_eq!(f.empty, None);
    assert!(matches!(
        next(&f, &m, 4000).0.step,
        Step::Retry {
            kind: Failure::Empty,
            ..
        }
    ));
    hub.edit_pull(REMI, 3, |p| p["merge_commit_sha"] = json!(sha));
    let newer = hub.add(TRUNK, "newer");
    hub.set_branch(REMI, "main", &newer);
    assert_eq!(facts(&hub, crate::sim::settings()).empty, None);
}

#[test]
fn threads_and_mergeability_are_read_from_the_sim() {
    // handle.py / conflicts.py: outstanding threads block even clean reviews;
    // conflicts bypass reviewing/CI and are handed off immediately.
    let (hub, m) = ready();
    hub.edit_flow(REMI, 3, |s| s.open_threads = 1);
    assert_eq!(
        decision(&hub, &m, crate::sim::settings()).step,
        Step::HandFix(Kind::Findings)
    );
    hub.edit_flow(REMI, 3, |s| {
        s.open_threads = 0;
        s.merge_state = "DIRTY".into();
    });
    assert_eq!(
        decision(&hub, &m, crate::sim::settings()).step,
        Step::HandFix(Kind::Conflict)
    );
    hub.edit_flow(REMI, 3, |s| {
        s.merge_state = "BEHIND".into();
    });
    assert_eq!(
        decision(&hub, &m, crate::sim::settings()).step,
        Step::UpdateBranch
    );
}

#[test]
fn a_permanently_short_history_requires_a_judge_not_unread_authorship_retry() {
    // codex_heads.py:81 / 130: a stable partial comparison cannot rule out another engine or a human edit.
    let (hub, m) = ready();
    let mut tip = TRUNK.to_owned();
    for i in 0..300 {
        let sha = format!("{:040x}", 1000 + i);
        hub.put_commit(&sha, &[&tip], false);
        tip = sha;
    }
    hub.push(REMI, 3, &tip);
    hub.refresh_flow(REMI, 3);
    hub.ci(&tip, &[("check", "success")]);
    hub.flow_review(REMI, 3, "reviewer", Verdict::Clean, 600);
    hub.short_history(TRUNK, &tip, 250);
    let f = facts(&hub, crate::sim::settings());
    assert!(f.authorship_known && f.needs_judge);
    assert_eq!(f.bot_edited, None);
    assert_eq!(
        next(&f, &m, 700).0.step,
        Step::Judge(vec![Trigger::AuthorEngine])
    );
    assert_eq!(
        decision(&hub, &m, crate::sim::settings()).step,
        Step::Judge(vec![Trigger::AuthorEngine])
    );
}

#[test]
fn a_coauthor_on_the_251st_bot_commit_removes_the_ci_only_exemption() {
    // codex_heads.py:117: all authors, committers and coauthors count beyond the PR endpoint's cap, with or without Renovate configured.
    let bot = "49699333+dependabot[bot]@users.noreply.github.com";
    for renovate in [None, Some("renovate@example.org")] {
        for (trailer, edited) in [
            ("", false),
            ("\n\nCo-authored-by: Person <person@example.org>\n", true),
            ("\n\nCo-authored-by: Person\n", true),
        ] {
            let mut c = super::flow_case::Case::new();
            c.roles(&["reviewer"]);
            c.settings.renovate_email = renovate.map(str::to_owned);
            let mut tip = TRUNK.to_owned();
            for i in 0..251 {
                let sha = format!("{:040x}", 1000 + i);
                c.hub.put_commit(&sha, &[&tip], false);
                c.hub
                    .commit_details(&sha, Some("deps: bump"), Some(bot), Some(bot));
                tip = sha;
            }
            c.hub.commit_details(
                &tip,
                Some(&format!("deps: bump{trailer}")),
                Some(bot),
                Some(bot),
            );
            c.push(&tip);
            c.hub
                .edit_pull(REMI, 3, |p| p["head"]["ref"] = json!("dependabot/pkg"));
            c.hub.refresh_flow(REMI, 3);
            c.memory.head = tip;
            let f = c.facts();
            assert_eq!(f.bot_edited, Some(edited));
            assert_eq!(
                next(&f, &c.memory, 700).0.step,
                if edited {
                    Step::QueueReview("reviewer".into())
                } else {
                    Step::Merge
                }
            );
        }
    }
}

#[test]
fn a_red_fix_lands_only_on_the_default_tip_observed_red() {
    // landed.py:152,163,167: red-fix guard binds CI to the observed tip.
    for (branch, state, moved, held) in [
        ("autoland/red-cccccccccccc", "failure", true, true),
        ("feature", "failure", true, false),
        ("autoland/red-cccccccccccc", "failure", false, false),
        ("autoland/red-cccccccccccc", "success", false, true),
        ("autoland/red-cccccccccccc", "pending", false, true),
    ] {
        let c = super::flow_case::Case::new();
        c.hub.edit_pull(REMI, 3, |p| {
            p["head"]["ref"] = json!(branch);
            p["head"]["repo"]["full_name"] = json!(REMI);
        });
        c.hub.refresh_flow(REMI, 3);
        c.hub.ci(TRUNK, &[("main-ci", state)]);
        if moved {
            c.hub.stale(Call::FlowSnapshot);
            let newer = head(80);
            c.hub.change(&newer, TRUNK, "new main");
            c.hub.set_branch(REMI, "main", &newer);
        }
        let f = c.facts();
        assert_eq!(f.base_held, held, "{branch} {state} moved={moved}");
        assert_eq!(next(&f, &c.memory, 4000).0.stage == Stage::Base, held);
    }
}
