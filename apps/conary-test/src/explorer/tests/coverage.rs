// apps/conary-test/src/explorer/tests/coverage.rs
#![cfg(test)]

use super::*;
use crate::explorer::coverage::CoverageGreedy;

#[tokio::test]
async fn coverage_choices_are_deterministic_bound_and_ignore_noop_candidates() {
    let request = DecisionRequest::new(observation(), 8).unwrap();
    let mut context = EpisodeMemory::new(&request.observation).context(&request);
    let first = CoverageGreedy.select(&request, &context).await.unwrap();
    let second = CoverageGreedy.select(&request, &context).await.unwrap();
    assert_eq!(first.candidate_id, second.candidate_id);
    assert_eq!(
        request.authorize(&first, &request.observation).unwrap(),
        Action::Install(Fixture::AppV1)
    );
    context.request_binding = "stale".into();
    assert!(CoverageGreedy.select(&request, &context).await.is_err());
    context.request_binding = request.binding.clone();
    context.candidates.remove(&first.candidate_id);
    assert!(CoverageGreedy.select(&request, &context).await.is_err());
}

#[tokio::test]
async fn refused_projection_never_becomes_coverage_or_starves_untried_actions() {
    let mut before = observation();
    before.facts.packages = BTreeMap::from([
        (Package::App, "2.0.0".into()),
        (Package::Companion, "1.0.0".into()),
    ]);
    let mut memory = EpisodeMemory::new(&before);
    let request = DecisionRequest::new(before.clone(), 8).unwrap();
    let selected = CoverageGreedy
        .select(&request, &memory.context(&request))
        .await
        .unwrap();
    let action = request.authorize(&selected, &before).unwrap();
    assert_eq!(action, Action::Update(Fixture::AppV1));
    memory.record(
        &before,
        &Receipt {
            operation_id: "refused".into(),
            action: action.clone(),
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
        },
        &before,
        &[],
    );
    let context = memory.context(&request);
    assert_eq!(context.coverage.distinct_package_states, 1);
    let next = CoverageGreedy.select(&request, &context).await.unwrap();
    assert_eq!(
        request.authorize(&next, &before).unwrap(),
        Action::Remove(Package::App)
    );
}

#[tokio::test]
async fn incomplete_facts_stop_without_speculating_about_packages() {
    let mut before = observation();
    before.facts.complete = false;
    let request = DecisionRequest::new(before.clone(), 8).unwrap();
    let context = EpisodeMemory::new(&before).context(&request);
    let decision = CoverageGreedy.select(&request, &context).await.unwrap();
    assert_eq!(request.authorize(&decision, &before).unwrap(), Action::Stop);
    assert_eq!(context.coverage.distinct_package_states, 0);
}

#[tokio::test]
async fn greedy_reaches_all_six_fixture_states_despite_a_refused_downgrade() {
    let mut fake = Fake {
        refuse_downgrade: true,
        ..Default::default()
    };
    let mut memory = EpisodeMemory::new(&fake.current);
    let mut refused = 0;
    for remaining in (1..=8).rev() {
        let before = fake.observe().await.unwrap();
        let request = DecisionRequest::new(before.clone(), remaining).unwrap();
        let decision = CoverageGreedy
            .select(&request, &memory.context(&request))
            .await
            .unwrap();
        let action = request.authorize(&decision, &before).unwrap();
        let receipt = fake.execute(&remaining.to_string(), &action).await.unwrap();
        let after = fake.observe().await.unwrap();
        refused += usize::from(receipt.exit_code != 0);
        memory.record(&before, &receipt, &after, &[]);
    }
    let request = DecisionRequest::new(fake.current, 1).unwrap();
    let coverage = memory.context(&request).coverage;
    assert_eq!(coverage.distinct_package_states, 6);
    assert_eq!(coverage.state_changing_operations, 7);
    assert_eq!(refused, 1);
}

#[tokio::test]
async fn greedy_controller_trace_replays_without_a_selector() {
    let tmp = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let limits = Limits {
        actions: 8,
        ..Limits::default()
    };
    let original = controller::run(
        &mut Fake {
            refuse_downgrade: true,
            ..Default::default()
        },
        Some(&mut CoverageGreedy),
        &mut Campaign::new(limits.clone()).unwrap(),
        Episode {
            mode: Mode::Exploration,
            identity: identity(),
            replay: None,
            output: &tmp.path().join("original"),
            cancel: &cancel,
        },
    )
    .await
    .unwrap();
    let replay = Replay::load(&tmp.path().join("original/replay.json")).unwrap();
    let repeated = controller::run(
        &mut Fake {
            refuse_downgrade: true,
            ..Default::default()
        },
        None,
        &mut Campaign::new(limits).unwrap(),
        Episode {
            mode: Mode::Replay,
            identity: identity(),
            replay: Some(&replay),
            output: &tmp.path().join("replay"),
            cancel: &cancel,
        },
    )
    .await
    .unwrap();
    assert_eq!(original.selector, "coverage-greedy-v1");
    assert_eq!(original.operations.len(), 8);
    assert_eq!(original.operations, repeated.operations);
    assert_eq!(repeated.reproduces_recorded_predicate, Some(true));
    for report in [&original, &repeated] {
        assert!(report.reset_verified);
        assert_eq!(report.cleanup, "removed");
        assert!(
            report
                .evaluations
                .iter()
                .all(|check| check.passed == Some(true))
        );
    }
}
