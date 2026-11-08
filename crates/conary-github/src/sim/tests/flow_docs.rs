use super::flow_ci_case::*;
use super::{TRUNK, head};
use crate::flow::*;
use crate::sim::{Call, REMI};
#[test]
fn docs_lane_success_pending_and_failure_have_distinct_next_steps() {
    // docs.py:55,59,62: waive only proofs; unfinished CI still waits and failures get fixed.
    for (state, step) in [
        ("success", Step::Merge),
        ("failure", Step::HandFix(Kind::Red)),
    ] {
        let s = Scenario::new();
        s.hub.touch(&head(3), &["README.md", "docs/guide.md"]);
        let f = s.read(&[Run::docs(1, TRUNK, state)], &["proof"]);
        assert_eq!(next(&f, &s.memory, 4000).0.step, step);
        if state == "success" {
            assert_eq!(f.proof, Proof::NotNeeded);
        }
    }
    let s = Scenario::new();
    let mut r = Run::docs(1, TRUNK, "success").going();
    r.checks.last_mut().unwrap()["status"] = serde_json::json!("in_progress");
    r.checks.last_mut().unwrap()["conclusion"] = serde_json::Value::Null;
    let f = s.read(&[r], &["proof"]);
    assert_eq!(next(&f, &s.memory, 4000).0.stage, Stage::Ci);
}
#[test]
fn docs_waiver_rejects_gate_edits_renames_cut_files_and_another_app() {
    // docs.py:66,76,81,88: every failure of the trust predicate keeps the machine proof.
    for case in 0..7 {
        let s = Scenario::new();
        let mut r = Run::docs(1, TRUNK, "success");
        match case {
            0 => r = Run::whole(1, "main", "success"),
            1 => r.metadata.app = "some-app".into(),
            2 => {
                r = Run::new(
                    1,
                    &[(
                        "check / docs only into main, honest from invalid",
                        "success",
                    )],
                )
            }
            3 => s
                .hub
                .touch(&head(3), &["README.md", ".github/workflows/flake.yml"]),
            4 => s.hub.touch(&head(3), &[".github/CODEOWNERS"]),
            5 => s.hub.renamed_path(
                &head(3),
                ".github/pull_request_template.md",
                "docs/template.md",
            ),
            _ => {
                let names: Vec<_> = (0..301).map(|n| format!("docs/{n}.md")).collect();
                s.hub.touch(
                    &head(3),
                    &names.iter().map(String::as_str).collect::<Vec<_>>(),
                );
            }
        }
        let f = s.read(&[r], &["proof"]);
        assert_eq!(f.proof, Proof::Needed, "case {case}");
        assert_eq!(next(&f, &s.memory, 4000).0.step, Step::Prove, "case {case}");
    }
    let s = Scenario::new();
    s.hub.fail(Call::Files);
    // No read means no Facts and therefore no decision, rather than trusting an unread diff.
    assert!(
        read(
            &s.hub,
            &REMI.parse().unwrap(),
            3,
            &conary_core::Policy::parse(b"{}").unwrap(),
            Settings::default(),
            &["owner".into()]
        )
        .is_err()
    );
}
#[test]
fn docs_runs_for_another_fork_or_base_never_prove_this_pr() {
    // docs.py:93,103,125,150,156,161: live named base and merge fork bind the lane.
    for missing in [false, true] {
        let s = Scenario::new();
        if missing {
            s.hub.fail(Call::MergeBase);
        }
        let fork = if missing { TRUNK.to_owned() } else { head(99) };
        let f = s.read(&[Run::docs(1, &fork, "success")], &["proof"]);
        assert_eq!(next(&f, &s.memory, 4000).0.stage, Stage::Ci);
    }
    for verdict in ["success", "failure"] {
        let s = Scenario::new();
        s.hub.set_branch(REMI, "next", TRUNK);
        s.hub.retarget(REMI, 3, "next");
        s.hub.refresh_flow(REMI, 3);
        s.hub.flow_review(REMI, 3, "reviewer", Verdict::Clean, 600);
        let f = s.read(&[Run::docs(1, TRUNK, verdict)], &["proof"]);
        assert_eq!(next(&f, &s.memory, 4000).0.stage, Stage::Ci);
        assert!(f.checks.iter().all(|c| c.state != Ci::Red));
    }
}
