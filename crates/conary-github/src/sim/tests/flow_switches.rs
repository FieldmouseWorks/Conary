//! Every switch in effect shows, with what it holds: each PR's decision
//! names the switches that stop it, each with the step it keeps off and the
//! workers it's stopping, whichever of them decided the PR's step. The
//! assertions read the decisions as `cn flow pass` prints them.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::api::Source;
use crate::flow::jobs;
use crate::flow::*;
use serde_json::{Value, json};

fn repo() -> conary_core::op::Repo {
    REPO.parse().unwrap()
}
/// PR 3 and PR 5, both trusted and green, both waiting on their review.
fn two() -> Case {
    let mut c = Case::new();
    c.hub.pr(REPO, 5, &head(5));
    c.hub.edit_pull(REPO, 5, |p| {
        p["user"]["login"] = json!("owner");
        p["created_at"] = json!("2099-01-01T00:00:00Z");
    });
    let mut snapshot = c.hub.snapshot(REPO, 3).unwrap();
    snapshot.revision = Revision::from(&c.hub.pull(REPO, 5).unwrap());
    c.hub.flow_snapshot(REPO, 5, snapshot);
    c.good_ci(&head(5));
    c.config.plugins.review = None;
    c
}
fn printed(reports: &[Report]) -> Vec<Value> {
    reports
        .iter()
        .map(|r| serde_json::to_value(r).unwrap())
        .collect()
}
fn review() -> Value {
    json!({"QueueReview": "reviewer"})
}

/// A paused repository with a held PR and no review plugin: each PR's
/// decision names every switch that stops it, though the pause alone
/// decided its step. Lifting the pause and the hold leaves the plugin.
#[test]
fn a_pause_a_hold_and_an_omitted_plugin_each_name_what_they_stop() {
    let c = two();
    let now = jobs::now();
    // Without a pause or a hold, the omitted plugin is what stops both.
    for report in printed(&c.pass(now, false)) {
        assert_eq!(report["description"], "No plugin configured for review");
        assert_eq!(
            report["switches"],
            json!([{"switch": "no_plugin", "kind": "review", "step": review(), "workers": []}]),
            "{report}"
        );
    }

    c.hub.label(REPO, 3, &["hold"]);
    let paused = Pause {
        scope: Scope::Repository,
        reason: "moving the machine".into(),
        at: 7,
    };
    crate::flow::pause::pause(c.dir.path(), &repo(), &paused).unwrap();
    let reports = printed(&c.pass(now + 1, false));
    let pause = json!({"switch": "pause", "reason": "moving the machine", "at": 7,
        "step": review(), "workers": []});
    let hold = json!({"switch": "hold", "label": "hold", "step": review(), "workers": []});
    let plugin = json!({"switch": "no_plugin", "kind": "review", "step": review(), "workers": []});
    assert_eq!(reports[0]["pr"], 3);
    assert_eq!(reports[0]["description"], "Paused: moving the machine");
    assert_eq!(
        reports[0]["switches"],
        json!([pause, hold, plugin]),
        "{}",
        reports[0]
    );
    assert_eq!(reports[1]["pr"], 5);
    assert_eq!(
        reports[1]["switches"],
        json!([pause, plugin]),
        "{}",
        reports[1]
    );

    crate::flow::pause::resume(c.dir.path(), &repo()).unwrap();
    c.hub.label(REPO, 3, &[]);
    for report in printed(&c.pass(now + 2, false)) {
        assert_eq!(report["switches"], json!([plugin]), "{report}");
    }
}

fn read_status(c: &Case) -> Value {
    let status = crate::flow::status(c.dir.path(), &repo(), &c.config, jobs::now()).unwrap();
    serde_json::to_value(status).unwrap()
}
/// The switch named, from a status.
fn named<'a>(status: &'a Value, switch: &str, extra: (&str, &str)) -> &'a Value {
    status["switches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["switch"] == switch && s[extra.0] == extra.1)
        .unwrap_or_else(|| panic!("no {switch} in {status}"))
}
fn prs(entry: &Value) -> Vec<u64> {
    entry["prs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pr"].as_u64().unwrap())
        .collect()
}

/// The repository's status names every switch in effect and the PRs each
/// holds, from the data directory and the configuration alone. A pause
/// written since the last pass already holds every open PR, and a PR that
/// closed drops out.
#[test]
fn status_lists_each_switch_with_the_prs_it_holds() {
    let c = two();
    let now = jobs::now();
    c.pass(now, false);
    let paused = Pause {
        scope: Scope::Repository,
        reason: "moving the machine".into(),
        at: 7,
    };
    crate::flow::pause::pause(c.dir.path(), &repo(), &paused).unwrap();
    // No pass since the pause.
    let status = read_status(&c);
    assert_eq!(status["read_at"], now);
    let pause = named(&status, "pause", ("reason", "moving the machine"));
    assert_eq!(pause["at"], 7);
    assert_eq!(prs(pause), [3, 5]);
    assert_eq!(pause["prs"][0]["step"], review());
    let switches: Vec<_> = status["switches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["switch"].as_str().unwrap(), s["kind"].as_str()))
        .collect();
    // The case's configuration has a review author list, people and an
    // evidence verifier, and no review, judge or proof plugin.
    assert_eq!(
        switches,
        [
            ("pause", None),
            ("no_plugin", Some("review")),
            ("no_plugin", Some("judge")),
            ("no_plugin", Some("proof")),
            ("train_off", None),
        ]
    );
    assert_eq!(prs(named(&status, "no_plugin", ("kind", "review"))), [3, 5]);
    assert_eq!(prs(named(&status, "no_plugin", ("kind", "judge"))), [0; 0]);

    c.hub.label(REPO, 3, &["hold"]);
    c.pass(now + 1, false);
    let status = read_status(&c);
    assert_eq!(
        prs(named(&status, "pause", ("reason", "moving the machine"))),
        [3, 5]
    );
    let hold = named(&status, "hold", ("label", "hold"));
    assert_eq!(prs(hold), [3]);
    assert_eq!(hold["prs"][0]["step"], review());
    assert_eq!(prs(named(&status, "no_plugin", ("kind", "review"))), [3, 5]);

    c.hub.close(REPO, 5);
    c.pass(now + 2, false);
    let status = read_status(&c);
    assert_eq!(
        prs(named(&status, "pause", ("reason", "moving the machine"))),
        [3]
    );
    assert_eq!(prs(named(&status, "no_plugin", ("kind", "review"))), [3]);
}

/// A pause holds its PRs' running workers: the decision that stops them
/// and the status read before any pass both name them.
#[test]
fn a_pause_names_the_workers_it_stops() {
    let c = super::flow_pass_slots::two(1);
    let now = jobs::now();
    super::flow_pass_slots::prepared(&c, now, true);
    c.pass(now + 1, false);
    let running =
        super::flow_pass_slots::work(&c, 3, |p| matches!(p, jobs::Purpose::Proof)).remove(0);
    let (deadline, running) = (running.deadline, running.job.id);
    let paused = Pause {
        scope: Scope::Repository,
        reason: "moving the machine".into(),
        at: 7,
    };
    crate::flow::pause::pause(c.dir.path(), &repo(), &paused).unwrap();
    let pause = read_status(&c)["switches"][0].clone();
    assert_eq!(pause["switch"], "pause");
    assert_eq!(pause["prs"][0]["pr"], 3);
    assert_eq!(pause["prs"][0]["workers"], json!([running]));
    assert_eq!(pause["prs"][1]["workers"], json!([]));

    let reports = printed(&c.pass(now + 2, false));
    assert_eq!(reports[0]["step"], json!({"StopJob": running}));
    let switches = &reports[0]["switches"];
    assert_eq!(switches[0]["switch"], "pause", "{}", reports[0]);
    assert_eq!(switches[0]["workers"], json!([running]));
    // Without it, the PR would wait on its running proof: stopping it is
    // the pause's own step, never the one it keeps off.
    let wake = switches[0]["step"]["Wait"]["until"].as_u64();
    assert!(wake.is_some_and(|at| at >= deadline), "{}", reports[0]);
}

/// The landing switches name a PR only at the step they keep off: no
/// evidence verifier holds a merge, and with an R2 change, the train off
/// and no person list hold it too. An owner's decision with no person list
/// is held the same way.
#[test]
fn landing_switches_hold_the_step_they_keep_off() {
    let mut c = Case::new();
    c.clean_review();
    c.config.evidence = None;
    let reports = printed(&c.pass(jobs::now(), false));
    assert_eq!(
        reports[0]["description"],
        "producer attestation verifier is not configured"
    );
    let evidence = json!([{"switch": "no_evidence", "step": "Merge", "workers": []}]);
    assert_eq!(reports[0]["switches"], evidence);
    // The saved decision, a switch with no fields of its own, loads again.
    let reports = printed(&c.pass(jobs::now() + 1, false));
    assert_eq!(reports[0]["error"], Value::Null, "{}", reports[0]);
    assert_eq!(reports[0]["switches"], evidence);
    assert_eq!(
        c.record().decision.unwrap().switches[0].switch,
        Switch::NoEvidence
    );

    let r2 = super::flow_case::Case::new();
    r2.secret();
    let f = r2.facts();
    assert_eq!(f.classified.class, conary_core::Class::R2);
    let mut config = Config {
        settings: r2.settings.clone(),
        ..Default::default()
    };
    let pass = |config: &Config, f: &Facts, stage: Stage, step: Step| -> Vec<Value> {
        let launcher = super::flow_pass_case::Deferred(Default::default());
        let pass = Pass {
            config,
            policy: &c.policy,
            root: c.dir.path(),
            launcher: &launcher,
            now: 4000,
            dry_run: true,
            ledger: None,
        };
        // A pass reads the configuration's settings into each PR's facts.
        let mut f = f.clone();
        f.settings = config.settings.clone();
        let f = &f;
        let mut decided = next(f, &r2.memory, 4000).0;
        decided.stage = stage;
        decided.step = step;
        crate::flow::switches::holding(&pass, f, &r2.memory, &decided, 4000)
            .iter()
            .map(|h| serde_json::to_value(h).unwrap()["switch"].clone())
            .collect()
    };
    config.settings.people.clear();
    assert_eq!(
        pass(&config, &f, Stage::Merge, Step::Merge),
        ["train_off", "no_people", "no_evidence"]
    );
    config.settings.people.insert("maintainer".into());
    config.evidence = c.config.evidence.clone().or(Some(Evidence {
        signer: crate::attest::testing::SIGNER.into(),
        fresh: None,
        require: vec!["check".into()],
        negative: "negative".into(),
        producers: None,
    }));
    assert_eq!(pass(&config, &f, Stage::Merge, Step::Merge), ["train_off"]);
    config.settings.train = true;
    assert_eq!(pass(&config, &f, Stage::Merge, Step::JoinTrain), [0; 0]);

    // An R2 change the owner must approve, with no one whose approval
    // counts, whether or not an owner label lists it.
    config.settings.people.clear();
    for label in [None, Some("needs-peter")] {
        config.settings.owner_label = label.map(String::from);
        assert_eq!(
            pass(&config, &f, Stage::Owner, Step::Wait { until: None }),
            ["no_people"],
            "{label:?}"
        );
    }
    // An owner's call no approval answers isn't the person list's.
    let mut r0 = super::flow_case::Case::new().facts();
    assert_ne!(r0.classified.class, conary_core::Class::R2);
    r0.labels.clear();
    assert_eq!(pass(&config, &r0, Stage::Owner, Step::ListOwner), [0; 0]);

    // The owner label, with no one whose approval counts.
    let mut f = f;
    config.settings.owner_label = Some("needs-peter".into());
    f.labels.insert("needs-peter".into());
    config.settings.people.clear();
    assert_eq!(
        pass(&config, &f, Stage::Owner, Step::ListOwner),
        ["no_people"]
    );
}

/// A held PR is read in full, so its hold can say what it keeps off. A
/// read that fails doesn't keep the hold from stopping its work: the label
/// the open list shows does.
#[test]
fn a_hold_stops_its_work_when_its_read_fails() {
    use crate::sim::Call;
    let c = super::flow_pass_slots::two(1);
    let now = jobs::now();
    super::flow_pass_slots::prepared(&c, now, true);
    c.pass(now + 1, false);
    let proof =
        super::flow_pass_slots::work(&c, 3, |p| matches!(p, jobs::Purpose::Proof)).remove(0);
    c.hub.label(REPO, 3, &["hold"]);
    c.hub.fail(Call::Pull.pr(3));
    c.hub.fail(Call::Reviews.pr(3));
    let reports = c.pass(now + 2, false);
    assert!(reports[0].error.is_some(), "{:?}", reports[0]);
    assert_eq!(
        std::fs::read(proof.path.with_extension("cancel")).unwrap(),
        b"held\n"
    );
}
