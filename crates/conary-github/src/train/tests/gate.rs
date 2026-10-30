//! The gate: what its checks' runs say about a candidate, and the
//! attestations a required check's pass counts only with, its job's own, on
//! the candidate's commit and tree, signed by burrow's shared CI at a commit
//! on burrow's main.

use super::*;
use conary_core::op::Attest;

#[test]
fn the_gates_results_are_recorded_for_the_candidates_commit_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let commit = store.ledger().candidate(id).unwrap().commit.to_string();
    hub.checks.insert(
        commit.clone(),
        json!([
            {"id": 2, "name": "check / check", "status": "completed", "conclusion": "success",
             "html_url": "https://github.com/FieldmouseWorks/remi/actions/runs/1/job/1"},
            {"id": 3, "name": "slow", "status": "in_progress", "conclusion": null, "html_url": null},
        ]),
    );
    hub.statuses.insert(
        commit.clone(),
        json!([
            {"context": "review-settled", "state": "success", "target_url": null},
            {"context": "proof", "state": "failure", "target_url": "https://remi/proof/1"},
        ]),
    );
    // Something else pushed to the train branch counts for nothing.
    let pushed = format!("{:0>40}", "dead");
    hub.checks.insert(
        pushed.clone(),
        json!([{"id": 4, "name": "check / check", "status": "completed", "conclusion": "failure", "html_url": null}]),
    );
    hub.branches.borrow_mut().insert(train_branch(id), pushed);
    step_once(&mut store, &hub);
    step_once(&mut store, &hub);
    let evidence: Vec<(String, conary_core::op::Outcome)> = store
        .ledger()
        .candidate(id)
        .unwrap()
        .evidence
        .iter()
        .map(|e| (e.check.clone(), e.result))
        .collect();
    use conary_core::op::Outcome::{Fail, Pass};
    assert_eq!(
        evidence,
        [("check / check".into(), Pass), ("proof".into(), Fail)]
    );
}

#[test]
fn the_gate_is_green_only_once_every_check_passed_and_the_required_ones_reported_attested() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let required = Settings {
        require: vec!["check / check".into(), "proof".into()],
        ..settings()
    };
    let step_req = |store: &mut Store, hub: &Hub| {
        step(store, hub, hub, &config(), &repo(), &required).unwrap()
    };
    // No check at all is no pass, and names nothing to wait on but what's
    // required.
    let stepped = step_req(&mut store, &hub);
    let (id, _) = gating(&stepped);
    assert_eq!(waiting(&stepped), ["check / check", "proof"]);
    let commit = commit_of(&store, id);
    hub.checks.insert(
        commit.clone(),
        json!([
            {"id": 5, "name": "check / check", "status": "completed", "conclusion": "success",
             "html_url": null, "started_at": "2026-10-29T11:00:00Z"},
            {"id": 6, "name": "slow", "status": "in_progress", "conclusion": null,
             "html_url": null, "started_at": "2026-10-29T11:00:00Z"},
        ]),
    );
    hub.attest_passes(&commit);
    assert_eq!(waiting(&step_req(&mut store, &hub)), ["slow", "proof"]);
    hub.ci(
        &commit,
        &[("check / check", "success"), ("slow", "skipped")],
    );
    // The proof job hasn't posted, and the steward's own word isn't a check.
    hub.statuses.insert(
        commit.clone(),
        json!([{"context": "review-settled", "state": "success", "target_url": null},
               {"context": CONTEXT, "state": "pending", "target_url": null}]),
    );
    assert_eq!(waiting(&step_req(&mut store, &hub)), ["proof"]);
    hub.statuses.insert(
        commit.clone(),
        json!([{"context": "proof", "state": "pending", "target_url": null}]),
    );
    assert_eq!(waiting(&step_req(&mut store, &hub)), ["proof"]);
    // A status that passed still isn't attested: no status carries an
    // attestation, so a required one can't make a candidate green.
    hub.statuses.insert(
        commit.clone(),
        json!([{"context": "proof", "state": "success", "target_url": null},
               {"context": "proof", "state": "pending", "target_url": null}]),
    );
    assert_eq!(
        waiting(&step_req(&mut store, &hub)),
        ["proof: a status carries no attestation"]
    );
    // A required job that passed, attested by its producer, is.
    hub.statuses.remove(&commit);
    hub.ci(
        &commit,
        &[("check / check", "success"), ("proof", "success")],
    );
    let stepped = step_req(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
    assert_eq!(stepped.landed, [3]);
}

#[test]
fn each_checks_latest_run_is_its_word() {
    // The verdict on a candidate whose commit has these runs and statuses,
    // or none while it waits.
    let gate = |runs: Vec<Value>, statuses: Value| {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let mut hub = Hub::new(&[3]);
        join_all(&mut store, &hub, &[3]);
        let (id, _) = gating(&step_once(&mut store, &hub));
        let commit = commit_of(&store, id);
        hub.checks.insert(commit.clone(), Value::Array(runs));
        hub.attest_passes(&commit);
        hub.statuses.insert(commit, statuses);
        step_once(&mut store, &hub).verdicts.first().map(|v| v.1)
    };
    // A run of the gate in a suite: done with `conclusion`, else queued.
    let run = |id: u64, suite: u64, conclusion: Option<&str>, started: Option<&str>| {
        json!({"id": id, "name": "check / check", "check_suite": {"id": suite}, "html_url": null,
               "status": if conclusion.is_some() { "completed" } else { "queued" },
               "conclusion": conclusion, "started_at": started})
    };
    let at = Some("2026-10-29T11:00:00Z");

    // A rerun's runs come in no order the train can count on: the later
    // run is the check's word.
    for (first, then, verdict) in [
        ("failure", "success", Verdict::Green),
        ("success", "failure", Verdict::Red),
    ] {
        let mut runs = vec![run(7, 1, Some(first), at), run(8, 1, Some(then), at)];
        assert_eq!(gate(runs.clone(), json!([])), Some(verdict));
        runs.reverse();
        assert_eq!(gate(runs, json!([])), Some(verdict));
    }
    // Whether it started says nothing about which is later: an earlier
    // skipped run that never started doesn't hide the rerun that failed,
    // and a rerun still queued keeps the gate waiting.
    let skipped_then_failed = vec![
        run(7, 1, Some("skipped"), None),
        run(8, 1, Some("failure"), at),
    ];
    assert_eq!(gate(skipped_then_failed, json!([])), Some(Verdict::Red));
    let failed_then_queued = vec![run(7, 1, Some("failure"), at), run(8, 1, None, None)];
    assert_eq!(gate(failed_then_queued, json!([])), None);
    // Runs of one name in two suites (two workflows, or two Apps) are two
    // checks, and a status and a run of one name are too.
    for (a, b) in [("success", "failure"), ("failure", "success")] {
        let runs = vec![run(7, 1, Some(a), at), run(8, 2, Some(b), at)];
        assert_eq!(gate(runs, json!([])), Some(Verdict::Red));
    }
    let failed = json!([{"context": "check / check", "state": "failure", "target_url": null}]);
    assert_eq!(
        gate(vec![run(7, 1, Some("success"), at)], failed),
        Some(Verdict::Red)
    );
}

/// A candidate of #3 gating, with its gate's one run passed but not yet
/// attested: the run's id.
fn gating_unattested(store: &mut Store, hub: &mut Hub) -> (CandidateId, String, u64) {
    join_all(store, hub, &[3]);
    let (id, _) = gating(&step_once(store, hub));
    let commit = commit_of(store, id);
    hub.checks.insert(
        commit.clone(),
        json!([{"id": 9, "name": "check / check", "status": "completed", "conclusion": "success",
                "html_url": "https://github.com/FieldmouseWorks/remi/actions/runs/90/job/9",
                "started_at": "2026-10-29T11:00:00Z"}]),
    );
    (id, commit, 9)
}

#[test]
fn a_required_pass_waits_for_its_attestation_and_then_lands_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    let (id, commit, run) = gating_unattested(&mut store, &mut hub);
    // Its pass alone is no green: the gate waits, and says why.
    let stepped = step_once(&mut store, &hub);
    assert_eq!(gating(&stepped).0, id);
    assert_eq!(
        waiting(&stepped),
        ["check / check: its run 90 kept no attestation"]
    );
    assert!(stepped.verdicts.is_empty());
    // Once the job's attestation is there, it lands on it, and the
    // evidence keeps it as it was checked.
    let file = hub.attestation(&commit, run);
    hub.attest(run, &file);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
    assert_eq!(stepped.landed, [3]);
    let evidence = &store.ledger().candidate(id).unwrap().evidence;
    let (statement, token) = file.trim_end().rsplit_once('\n').unwrap();
    assert_eq!(
        evidence.last().unwrap().attest,
        Some(Attest::GithubToken {
            statement: format!("{statement}\n"),
            token: token.into(),
        })
    );
    // The unattested pass is in it too, as it was seen first.
    assert_eq!(
        evidence
            .iter()
            .map(|e| e.attest.is_some())
            .collect::<Vec<_>>(),
        [false, true]
    );
}

#[test]
fn an_attestation_already_checked_isnt_read_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    let required = Settings {
        require: vec!["check / check".into(), "proof".into()],
        ..settings()
    };
    let (id, commit, run) = gating_unattested(&mut store, &mut hub);
    hub.attest(run, &hub.attestation(&commit, run));
    let step_req = |store: &mut Store, hub: &Hub| {
        step(store, hub, hub, &config(), &repo(), &required).unwrap()
    };
    // Waiting on the proof job, step after step.
    for _ in 0..3 {
        assert_eq!(waiting(&step_req(&mut store, &hub)), ["proof"]);
    }
    assert_eq!(hub.downloads.get(), 1);
    let attested = |store: &Store| {
        store
            .ledger()
            .candidate(id)
            .unwrap()
            .evidence
            .iter()
            .filter(|e| e.attest.is_some())
            .count()
    };
    assert_eq!(attested(&store), 1);
    // The proof job passes: the gate job's attestation isn't its own.
    let proof = json!({"id": 19, "name": "proof", "status": "completed", "conclusion": "success",
                       "html_url": "https://github.com/FieldmouseWorks/remi/actions/runs/190/job/19",
                       "started_at": "2026-10-29T11:05:00Z"});
    hub.checks
        .get_mut(&commit)
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(proof);
    assert_eq!(
        waiting(&step_req(&mut store, &hub)),
        ["proof: its run 190 kept no attestation"]
    );
    hub.attest(19, &hub.attestation(&commit, 19));
    assert_eq!(step_req(&mut store, &hub).verdicts, [(id, Verdict::Green)]);
    assert_eq!(hub.downloads.get(), 2);
    assert_eq!(attested(&store), 2);
}

#[test]
fn an_attestation_of_another_commit_run_or_tree_doesnt_count() {
    // Each attestation, kept in the gate run's artifacts, as the only one.
    let waits = |forged: &dyn Fn(&Hub, &str, u64) -> String| {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let mut hub = Hub::new(&[3]);
        let (_, commit, run) = gating_unattested(&mut store, &mut hub);
        let file = forged(&hub, &commit, run);
        hub.attest(run, &file);
        let stepped = step_once(&mut store, &hub);
        assert!(stepped.verdicts.is_empty());
        waiting(&stepped)
    };
    let other = "d".repeat(40);
    // Another commit's honest attestation, replayed.
    let why = waits(&|hub, _, run| {
        hub.holds.borrow_mut().insert(other.clone(), Vec::new());
        hub.attestation(&other, run)
    });
    assert!(why[0].contains("its token is for"), "{why:?}");
    // Another run's, on this commit.
    let why = waits(&|hub, commit, _| hub.attestation(commit, 8));
    assert!(why[0].contains("check run Some(8)'s"), "{why:?}");
    // On another tree.
    let why = waits(&|_, commit, run| {
        let s = testing::statement(commit, &"e".repeat(40), "pass");
        let c = testing::claims("FieldmouseWorks/remi", commit, run, &s);
        testing::file(&s, &testing::token(&c))
    });
    assert!(why[0].contains("it ran on tree"), "{why:?}");
    // Another repo's.
    let why = waits(&|hub, commit, run| {
        let tree = tree_of(&hub.holds.borrow()[commit]);
        let s = testing::statement(commit, &tree, "pass");
        let c = testing::claims("FieldmouseWorks/burrow", commit, run, &s);
        testing::file(&s, &testing::token(&c))
    });
    assert!(
        why[0].contains("its token is FieldmouseWorks/burrow's"),
        "{why:?}"
    );
}

#[test]
fn only_burrows_ci_at_a_commit_on_its_main_signs() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    let (id, commit, run) = gating_unattested(&mut store, &mut hub);
    hub.attest(run, &hub.attestation(&commit, run));
    // burrow's main hasn't got the signer's commit: a branch's copy of the
    // workflow could sign anything.
    hub.signer.replace(vec!["e".repeat(40)]);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.verdicts.is_empty());
    assert_eq!(
        waiting(&stepped),
        [format!(
            "check / check: {ARTIFACT}0: signed by .github/workflows/flake-check.yml at {}, which isn't on FieldmouseWorks/burrow's default branch",
            testing::SIGNER_SHA
        )]
    );
    // Once main has it, behind its tip, it counts.
    hub.signer
        .replace(vec!["e".repeat(40), testing::SIGNER_SHA.into()]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
}

#[test]
fn a_pass_counts_only_from_a_runner_started_fresh_for_it() {
    // The gate's job on `runner`, attested with its token's claims changed
    // by `claims`, under `fresh`: whether it lands, or why not.
    let lands = |runner: &str, claims: fn(&mut Value), fresh: Option<&str>| {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let mut hub = Hub::new(&[3]);
        let (id, commit, run) = gating_unattested(&mut store, &mut hub);
        hub.runners.borrow_mut().insert(run, runner.into());
        let tree = tree_of(&hub.holds.borrow()[&commit]);
        let s = testing::statement(&commit, &tree, "pass");
        let mut c = testing::claims("FieldmouseWorks/remi", &commit, run, &s);
        claims(&mut c);
        hub.attest(run, &testing::file(&s, &testing::token(&c)));
        let settings = Settings {
            fresh: fresh.map(Into::into),
            ..settings()
        };
        let stepped = step(&mut store, &hub, &hub, &config(), &repo(), &settings).unwrap();
        if stepped.verdicts == [(id, Verdict::Green)] {
            Ok(())
        } else {
            Err(waiting(&stepped))
        }
    };
    let as_is = |_: &mut Value| {};
    let hosted = |c: &mut Value| c["runner_environment"] = json!("github-hosted");
    lands("fresh-remi-9", as_is, Some(FRESH)).unwrap();
    // A runner that takes job after job: one before could have signed.
    let why = lands("remi-actions-runner", as_is, Some(FRESH)).unwrap_err();
    assert_eq!(
        why,
        [format!(
            "check / check: {ARTIFACT}0: it ran on remi-actions-runner, which wasn't started fresh for it"
        )]
    );
    // A name only starts like it, or no prefix at all.
    lands("remi-fresh-remi-9", as_is, Some(FRESH)).unwrap_err();
    lands("fresh-remi-9", as_is, None).unwrap_err();
    lands("fresh-remi-9", as_is, Some("")).unwrap_err();
    // GitHub's own runners are a VM for each job, whatever their names.
    lands("GitHub Actions 3", hosted, Some(FRESH)).unwrap();
    lands("GitHub Actions 3", hosted, None).unwrap();
}

#[test]
fn a_required_job_that_was_skipped_attests_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let commit = commit_of(&store, id);
    hub.ci(&commit, &[("check / check", "skipped")]);
    assert_eq!(
        waiting(&step_once(&mut store, &hub)),
        ["check / check: it concluded skipped, which attests nothing"]
    );
}

#[test]
fn red_needs_no_attestation_and_a_check_not_required_needs_none_to_pass() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    // A check failed: red, with nothing attested, and the required one's
    // attestation not even read.
    hub.ci(
        &commit_of(&store, id),
        &[("check / check", "success"), ("lint", "failure")],
    );
    assert_eq!(step_once(&mut store, &hub).verdicts, [(id, Verdict::Red)]);
    assert_eq!(hub.downloads.get(), 0);
    // A pass of a check that isn't required counts unattested beside the
    // required one's.
    join_all(&mut store, &hub, &[4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let commit = commit_of(&store, id);
    hub.ci(
        &commit,
        &[("check / check", "success"), ("lint", "success")],
    );
    // Only the required one's job attested it.
    let lint = parse::<Vec<CheckRun>>(&hub.checks[&commit])
        .into_iter()
        .find(|r| r.name == "lint")
        .unwrap();
    hub.attestations.borrow_mut().remove(&lint.id).unwrap();
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
}

#[test]
fn a_train_with_nothing_required_doesnt_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let none = Settings {
        require: Vec::new(),
        ..settings()
    };
    assert!(matches!(
        step(&mut store, &hub, &hub, &config(), &repo(), &none),
        Err(TrainError::NothingRequired)
    ));
    // Nothing was built.
    assert_eq!(hub.names(), ["main"]);
}
