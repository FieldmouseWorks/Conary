//! CI scenarios read through the shared simulated GitHub fixture.
use super::flow_ci_case::*;
use super::{TRUNK, head};
use crate::flow::*;
use crate::sim::{Call, REMI};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[test]
fn cancelled_run_replaced_by_same_work_in_either_order() {
    // cancelled.py: "The newer run passed ... whichever order"; newer red.
    let s = Scenario::new();
    for conclusion in ["success", "failure"] {
        let old = Run::whole(1, "main", "cancelled");
        let new = Run::whole(2, "main", conclusion);
        let state = if conclusion == "failure" {
            Ci::Red
        } else {
            Ci::Green
        };
        let names: &[&str] = if state == Ci::Red { &[GATE] } else { &[] };
        s.assert(&[old.clone(), new.clone()], state, names);
        s.assert(&[new, old], state, names);
    }
    let lane = Run::new(
        1,
        &[("check / lane into main", "cancelled"), (GATE, "skipped")],
    );
    s.assert(&[lane, Run::whole(2, "main", "success")], Ci::Green, &[]);
}

#[test]
fn cancelled_run_waits_for_running_replacement() {
    // cancelled.py: "It's still going"; "Run after run, cancelled".
    let s = Scenario::new();
    let old = Run::whole(1, "main", "cancelled");
    let going = Run::whole(3, "main", "success").going();
    let f = s.read(&[old.clone(), going.clone()], &[]);
    assert_eq!(next(&f, &s.memory, 4000).0.stage, Stage::Ci);
    let twice = Run::whole(2, "main", "cancelled");
    s.assert(
        &[old.clone(), twice.clone(), Run::whole(3, "main", "success")],
        Ci::Green,
        &[],
    );
    let f = s.read(&[old, twice, going], &[]);
    assert_eq!(next(&f, &s.memory, 4000).0.stage, Stage::Ci);
}

#[test]
fn flake_twins_replace_but_namesake_workflows_do_not() {
    // cancelled.py: "flake.yml and flake-retarget.yml are one workflow".
    let s = Scenario::new();
    let old = Run::whole(1, "main", "cancelled");
    let twin = Run::whole(2, "main", "success").workflow("twin", Some(TWIN));
    s.assert(&[old.clone(), twin.clone()], Ci::Green, &[]);
    s.assert(&[twin, old.clone()], Ci::Green, &[]);
    for path in [Some(".github/workflows/ci.yml"), None] {
        let namesake = Run::whole(2, "main", "success").workflow("other", path);
        s.assert(
            &[old.clone(), namesake.clone()],
            Ci::Pending,
            &["check / check (cancelled, to be run again)"],
        );
        s.assert(
            &[namesake, old.clone()],
            Ci::Pending,
            &["check / check (cancelled, to be run again)"],
        );
    }
    let twin = Run::whole(2, "main", "success")
        .workflow("twin", Some(TWIN))
        .going();
    assert_eq!(
        next(&s.read(&[old, twin], &[]), &s.memory, 4000).0.stage,
        Stage::Ci
    );
}

#[test]
fn cancelled_timeout_is_red_and_annotations_are_read_lazily() {
    // cancelled.py: "One GitHub stopped at its job's timeout-minutes".
    let s = Scenario::new();
    let old = Run::whole(1, "main", "cancelled");
    let mut messages = vec!["warning"; 150];
    messages.push(
        "The job running on runner remi-3 has exceeded the maximum execution time of 90 minutes.",
    );
    s.hub.annotations(101, &messages);
    s.assert(
        std::slice::from_ref(&old),
        Ci::Red,
        &["check / check (timed out)"],
    );
    s.hub.fail(Call::Annotations);
    s.assert(
        &[old.clone(), Run::whole(2, "main", "success")],
        Ci::Green,
        &[],
    );
    // A failure to fetch the reason is an error, not a guessed verdict.
    s.hub.put_checks(&head(3), Value::Array(old.checks));
    s.hub
        .flow_suites(REMI, 3, BTreeMap::from([(1, old.metadata)]));
    assert!(
        read(
            &s.hub,
            &REMI.parse().unwrap(),
            3,
            &conary_core::Policy::parse(b"{}").unwrap(),
            Settings::default(),
            &["owner".into()]
        )
        .unwrap_err()
        .unavailable
    );
}

#[test]
fn shutdown_or_person_cancel_is_pending() {
    // cancelled.py: runner shutdown and manual cancel annotations.
    let s = Scenario::new();
    for message in [
        "The runner has received a shutdown signal.",
        "The run was canceled by @sam.",
    ] {
        s.hub.annotations(101, &[message]);
        s.assert(
            &[Run::whole(1, "main", "cancelled")],
            Ci::Pending,
            &["check / check (cancelled, to be run again)"],
        );
    }
}

#[test]
fn cancelled_runs_do_not_clear_each_other_or_unrelated_work() {
    // cancelled.py: "Nothing stands in for a newer run that ran through".
    let s = Scenario::new();
    let old = Run::whole(1, "main", "cancelled");
    for other in [
        None,
        Some(Run::whole(2, "main", "cancelled")),
        Some(
            Run::new(2, &[("report", "success")])
                .workflow("report", Some(".github/workflows/report.yml")),
        ),
    ] {
        let mut runs = vec![old.clone()];
        runs.extend(other);
        s.assert(
            &runs,
            Ci::Pending,
            &["check / check (cancelled, to be run again)"],
        );
    }
    let old = Run::new(1, &[("scan", "cancelled")]).no_workflow();
    let green = Run::new(2, &[("scan", "success")]).no_workflow();
    s.assert(
        &[old, green],
        Ci::Pending,
        &["scan (cancelled, to be run again)"],
    );
}

#[test]
fn skipped_and_unrun_jobs_do_not_replace_cancelled_work() {
    // cancelled.py: "A suite that completed without doing the check's work".
    let s = Scenario::new();
    let old = Run::whole(1, "main", "cancelled");
    for new in [
        Run::new(2, &[("check", "skipped")]),
        Run::new(
            2,
            &[("check / lane into main", "success"), (GATE, "skipped")],
        ),
        Run::new(2, &[("check / commits", "success")]),
        Run::docs(2, TRUNK, "skipped"),
        Run::new(2, &[(BLANK_LANE, "skipped"), (GATE, "success")]),
        Run::new(2, &[(GATE, "success")]),
        Run::new(
            2,
            &[("check / lane into main", "skipped"), (GATE, "success")],
        ),
    ] {
        s.assert(
            &[old.clone(), new.clone()],
            Ci::Pending,
            &["check / check (cancelled, to be run again)"],
        );
        s.assert(
            &[new, old.clone()],
            Ci::Pending,
            &["check / check (cancelled, to be run again)"],
        );
    }
}

#[test]
fn docs_and_whole_gate_replace_each_other_for_the_same_diff() {
    // cancelled.py: docs lane stands in for the gate, and whole gate for docs.
    let s = Scenario::new();
    for conclusion in ["success", "failure"] {
        let state = if conclusion == "failure" {
            Ci::Red
        } else {
            Ci::Green
        };
        let docs_name = format!("check / docs only into main from {TRUNK}");
        let names = if state == Ci::Red {
            vec![docs_name.as_str()]
        } else {
            vec![]
        };
        s.assert(
            &[
                Run::whole(1, "main", "cancelled"),
                Run::docs(2, TRUNK, conclusion),
            ],
            state,
            &names,
        );
        let names = if state == Ci::Red { vec![GATE] } else { vec![] };
        s.assert(
            &[
                Run::docs(1, TRUNK, "cancelled"),
                Run::whole(2, "main", conclusion),
            ],
            state,
            &names,
        );
    }
    let f = s.read(
        &[
            Run::whole(1, "main", "cancelled"),
            Run::docs(2, &"e".repeat(40), "success"),
        ],
        &[],
    );
    assert_eq!(next(&f, &s.memory, 4000).0.stage, Stage::Ci);
    let blank = Run::new(
        1,
        &[
            ("check / lane into main", "success"),
            (BLANK_DOCS, "cancelled"),
        ],
    );
    s.assert(&[blank, Run::docs(2, TRUNK, "success")], Ci::Green, &[]);
}

#[test]
fn ordinary_workflow_checks_require_their_own_work() {
    // cancelled.py: "Outside the shared CI ... by name, only a job that ran".
    let s = Scenario::new();
    let old = Run::new(1, &[("build", "cancelled")])
        .workflow("build", Some(".github/workflows/build.yml"));
    let new =
        Run::new(2, &[("build", "success")]).workflow("build", Some(".github/workflows/build.yml"));
    s.assert(&[old.clone(), new], Ci::Green, &[]);
    let skipped = Run::new(2, &[("build", "skipped"), ("lint", "success")])
        .workflow("build", Some(".github/workflows/build.yml"));
    s.assert(
        &[old, skipped],
        Ci::Pending,
        &["build (cancelled, to be run again)"],
    );
}

#[test]
fn docs_lane_replaces_only_its_own_gate() {
    // cancelled.py: "not a job it skipped beside itself ... sibling ... matrix".
    let s = Scenario::new();
    for name in [
        "check / scan",
        "check / commits",
        "other / check",
        "check (a) / check",
    ] {
        let old = Run::whole(1, "main", "cancelled")
            .add(name, "cancelled")
            .add("other / lane into main", "success")
            .add("check (a) / lane into main", "success");
        let new = Run::docs(2, TRUNK, "success")
            .add(name, "skipped")
            .add("other / lane into main", "success");
        s.assert(
            &[old, new],
            Ci::Pending,
            &[&format!("{name} (cancelled, to be run again)")],
        );
    }
    let old = Run::whole(1, "main", "cancelled").add("check / commits", "cancelled");
    let new = Run::docs(2, TRUNK, "success").add("check / commits", "success");
    s.assert(&[old, new], Ci::Green, &[]);
    let old = Run::new(
        1,
        &[
            ("other / check", "cancelled"),
            ("other / lane into main", "success"),
        ],
    );
    let new = Run::new(
        2,
        &[
            ("check / lane into main", "success"),
            ("other / check", "success"),
        ],
    );
    s.assert(
        &[old, new],
        Ci::Pending,
        &["other / check (cancelled, to be run again)"],
    );
}

#[test]
fn sibling_jobs_have_no_shared_lane_requirement() {
    // cancelled.py: "top-level or another reusable workflow's ... own run".
    let s = Scenario::new();
    for name in ["browser-tests", "e2e / browser-tests"] {
        let old = Run::whole(1, "main", "success").add(name, "cancelled");
        let green = Run::whole(2, "main", "success").add(name, "success");
        s.assert(&[old.clone(), green.clone()], Ci::Green, &[]);
        s.assert(&[green, old.clone()], Ci::Green, &[]);
        let skipped = Run::whole(2, "main", "success").add(name, "skipped");
        s.assert(
            &[old.clone(), skipped],
            Ci::Pending,
            &[&format!("{name} (cancelled, to be run again)")],
        );
        let red = Run::whole(2, "main", "success").add(name, "failure");
        s.assert(&[old.clone(), red], Ci::Red, &[name]);
        s.assert(
            &[old, Run::whole(2, "main", "success")],
            Ci::Pending,
            &[&format!("{name} (cancelled, to be run again)")],
        );
    }
}

#[test]
fn workflow_running_prevents_green_before_gate_starts() {
    // legs.py: "Every leg passed and the check job hasn't started".
    let s = Scenario::new();
    let legs = |suite: u64, done: bool, verdict: Option<&str>, leg: &str, n: u32, base: &str| {
        let mut r = Run::new(
            suite,
            &[
                (&format!("check / lane into {base}"), "success"),
                ("check / commits", "success"),
            ],
        );
        if !done {
            r = r.going();
        }
        for k in 1..=n {
            r = r.add(
                &format!("check / leg {k} of {n}"),
                if leg == "cancelled" || k == 3 {
                    leg
                } else {
                    "success"
                },
            );
        }
        if let Some(v) = verdict {
            r = r.add(GATE, v);
        }
        r
    };
    s.assert(
        &[legs(1, false, None, "success", 4, "main")],
        Ci::Pending,
        &["flake (still running)"],
    );
    s.assert(
        &[legs(1, true, Some("success"), "success", 4, "main")],
        Ci::Green,
        &[],
    );
    s.assert(
        &[legs(1, true, Some("failure"), "success", 4, "main")],
        Ci::Red,
        &[GATE],
    );
    s.assert(
        &[legs(1, false, None, "failure", 4, "main")],
        Ci::Red,
        &["check / leg 3 of 4"],
    );
    let app = Run::new(1, &[("scan", "success")]).going().no_workflow();
    s.assert(&[app], Ci::Green, &[]);
    // legs.py: cancelled legs are replaced by the gate on any leg count.
    let old = legs(1, true, Some("skipped"), "cancelled", 4, "main");
    for new in [
        legs(2, true, Some("success"), "success", 4, "main"),
        legs(2, true, Some("success"), "success", 3, "main"),
        Run::whole(2, "main", "success"),
        Run::docs(2, TRUNK, "success"),
    ] {
        s.assert(&[old.clone(), new.clone()], Ci::Green, &[]);
        s.assert(&[new, old.clone()], Ci::Green, &[]);
    }
    s.assert(
        &[old.clone(), Run::whole(2, "main", "failure")],
        Ci::Red,
        &[GATE],
    );
    // No matching work: pending, never red for stopped work.
    for new in [
        Run::new(
            2,
            &[
                ("check / lane into main", "success"),
                ("check / commits", "success"),
            ],
        ),
        legs(2, true, Some("success"), "success", 4, "next"),
        legs(2, true, Some("skipped"), "cancelled", 4, "main"),
        Run::new(
            2,
            &[
                ("check / lane into main", "success"),
                ("check / leg 1 of 4", "skipped"),
                ("check / leg 2 of 4", "skipped"),
                ("check / leg 3 of 4", "skipped"),
                ("check / leg 4 of 4", "skipped"),
            ],
        ),
    ] {
        let f = s.read(&[old.clone(), new], &[]);
        assert_eq!(next(&f, &s.memory, 4000).0.stage, Stage::Ci);
        assert!(
            f.checks
                .iter()
                .any(|c| c.name == "check / leg 1 of 4 (cancelled, to be run again)")
        );
    }
}

#[test]
fn stale_diff_cannot_clear_cancelled_work() {
    // cancelled.py: "A run that judged another base isn't the PR's CI".
    let s = Scenario::new();
    s.assert(
        &[
            Run::whole(1, "main", "cancelled"),
            Run::whole(2, "next", "success"),
        ],
        Ci::Pending,
        &["check / check (cancelled, to be run again)"],
    );
    s.assert(
        &[
            Run::whole(1, "next", "cancelled"),
            Run::whole(2, "main", "success"),
        ],
        Ci::Green,
        &[],
    );
}

#[test]
fn docs_lane_waives_only_proofs_for_a_complete_trusted_diff() {
    // docs.py: workflow edits, rename sources and incomplete paths never waive.
    let s = Scenario::new();
    let docs = Run::docs(1, TRUNK, "success");
    let f = s.read(std::slice::from_ref(&docs), &["agents"]);
    assert_eq!(f.proof, Proof::NotNeeded);
    assert_eq!(next(&f, &s.memory, 4000).0.step, Step::Merge);
    s.hub.touch(&head(3), &[".github/workflows/flake.yml"]);
    let f = s.read(&[docs], &["agents"]);
    assert_eq!(f.proof, Proof::Needed);
    assert!(
        f.checks
            .iter()
            .any(|c| c.name == "agents" && c.state == Ci::Pending)
    );
}

#[test]
fn startup_failure_is_red_even_without_a_job() {
    // handle.py / prs.py: a workflow GitHub could not start is not "no CI".
    let s = Scenario::new();
    let mut run = Run::new(1, &[]);
    run.metadata.conclusion = Some("startup_failure".into());
    s.assert(&[run], Ci::Red, &["flake (failed to start)"]);
}

#[test]
fn proofs_are_case_insensitive_and_not_run_again_after_success() {
    // prs.py / handle.py: proof contexts use GitHub's case-insensitive names.
    let s = Scenario::new();
    let runs = [Run::whole(1, "main", "success")];
    s.hub
        .post_status(&head(3), json!({"context":"AGENTS","state":"success"}));
    let f = s.read(&runs, &["agents"]);
    assert_eq!(f.proof, Proof::Passed);
    assert_eq!(next(&f, &s.memory, 4000).0.step, Step::Merge);
    s.hub
        .post_status(&head(3), json!({"context":"agents","state":"pending"}));
    let f = s.read(&runs, &["agents"]);
    assert_eq!(f.proof, Proof::Needed);
    assert_eq!(next(&f, &s.memory, 4000).0.step, Step::Prove);
    s.hub
        .post_status(&head(3), json!({"context":"Agents","state":"failure"}));
    let f = s.read(&runs, &["agents"]);
    assert_eq!(f.proof, Proof::Failed);
    assert_eq!(next(&f, &s.memory, 4000).0.step, Step::HandFix(Kind::Red));
}

#[test]
fn job_proofs_wait_for_ci_but_machine_proofs_start_beside_it() {
    // handle.py: "A check the steward builds itself ... while CI runs".
    let s = Scenario::new();
    let runs = [Run::whole(1, "main", "success").going()];
    let f = s.read(&runs, &["agents"]);
    let n = next(&f, &s.memory, 4000).0;
    assert_eq!(n.stage, Stage::Ci);
    assert!(n.description.contains("flake (still running)"));
    for kind in ["build", "run"] {
        let mut f = f.clone();
        if kind == "build" {
            f.settings
                .proofs
                .insert("agents".into(), Recipe::Build("checks.vm".into()));
        } else {
            f.settings
                .proofs
                .insert("agents".into(), Recipe::Run(vec!["just proof".into()]));
        }
        assert_eq!(next(&f, &s.memory, 4000).0.step, Step::Prove);
    }
}

#[test]
fn cancelled_twin_runs_do_not_clear_skipped_or_cancelled_work() {
    // cancelled.py:71-76, the flake twins still need actual completed work.
    let s = Scenario::new();
    let old = Run::whole(1, "main", "cancelled");
    let going = Run::whole(2, "main", "success")
        .workflow("twin", Some(TWIN))
        .going();
    assert_eq!(
        next(&s.read(&[old.clone(), going], &[]), &s.memory, 4000)
            .0
            .stage,
        Stage::Ci
    );
    let old_twin = old.clone().workflow("twin", Some(TWIN));
    let going = Run::whole(2, "main", "success").going();
    assert_eq!(
        next(&s.read(&[old_twin, going], &[]), &s.memory, 4000)
            .0
            .stage,
        Stage::Ci
    );
    let skipped = Run::new(2, &[(GATE, "skipped")]).workflow("twin", Some(TWIN));
    s.assert(
        &[old.clone(), skipped],
        Ci::Pending,
        &["check / check (cancelled, to be run again)"],
    );
    let cancelled = Run::whole(2, "main", "cancelled").workflow("twin", Some(TWIN));
    s.assert(
        &[old, cancelled],
        Ci::Pending,
        &["check / check (cancelled, to be run again)"],
    );
}

#[test]
fn push_or_unfilled_lane_does_not_vouch_for_a_pr() {
    // cancelled.py:146-162: push lanes, unfilled cancelled lanes and a real PR run beside them.
    let s = Scenario::new();
    let old = Run::whole(1, "main", "cancelled");
    let push = Run::new(2, &[(BLANK_LANE, "skipped"), (GATE, "success")]);
    let nameless = Run::new(1, &[(BLANK_LANE, "cancelled"), (GATE, "cancelled")]);
    s.assert(
        &[nameless.clone(), push.clone()],
        Ci::Pending,
        &[
            &format!("{BLANK_LANE} (cancelled, to be run again)"),
            "check / check (cancelled, to be run again)",
        ],
    );
    s.assert(
        &[nameless, Run::whole(2, "main", "success")],
        Ci::Green,
        &[],
    );
    for conclusion in ["success", "failure"] {
        let state = if conclusion == "success" {
            Ci::Green
        } else {
            Ci::Red
        };
        let names: &[&str] = if state == Ci::Red { &[GATE] } else { &[] };
        s.assert(
            &[old.clone(), push.clone(), Run::whole(3, "main", conclusion)],
            state,
            names,
        );
    }
    let old = old.add("browser-tests", "success");
    s.assert(
        &[old, push.add("browser-tests", "success")],
        Ci::Pending,
        &["check / check (cancelled, to be run again)"],
    );
}

#[test]
fn cancelled_legs_require_their_own_gate_work() {
    // legs.py:51-73, a sibling call cannot stand in for the cancelled legs.
    let s = Scenario::new();
    let mut old = Run::new(
        1,
        &[
            ("check / lane into main", "success"),
            ("check / commits", "success"),
            (GATE, "skipped"),
        ],
    );
    for k in 1..=4 {
        old = old.add(&format!("check / leg {k} of 4"), "cancelled");
    }
    let mut sibling = Run::new(2, &[("other / lane into main", "success")]);
    for k in 1..=4 {
        sibling = sibling.add(&format!("other / leg {k} of 4"), "success");
    }
    s.assert(
        &[old.clone(), sibling],
        Ci::Pending,
        &[
            "check / leg 1 of 4 (cancelled, to be run again)",
            "check / leg 2 of 4 (cancelled, to be run again)",
            "check / leg 3 of 4 (cancelled, to be run again)",
            "check / leg 4 of 4 (cancelled, to be run again)",
        ],
    );
    let mut red = Run::whole(2, "main", "failure");
    for k in 1..=3 {
        red = red.add(&format!("check / leg {k} of 3"), "success");
    }
    s.assert(&[old, red], Ci::Red, &[GATE]);
}

#[test]
fn another_apps_workflow_cannot_clear_a_cancellation() {
    // cancelled.py: no unrelated App's success stands in for stopped work.
    let s = Scenario::new();
    let old = Run::whole(1, "main", "cancelled");
    let mut other = Run::whole(2, "main", "success");
    other.metadata.app = "other-app".into();
    s.assert(
        &[old, other],
        Ci::Pending,
        &["check / check (cancelled, to be run again)"],
    );
}

#[test]
fn reader_rejects_a_retarget_or_redescription_between_endpoints() {
    // handle.py / misdescribed.py: reviews and judgments bind all their inputs.
    // Same-head edits must not combine an old projection with a new REST pull.
    let edits: [fn(&mut Value); 3] = [
        |p| p["base"]["ref"] = json!("release"),
        |p| p["body"] = json!("new description"),
        |p| p["title"] = json!("new intent"),
    ];
    for edit in edits {
        let s = Scenario::new();
        s.hub.edit_pull(REMI, 3, edit);
        let result = read(
            &s.hub,
            &REMI.parse().unwrap(),
            3,
            &conary_core::Policy::parse(b"{}").unwrap(),
            Settings::default(),
            &["owner".into()],
        );
        assert!(result.unwrap_err().to_string().contains("moved"));
    }
}

#[test]
fn train_enabled_release_branch_lands_outside_the_train() {
    // trains.py: train rides only PRs targeting the repo's default branch.
    // Into a release branch, the ordinary CI and merge rules still apply.
    let s = Scenario::new();
    s.hub.set_branch(REMI, "release", TRUNK);
    s.hub
        .edit_pull(REMI, 3, |p| p["base"]["ref"] = json!("release"));
    let revision = Revision::from(&crate::api::Source::pull(&s.hub, REMI, 3).unwrap());
    s.hub.flow_snapshot(
        REMI,
        3,
        Snapshot {
            default_branch: None,
            revision,
            draft: false,
            complete: true,
            merge_state: "CLEAN".into(),
            open_threads: 0,
            threads: Default::default(),
            resolved_reviews: Default::default(),
            updated_at: 0,
            suites: BTreeMap::new(),
            checks_complete: true,
            rebase_removed: Vec::new(),
            timeline_cut_at: None,
        },
    );
    s.hub.flow_review(REMI, 3, "reviewer", Verdict::Clean, 0);
    let mut f = s.read(&[Run::whole(1, "release", "success")], &[]);
    f.settings.train = true;
    assert_eq!(f.default_branch, "main");
    assert_eq!(next(&f, &s.memory, 4000).0.step, Step::Merge);
    // Missing CI is not waived merely because the repository has a train.
    let mut f = s.read(&[], &[]);
    f.settings.train = true;
    let (n, m) = next(&f, &s.memory, 4000);
    assert_eq!(n.stage, Stage::Ci);
    assert_eq!(next(&f, &m, 4300).0.step, Step::Judge(vec![Trigger::NoCi]));
}

#[test]
fn build_or_run_configuration_alone_still_requires_its_proof() {
    // common.machine / handle.py: machine checks are the union of all kinds.
    let s = Scenario::new();
    for build in [true, false] {
        let mut settings = Settings::default();
        if build {
            settings
                .proofs
                .insert("vm".into(), Recipe::Build("checks.vm".into()));
        } else {
            settings
                .proofs
                .insert("vm".into(), Recipe::Run(vec!["just proof".into()]));
        }
        let f = s.read_settings(&[Run::whole(1, "main", "success")], settings.clone());
        assert_eq!(f.proof, Proof::Needed);
        assert!(
            f.checks
                .iter()
                .any(|c| c.name == "vm" && c.state == Ci::Pending)
        );
        assert_eq!(next(&f, &s.memory, 4000).0.step, Step::Prove);
        let f = s.read_settings(&[Run::whole(1, "main", "success").going()], settings);
        assert_eq!(next(&f, &s.memory, 4000).0.step, Step::Prove);
    }
}
