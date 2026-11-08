use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use serde_json::json;
fn dirty() -> Case {
    let c = Case::new();
    c.hub.edit_flow(REMI, 3, |s| s.merge_state = "DIRTY".into());
    c
}
fn bot(renovate: bool, major: bool, edited: bool) -> Case {
    let mut c = dirty();
    let branch = if renovate {
        "renovate/pkg"
    } else {
        "dependabot/pkg"
    };
    let title = if major {
        "Update pkg from 1.4 to 2.0"
    } else {
        "Update pkg from 1.4 to 1.5"
    };
    c.settings.renovate_email = Some("renovate@example.org".into());
    c.hub.edit_pull(REMI, 3, |p| {
        p["head"]["ref"] = json!(branch);
        p["title"] = json!(title);
        p["created_at"] = json!("1970-01-01T00:00:00Z");
    });
    c.hub.refresh_flow(REMI, 3);
    let email = if edited {
        "person@example.org"
    } else if renovate {
        "renovate@example.org"
    } else {
        "49699333+dependabot[bot]@users.noreply.github.com"
    };
    c.hub.commit_details(
        &c.scope().head,
        Some("deps: update"),
        Some(email),
        Some(email),
    );
    c
}
#[test]
fn conflicts_are_fixed_before_any_review_ci_or_judge_landing() {
    // conflicts.py:28 / 111 / 130: ordinary, one-way and release PRs all merge the base first.
    for scenario in 0..3 {
        let mut c = dirty();
        match scenario {
            0 => {
                c.roles(&["reviewer"]);
                c.no_ci();
            }
            1 => {
                c.secret();
                c.judge("land");
            }
            _ => {
                c.hub.set_branch(REMI, "release", super::TRUNK);
                c.hub.retarget(REMI, 3, "release");
                c.hub.refresh_flow(REMI, 3);
            }
        }
        assert_eq!(c.view(700).step, Step::HandFix(Kind::Conflict));
    }
}
#[test]
fn a_retargeted_conflict_waits_for_the_old_bases_job_then_starts_fresh() {
    // conflicts.py:66: old-base retries never count against the new base, but live jobs keep the branch.
    let mut c = dirty();
    let old = c.scope();
    c.memory.unanswered.insert(
        (Kind::Conflict, old.clone()),
        std::collections::BTreeSet::from(["old-1".into(), "old-2".into()]),
    );
    c.jobs.insert(
        Kind::Conflict,
        Job {
            id: "old-2".into(),
            revision: old,
            state: JobState::Running,
            started: 700,
            repeated: true,
        },
    );
    c.hub.set_branch(REMI, "release", super::TRUNK);
    c.hub.retarget(REMI, 3, "release");
    c.hub.refresh_flow(REMI, 3);
    for state in [JobState::Running, JobState::Queued, JobState::Unknown] {
        c.jobs.get_mut(&Kind::Conflict).unwrap().state = state;
        assert!(matches!(c.view(1100).step, Step::Wait { .. }));
    }
    c.jobs.get_mut(&Kind::Conflict).unwrap().state = JobState::Done;
    assert_eq!(c.view(1200).step, Step::HandFix(Kind::Conflict));
}
#[test]
fn only_a_bots_unedited_branch_is_left_for_the_bot_to_rebase() {
    // conflicts.py:134 / 138 / 145: both bots rebase their own, edited branches go to our conflict job.
    for renovate in [false, true] {
        let c = bot(renovate, false, false);
        let n = c.view(90000);
        assert!(matches!(n.step, Step::Wait { .. }));
        assert_eq!(
            n.owner_notice,
            if renovate {
                None
            } else {
                Some(OwnerNotice::OpenOverDay)
            }
        );
        let c = bot(renovate, false, true);
        assert_eq!(c.view(700).step, Step::HandFix(Kind::Conflict));
    }
    let mut c = bot(true, true, true);
    c.sweep("ok");
    assert_eq!(c.view(700).step, Step::HandFix(Kind::Conflict));
}
#[test]
fn swept_and_held_conflicts_never_get_a_second_fix_job() {
    // conflicts.py:164 / 175: red or unapproved majors belong to the sweep; a held major belongs to the owner.
    for red in [false, true] {
        let c = bot(true, true, true);
        if red {
            c.ci("failure");
        }
        let n = c.view(90000);
        assert!(matches!(n.step, Step::Wait { .. }));
        assert_eq!(n.owner_notice, None);
        assert_eq!(n.wait_on, Some(WaitOn::Sweep));
    }
    let mut c = bot(true, true, true);
    c.sweep("held");
    assert_eq!(c.view(700).step, Step::ListOwner);
}
#[test]
fn an_old_conflict_job_is_listed_even_when_queued_or_unknown() {
    // conflicts.py:155: ordinary and sweep-cleared edited major jobs still open a day are listed.
    for renovate in [false, true] {
        let mut c = if renovate {
            let mut c = bot(true, true, true);
            c.sweep("ok");
            c
        } else {
            let c = dirty();
            c.hub
                .edit_pull(REMI, 3, |p| p["created_at"] = json!("1970-01-01T00:00:00Z"));
            c.hub.refresh_flow(REMI, 3);
            c
        };
        for state in [JobState::Queued, JobState::Unknown] {
            c.jobs.insert(
                Kind::Conflict,
                Job {
                    id: "conflict".into(),
                    revision: c.scope(),
                    state,
                    started: 700,
                    repeated: false,
                },
            );
            let n = c.view(90000);
            assert!(matches!(n.step, Step::Wait { .. }));
            assert_eq!(n.owner_notice, Some(OwnerNotice::OpenOverDay));
        }
    }
}

#[test]
fn proof_only_checks_never_pretend_ci_will_run_the_fixs_tests() {
    // conflicts.py:180 / 194 / 210: has_ci excludes machine statuses; the fix kind still follows the outstanding findings.
    for state in [None, Some("pending"), Some("success"), Some("failure")] {
        for required in [false, true] {
            let mut c = Case::new();
            c.no_ci();
            c.settings.require_ci = required;
            c.settings
                .proofs
                .insert("proof".into(), Recipe::Job("run proof".into()));
            c.threads(1);
            if let Some(state) = state {
                c.hub
                    .put_statuses(&c.scope().head, json!([{"context":"proof","state":state}]));
            }
            let f = c.facts();
            assert!(!f.has_ci);
            assert_eq!(
                next(&f, &c.memory, 7200).0.step,
                Step::HandFix(Kind::Findings)
            );
        }
    }
    let c = Case::new();
    assert!(c.facts().has_ci);
}
#[test]
fn own_and_train_statuses_are_not_ci_but_an_ordinary_status_or_check_run_is() {
    // prs.py:has_ci, as used by conflicts.py:194: a proof-named check run is still a CI check run.
    let mut c = Case::new();
    c.no_ci();
    c.settings
        .proofs
        .insert("proof".into(), Recipe::Job("run proof".into()));
    for context in ["review-settled", "conary/train", "PROOF"] {
        c.hub.put_statuses(
            &c.scope().head,
            json!([{"context":context,"state":"success"}]),
        );
        assert!(!c.facts().has_ci);
    }
    c.hub.put_statuses(
        &c.scope().head,
        json!([{"context":"ordinary-ci","state":"pending"}]),
    );
    assert!(c.facts().has_ci);
    c.hub.put_statuses(&c.scope().head, json!([]));
    c.hub.ci(&c.scope().head, &[("proof", "success")]);
    assert!(c.facts().has_ci);
}
