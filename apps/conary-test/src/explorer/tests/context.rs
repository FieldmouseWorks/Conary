// apps/conary-test/src/explorer/tests/context.rs
#![cfg(test)]

use super::*;

#[test]
fn bounded_history_preserves_checked_results_without_counting_checks_as_exploration() {
    let baseline = observation();
    let mut memory = EpisodeMemory::new(&baseline);
    let checks = checker::Oracle::default().evaluate(&baseline.facts, false);
    let mut after = baseline.clone();
    for revision in 1..=64 {
        let before = after.clone();
        after.revision = revision;
        after.facts.publication = Some(PublicationFact {
            snapshot_id: revision as i64,
            status: "building".into(),
            phase: "boot".into(),
            error: None,
        });
        memory.record(
            &before,
            &Receipt {
                operation_id: revision.to_string(),
                action: Action::Check,
                exit_code: 0,
                stdout: "untrusted raw output must not enter history".into(),
                stderr: String::new(),
            },
            &after,
            &checks,
        );
    }
    let request = DecisionRequest::new(after, 1).unwrap();
    let context = memory.context(&request);
    assert_eq!(context.recent_steps.len(), 8);
    assert_eq!(context.coverage.distinct_package_states, 1);
    assert_eq!(context.coverage.state_changing_operations, 0);
    assert_eq!(context.coverage.distinct_state_changing_transitions, 0);
    assert_eq!(context.coverage.visited_states[0].observations, 65);
    assert_eq!(context.candidates["c1"].attempts_from_current_state, 64);
    assert_eq!(context.recent_steps[0].checks_passed, 6);
    let body = crate::explorer::jev::Jev::request(&request, &context).unwrap();
    assert!(!body.to_string().contains("untrusted raw output"));
    assert!(body.to_string().len() <= 16384);
    let reset = EpisodeMemory::new(&baseline).context(&request);
    assert!(reset.recent_steps.is_empty());
    assert_eq!(reset.candidates["c1"].attempts_total, 0);
}

#[test]
fn incomplete_observation_is_unknown_and_refusal_remains_a_refusal() {
    let before = observation();
    let mut memory = EpisodeMemory::new(&before);
    let receipt = Receipt {
        operation_id: "1".into(),
        action: Action::Remove(Package::App),
        exit_code: 1,
        stdout: String::new(),
        stderr: "absent".into(),
    };
    let checks = [Evaluation {
        criterion: "operation.refusal_or_success".into(),
        checker: CHECKER.into(),
        classification: Classification::ExpectedRefusal,
        passed: Some(true),
        detail: String::new(),
    }];
    memory.record(&before, &receipt, &before, &checks);
    let mut after = before.clone();
    after.facts.complete = false;
    memory.record(
        &before,
        &receipt,
        &after,
        &[Evaluation {
            classification: Classification::Inconclusive,
            passed: None,
            ..checks[0].clone()
        }],
    );
    let context = memory.context(&DecisionRequest::new(after, 1).unwrap());
    assert_eq!(context.coverage.distinct_package_states, 1);
    assert_eq!(context.coverage.visited_states[0].observations, 2);
    assert_eq!(
        context.recent_steps[0].classifications,
        vec![Classification::ExpectedRefusal]
    );
    assert_eq!(context.recent_steps[1].after, None);
    assert_eq!(context.recent_steps[1].checks_inconclusive, 1);
}
