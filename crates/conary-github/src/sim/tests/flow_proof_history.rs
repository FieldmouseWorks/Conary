//! A proof's deadline is sized from its recipes' own history, never above
//! `proof_timeout` (`docs/flow.md`, "Deadlines").
use super::flow_pass_case::{Case, REPO, example};
use super::flow_pass_slots::work;
use super::head;
use crate::flow::durations::{self, Durations, FLOOR, MARGIN, Run, Sized, Source};
use crate::flow::jobs::{self, Completion, Purpose, Work};
use crate::flow::*;
use conary_core::plugin::{self, Verdict};
use serde_json::json;

/// Three hours, as FP12's scenario configures.
const TIMEOUT: u64 = 10_800;
/// About how long the recipe's runs take: ten minutes.
const RUN: u64 = 600;

/// PR 3, reviewed clean on a green head, its `machine` check pending: a
/// program the proof plugin runs, with one proof slot.
fn case() -> Case {
    let mut c = Case::new();
    c.config.settings.proof_slots = Some(1);
    c.config.settings.proof_timeout = TIMEOUT;
    c.config.settings.proofs.insert("machine".into(), recipe());
    c.config.plugins.proof = Some(example());
    c.clean_review();
    pending(&c, &head(3));
    c
}
fn recipe() -> Recipe {
    Recipe::Run(vec!["check".into()])
}
fn pending(c: &Case, sha: &str) {
    c.hub
        .post_status(sha, json!({"context":"machine","state":"pending"}));
}
/// PR 3 moves to `sha`, green but for its pending `machine` check.
fn push(c: &Case, sha: &str) {
    c.hub.push(REPO, 3, sha);
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(sha);
    c.clean_review();
    pending(c, sha);
}
fn proofs(c: &Case) -> Vec<Work> {
    work(c, 3, |p| matches!(p, Purpose::Proof))
}
/// Real passes from `t`, each preparation answered ready and needing a
/// slot, until PR 3 has started `n` proofs: the last of them.
fn start(c: &Case, t: &mut u64, n: usize) -> Work {
    for _ in 0..20 {
        c.prepared(true, None);
        let next = c.pass(*t, false)[0].next.clone().unwrap();
        if let Some(proof) = proofs(c).get(n - 1) {
            return proof.clone();
        }
        *t = next.wait_until.filter(|w| *w > *t).unwrap_or(*t + 60);
    }
    panic!("proof {n} never started: {:?}", proofs(c));
}
/// The proof plugin answers `work` `after` seconds from its start, its
/// `machine` check with `verdict`.
fn answer(c: &Case, work: &Work, verdict: Verdict, after: u64) {
    c.deferred.0.borrow_mut().retain(|p| *p != work.path);
    let completion = Completion {
        at: work.job.started + after,
        result: Some(Ok(plugin::Response {
            contract: plugin::CONTRACT.into(),
            by: plugin::Identity {
                engine: "fixture".into(),
                session: None,
            },
            execution: None,
            answer: plugin::Answer::Proof {
                results: vec![plugin::CheckResult {
                    check: "machine".into(),
                    verdict,
                    why: "ran".into(),
                }],
                head: Some(work.job.revision.head.clone()),
            },
        })),
    };
    std::fs::write(
        work.path.with_extension("answer"),
        serde_json::to_vec(&completion).unwrap(),
    )
    .unwrap();
}
/// The repository's timed proofs file.
fn file(c: &Case) -> std::path::PathBuf {
    durations::path(&crate::flow::persist::dir(
        c.dir.path(),
        &REPO.parse().unwrap(),
    ))
}
/// A run of the `machine` recipe that held a slot and took `seconds`.
fn timed(seconds: u64) -> Durations {
    Durations {
        runs: vec![Run {
            recipes: [("machine".to_string(), recipe())].into(),
            slot: true,
            job: "earlier".into(),
            seconds,
            at: 1,
        }],
    }
}
fn seed(c: &Case, durations: &Durations) {
    std::fs::create_dir_all(file(c).parent().unwrap()).unwrap();
    std::fs::write(file(c), serde_json::to_vec(durations).unwrap()).unwrap();
}

/// FP12's regression scenario: a recipe whose runs take about ten minutes,
/// with `proof_timeout` three hours. Its first proof, with no history, has
/// the configured deadline. Once one has passed in ten minutes, a hung
/// proof is cut at twice that, not after three hours.
#[test]
fn a_hung_proof_is_cut_at_its_history_sized_deadline() {
    let c = case();
    let mut t = jobs::now();
    let first = start(&c, &mut t, 1);
    assert_eq!(first.deadline, first.job.started + TIMEOUT, "{first:?}");
    answer(&c, &first, Verdict::Pass, RUN);
    t = first.job.started + RUN;
    c.pass(t, false);
    push(&c, &head(4));
    let hung = start(&c, &mut t, 2);
    assert_eq!(hung.deadline, hung.job.started + 2 * RUN, "{hung:?}");
    // Its passes wake by that deadline, and the one after it cuts it.
    let answered = hung.path.with_extension("answer");
    while !answered.exists() {
        let next = c.pass(t, false)[0].next.clone().unwrap();
        let wake = next.wait_until.filter(|w| *w > t).unwrap_or(t + 60);
        assert!(wake <= hung.deadline + 300, "{next:?}");
        t = wake;
    }
    let cut: Completion =
        serde_json::from_slice(&std::fs::read(hung.path.with_extension("answer")).unwrap())
            .unwrap();
    assert!(matches!(cut.result, Some(Err(_))), "{cut:?}");
    assert!(cut.at < hung.job.started + TIMEOUT, "{cut:?}");
}

/// Next shows the deadline a proof starts with, and the one a running
/// proof has, and where each came from; a dry pass the same.
#[test]
fn next_shows_the_deadline_and_where_it_came_from() {
    let c = case();
    let mut t = jobs::now();
    // The pass that decides to prove says what deadline it'll have.
    c.pass(t, false);
    c.prepared(true, None);
    t += 1;
    let prove = c.pass(t, true)[0].next.clone().unwrap();
    assert_eq!(prove.step, Step::Prove, "{prove:?}");
    assert_eq!(prove.deadline, Some(Sized::configured(TIMEOUT)));
    let first = start(&c, &mut t, 1);
    answer(&c, &first, Verdict::Pass, RUN);
    t = first.job.started + RUN;
    c.pass(t, false);
    push(&c, &head(4));
    let second = start(&c, &mut t, 2);
    let sized = Sized {
        seconds: MARGIN * RUN,
        from: Source::History {
            runs: 1,
            longest: RUN,
        },
    };
    assert_eq!(second.sized, Some(sized.clone()));
    let running = c.pass(t, false)[0].next.clone().unwrap();
    assert_eq!(running.deadline, Some(sized.clone()), "{running:?}");
    let dry = c.pass(t, true)[0].next.clone().unwrap();
    assert_eq!(dry.deadline, running.deadline);
    // A restart reads the same history from the flow's records.
    let saved: Durations = serde_json::from_slice(&std::fs::read(file(&c)).unwrap()).unwrap();
    assert_eq!(saved.runs.len(), 1, "{saved:?}");
    assert_eq!(saved.runs[0].job, first.job.id);
    assert_eq!(saved.runs[0].seconds, RUN);
}

/// History never sizes a deadline below the floor or above
/// `proof_timeout`.
#[test]
fn a_sized_deadline_stays_between_the_floor_and_the_configured_one() {
    for (seconds, deadline) in [(60, FLOOR), (6_000, TIMEOUT)] {
        let c = case();
        seed(&c, &timed(seconds));
        let mut t = jobs::now();
        let proof = start(&c, &mut t, 1);
        assert_eq!(proof.deadline, proof.job.started + deadline, "{seconds}");
        assert_eq!(
            proof.sized.unwrap().from,
            Source::History {
                runs: 1,
                longest: seconds
            }
        );
    }
}

/// Only a proof whose checks all passed is timed: one that failed may have
/// stopped early.
#[test]
fn a_failed_proof_isnt_timed() {
    let c = case();
    let mut t = jobs::now();
    let first = start(&c, &mut t, 1);
    answer(&c, &first, Verdict::Fail, 60);
    t = first.job.started + 60;
    c.pass(t, false);
    push(&c, &head(4));
    let second = start(&c, &mut t, 2);
    assert_eq!(second.deadline, second.job.started + TIMEOUT);
    assert_eq!(second.sized, Some(Sized::configured(TIMEOUT)));
}

/// Recipes held another way, or another recipe, have no history of their
/// own: the configured deadline.
#[test]
fn another_recipe_or_slot_has_no_history() {
    let mut other = timed(RUN);
    other.runs[0].slot = false;
    let mut changed = timed(RUN);
    changed.runs[0].recipes = [("machine".to_string(), Recipe::Run(vec!["other".into()]))].into();
    for durations in [other, changed] {
        let c = case();
        seed(&c, &durations);
        let mut t = jobs::now();
        let proof = start(&c, &mut t, 1);
        assert_eq!(
            proof.sized,
            Some(Sized::configured(TIMEOUT)),
            "{durations:?}"
        );
    }
}

/// A proof cut at its sized deadline is retried with the configured one:
/// history said too little for it, and a second deadline on its tree is a
/// stall (FP6).
#[test]
fn a_retry_after_a_sized_deadline_gets_the_configured_one() {
    let c = case();
    seed(&c, &timed(RUN));
    let mut t = jobs::now();
    let hung = start(&c, &mut t, 1);
    assert_eq!(hung.deadline, hung.job.started + MARGIN * RUN);
    let retry = start(&c, &mut t, 2);
    assert_eq!(
        retry.sized,
        Some(Sized {
            seconds: TIMEOUT,
            from: Source::Reached
        })
    );
    assert_eq!(retry.deadline, retry.job.started + TIMEOUT);
}

/// Timed proofs that don't read are never taken as no history: no proof
/// starts, and the PR says why, a dry pass the same.
#[test]
fn durations_that_dont_read_start_no_proof() {
    let c = case();
    std::fs::create_dir_all(file(&c).parent().unwrap()).unwrap();
    std::fs::write(file(&c), b"{not json").unwrap();
    let mut t = jobs::now();
    c.pass(t, false);
    c.prepared(true, None);
    t += 1;
    let next = c.pass(t, false)[0].next.clone().unwrap();
    assert_eq!(next.step, Step::Wait { until: None }, "{next:?}");
    assert!(
        next.description
            .starts_with("Can't read the proofs' durations"),
        "{next:?}"
    );
    assert!(proofs(&c).is_empty());
    let dry = c.pass(t, true)[0].next.clone().unwrap();
    assert_eq!(dry.description, next.description);
    // Nothing writes over them.
    assert_eq!(std::fs::read(file(&c)).unwrap(), b"{not json");
}
