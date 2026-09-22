// apps/conary-test/src/explorer/tests/jev.rs
#![cfg(test)]

use super::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn initial_context(remaining: u32) -> DecisionContext {
    EpisodeMemory::new(&observation())
        .context(&DecisionRequest::new(observation(), remaining).unwrap())
}

async fn server(
    statuses: Vec<u16>,
    variant: &'static str,
) -> (String, tokio::task::JoinHandle<usize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut calls = 0;
        for status in statuses {
            let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_secs(3), listener.accept()).await
            else {
                break;
            };
            calls += 1;
            let mut bytes = Vec::new();
            let header_end;
            loop {
                let mut part = [0u8; 1024];
                let n = stream.read(&mut part).await.unwrap();
                if n == 0 {
                    return calls;
                }
                bytes.extend_from_slice(&part[..n]);
                if let Some(at) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                    header_end = at + 4;
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            if variant == "authenticated" {
                assert!(
                    headers
                        .to_lowercase()
                        .contains("authorization: bearer redshirt-test-credential")
                );
            } else {
                assert!(!headers.to_lowercase().contains("authorization:"));
            }
            let size: usize = headers
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap();
            while bytes.len() < header_end + size {
                let mut part = [0u8; 1024];
                let n = stream.read(&mut part).await.unwrap();
                if n == 0 {
                    return calls;
                }
                bytes.extend_from_slice(&part[..n]);
            }
            let request: Value =
                serde_json::from_slice(&bytes[header_end..header_end + size]).unwrap();
            let (key, question) = request["questions"]
                .as_object()
                .unwrap()
                .iter()
                .next()
                .unwrap();
            let ids = question["criteria"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let mut selected = ids[0].clone();
            if variant == "approximate_install" {
                selected = question["criteria"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .find(|(_, v)| v["action"] == json!({"kind":"install","fixture":"app_v1"}))
                    .unwrap()
                    .0
                    .clone();
            }
            if variant == "history" {
                let state = &request["state"];
                assert_eq!(state["coverage"]["distinct_package_states"], calls);
                assert_eq!(state["coverage"]["state_changing_operations"], calls - 1);
                assert_eq!(state["recent_steps"].as_array().unwrap().len(), calls - 1);
                assert!(
                    state["goal"]
                        .as_str()
                        .unwrap()
                        .contains("Maximize distinct")
                );
                if calls > 1 {
                    let step = &state["recent_steps"][calls - 2];
                    assert_eq!(step["exit_code"], 0);
                    assert_eq!(step["checks_passed"], 6);
                    assert_eq!(step["classifications"], json!(["pass"]));
                    assert_ne!(step["before"], step["after"]);
                }
                let action = match calls {
                    1 => json!({"kind":"install","fixture":"app_v1"}),
                    2 => json!({"kind":"update","fixture":"app_v2"}),
                    _ => json!({"kind":"stop"}),
                };
                selected = question["criteria"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .find(|(_, value)| value["action"] == action)
                    .unwrap()
                    .0
                    .clone();
                assert!(
                    !question["criteria"][&selected]["effect"]
                        .as_str()
                        .unwrap()
                        .is_empty()
                );
            }
            let mut probabilities = ids
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        if *id == selected {
                            if matches!(variant, "probability_sum" | "approximate_install") {
                                0.99
                            } else {
                                1.0
                            }
                        } else {
                            0.0
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>();
            match variant {
                "probability_sum_outside" => {
                    probabilities.insert(selected.clone(), 0.98);
                }
                "nonmaximum" => {
                    probabilities.insert(selected.clone(), 0.4);
                    probabilities.insert(ids[1].clone(), 0.6);
                }
                "missing_probability" => {
                    probabilities.remove(&ids[1]);
                }
                "negative_probability" => {
                    probabilities.insert(selected.clone(), 1.01);
                    probabilities.insert(ids[1].clone(), -0.01);
                }
                _ => {}
            }
            let key = if variant == "stale" {
                "next_action_old"
            } else {
                key
            };
            let choice = if variant == "unknown" {
                "c999"
            } else {
                &selected
            };
            let mut body = json!({"model": crate::explorer::jev::MODEL, "answers": {key: {"type": "choice", "choice": choice, "probabilities": probabilities, "confidence": 1.0}}, "usage": {"input_tokens": 20, "output_tokens": 4}}).to_string();
            if variant == "authenticated" {
                let mut value: Value = serde_json::from_str(&body).unwrap();
                value["debug_echo"] = json!("redshirt-test-credential");
                body = value.to_string();
            }
            if variant == "malformed" {
                body = "{bad".into();
            }
            if variant == "timeout" {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
        calls
    });
    (url, task)
}

#[tokio::test]
async fn one_request_budget_stops_cleanly_without_another_call_or_dispatch() {
    let (url, server) = server(vec![200], "valid").await;
    let cancel = Arc::new(AtomicBool::new(false));
    let mut selector =
        crate::explorer::jev::Jev::mock(&url, 1, 2, Duration::from_secs(1), cancel.clone())
            .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut environment = Fake::default();
    let report = controller::run(
        &mut environment,
        Some(&mut selector),
        &mut Campaign::new(Limits::default()).unwrap(),
        Episode {
            mode: Mode::Exploration,
            identity: identity(),
            replay: None,
            output: &tmp.path().join("one-request"),
            cancel: &cancel,
        },
    )
    .await
    .unwrap();
    assert_eq!(server.await.unwrap(), 1);
    assert_eq!(environment.dispatched, 1);
    assert_eq!(report.operations, vec![Action::Inspect]);
    assert_eq!(report.stop_reason, "provider request budget exhausted");
    assert_eq!(report.cleanup, "removed");
    assert!(report.reset_verified);
    assert_eq!(report.evaluations.len(), 12);
    assert!(
        report
            .evaluations
            .iter()
            .all(|e| e.classification == Classification::Pass)
    );
}

#[tokio::test]
async fn last_allowed_rate_limit_failure_is_not_a_successful_budget_stop() {
    let (url, server) = server(vec![429], "valid").await;
    let mut selector = crate::explorer::jev::Jev::mock(
        &url,
        1,
        2,
        Duration::from_secs(1),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let error = selector
        .select(
            &DecisionRequest::new(observation(), 10).unwrap(),
            &initial_context(10),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<selector::RequestBudgetExhausted>()
            .is_none()
    );
    assert!(error.to_string().contains("HTTP 429"));
    assert_eq!(server.await.unwrap(), 1);
    assert_eq!(selector.take_evidence().len(), 1);
}

#[tokio::test]
async fn provider_mock_valid_malformed_stale_auth_rate_overload_timeout_and_cancel() {
    for (statuses, variant, valid, expected) in [
        (vec![200], "valid", true, 1),
        (vec![200], "malformed", false, 1),
        (vec![200], "stale", false, 1),
        (vec![200], "unknown", false, 1),
        (vec![200], "probability_sum", true, 1),
        (vec![200], "probability_sum_outside", false, 1),
        (vec![200], "nonmaximum", false, 1),
        (vec![200], "missing_probability", false, 1),
        (vec![200], "negative_probability", false, 1),
        (vec![401], "valid", false, 1),
        (vec![422], "valid", false, 1),
        (vec![429, 200], "valid", true, 2),
        (vec![529, 200], "valid", true, 2),
        (vec![529, 529], "valid", false, 2),
        (vec![200], "timeout", false, 1),
    ] {
        let (url, server) = server(statuses, variant).await;
        let mut selector = crate::explorer::jev::Jev::mock(
            &url,
            2,
            2,
            if variant == "timeout" {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(1)
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let request = DecisionRequest::new(observation(), 10).unwrap();
        assert_eq!(
            selector
                .select(&request, &initial_context(10))
                .await
                .is_ok(),
            valid,
            "{variant}"
        );
        assert_eq!(selector.take_evidence().len(), expected);
        assert_eq!(server.await.unwrap(), expected);
    }
    let cancel = Arc::new(AtomicBool::new(true));
    let mut selector = crate::explorer::jev::Jev::mock(
        "http://127.0.0.1:1/v1/systemone",
        1,
        1,
        Duration::from_millis(50),
        cancel,
    )
    .unwrap();
    assert!(
        selector
            .select(
                &DecisionRequest::new(observation(), 1).unwrap(),
                &initial_context(1)
            )
            .await
            .is_err()
    );
    assert!(selector.take_evidence().is_empty());
}

#[test]
fn provider_rejects_external_endpoints_and_disabled_limits() {
    for url in [
        "https://api.typesafe.ai/v1/systemone",
        "http://example.com/v1/systemone",
        "http://localhost/v1/systemone",
        "http://127.0.0.1/wrong",
    ] {
        assert!(
            crate::explorer::jev::Jev::mock(
                url,
                2,
                1,
                Duration::from_secs(1),
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn authenticated_transport_redacts_credentials_and_respects_shared_call_budget() {
    let (url, server) = server(vec![200], "authenticated").await;
    let mut selector =
        crate::explorer::jev::Jev::authenticated_mock(&url, "redshirt-test-credential", 1).unwrap();
    let request = DecisionRequest::new(observation(), 10).unwrap();
    assert!(
        selector
            .select(&request, &initial_context(10))
            .await
            .is_ok()
    );
    assert!(
        selector
            .select(&request, &initial_context(10))
            .await
            .is_err()
    );
    assert_eq!(server.await.unwrap(), 1);
    let records = selector.take_evidence();
    assert_eq!(records.len(), 1);
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("redshirt-test-credential")
    );
    assert!(
        records[0]["response"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]")
    );
    assert_eq!(selector.identity(), "jev-local-mock (no live model)");
}

#[test]
fn live_selector_requires_valid_credential_and_bounded_explicit_budget() {
    use crate::explorer::jev::Jev;
    for (key, requests) in [("", 1), ("test\r\nsecret", 1), ("test", 0), ("test", 9)] {
        assert!(Jev::live(key, requests, 2, Arc::new(AtomicBool::new(false))).is_err());
    }
    let live = Jev::live(
        "synthetic-test-only",
        1,
        1,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(live.identity(), "jev-live");
    assert_eq!(live.configuration()["request_limit"], 1);
    assert_eq!(live.configuration()["max_attempts_per_decision"], 1);
    for attempts in [0, 4] {
        assert!(
            Jev::live(
                "synthetic-test-only",
                8,
                attempts,
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
    }
    assert!(
        !live
            .configuration()
            .to_string()
            .contains("synthetic-test-only")
    );
}

#[tokio::test]
async fn mock_receives_actual_checked_history_and_coverage_before_next_choice() {
    let (url, server) = server(vec![200; 3], "history").await;
    let cancel = Arc::new(AtomicBool::new(false));
    let mut selector =
        crate::explorer::jev::Jev::mock(&url, 3, 1, Duration::from_secs(1), cancel.clone())
            .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut environment = Fake::default();
    let report = controller::run(
        &mut environment,
        Some(&mut selector),
        &mut Campaign::new(Limits::default()).unwrap(),
        Episode {
            mode: Mode::Exploration,
            identity: identity(),
            replay: None,
            output: &tmp.path().join("history"),
            cancel: &cancel,
        },
    )
    .await
    .unwrap();
    assert_eq!(server.await.unwrap(), 3);
    assert_eq!(
        report.operations,
        vec![
            Action::Install(Fixture::AppV1),
            Action::Update(Fixture::AppV2),
            Action::Stop
        ]
    );
    assert_eq!(report.stop_reason, "selector_stop");
    assert_eq!(report.cleanup, "removed");
    assert_eq!(report.evaluations.len(), 18);
    assert!(report.evaluations.iter().all(|e| e.passed == Some(true)));
    let events = std::fs::read_to_string(tmp.path().join("history/events.jsonl")).unwrap();
    let contexts = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event"] == "decision_context")
        .collect::<Vec<_>>();
    assert_eq!(contexts.len(), 3);
    assert_eq!(
        contexts[2]["data"]["recent_steps"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn stale_or_rebound_context_is_rejected_before_http() {
    let mut selector = crate::explorer::jev::Jev::mock(
        "http://127.0.0.1:1/v1/systemone",
        1,
        1,
        Duration::from_millis(50),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let request = DecisionRequest::new(observation(), 8).unwrap();
    let mut context = initial_context(7);
    let error = selector.select(&request, &context).await.unwrap_err();
    assert!(error.to_string().contains("context binding mismatch"));
    context = initial_context(8);
    context.candidates.get_mut("c0").unwrap().action = Action::Stop;
    assert!(selector.select(&request, &context).await.is_err());
    assert!(selector.take_evidence().is_empty());
}

#[tokio::test]
async fn choice_contract_is_audited_and_invalid_maps_never_dispatch_or_retry() {
    for (variant, expected_outcome, dispatches) in [
        ("approximate_install", "accepted_approximate", 1),
        ("probability_sum_outside", "rejected_total", 0),
        ("nonmaximum", "rejected_choice_not_maximum", 0),
        ("missing_probability", "rejected_candidate_keys", 0),
        ("negative_probability", "rejected_probability_value", 0),
    ] {
        let (url, server) = server(vec![200], variant).await;
        let cancel = Arc::new(AtomicBool::new(false));
        // Invalid responses must stop even when a second HTTP attempt is available.
        let requests = if dispatches == 1 { 1 } else { 2 };
        let mut selector = crate::explorer::jev::Jev::mock(
            &url,
            requests,
            2,
            Duration::from_secs(1),
            cancel.clone(),
        )
        .unwrap();
        let mut environment = Fake::default();
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join(variant);
        let report = controller::run(
            &mut environment,
            Some(&mut selector),
            &mut Campaign::new(Limits::default()).unwrap(),
            Episode {
                mode: Mode::Exploration,
                identity: identity(),
                replay: None,
                output: &output,
                cancel: &cancel,
            },
        )
        .await
        .unwrap();
        assert_eq!(server.await.unwrap(), 1, "{variant}");
        assert_eq!(environment.dispatched, dispatches, "{variant}");
        assert!(report.reset_verified);
        assert_eq!(report.cleanup, "removed");
        let events = std::fs::read_to_string(output.join("events.jsonl")).unwrap();
        let events = events
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect::<Vec<_>>();
        let finals = events
            .iter()
            .filter(|e| e["event"] == "required_final_checks")
            .collect::<Vec<_>>();
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0]["data"][1].as_array().unwrap().len(), 6);
        let receipts = events
            .iter()
            .filter(|e| e["event"] == "selector_receipts")
            .flat_map(|e| e["data"].as_array().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(receipts.len(), 1);
        let audit = &receipts[0]["choice_validation"];
        assert_eq!(audit["outcome"], expected_outcome);
        assert_eq!(audit["policy"], "choice-approximate-total-v1");
        assert_eq!(audit["total_tolerance"], 0.01);
        if dispatches == 1 {
            assert_eq!(report.operations, vec![Action::Install(Fixture::AppV1)]);
            assert_eq!(report.stop_reason, "provider request budget exhausted");
            assert!(report.evaluations.iter().all(|e| e.passed == Some(true)));
            let raw: Value =
                serde_json::from_str(receipts[0]["response"].as_str().unwrap()).unwrap();
            let answer = raw["answers"].as_object().unwrap().values().next().unwrap();
            assert_eq!(
                answer["probabilities"][answer["choice"].as_str().unwrap()],
                0.99
            );
            assert_eq!(audit["raw_total"], 0.99);
        } else {
            assert!(report.operations.is_empty());
            assert!(
                report
                    .evaluations
                    .iter()
                    .any(|e| e.classification == Classification::HarnessFailure)
            );
        }
        assert!(
            std::fs::read_to_string(output.join("report.md"))
                .unwrap()
                .contains("choice-approximate-total-v1")
        );
    }
}

#[tokio::test]
async fn one_attempt_stops_on_throttling_even_with_unused_request_budget() {
    for status in [429, 529] {
        let (url, server) = server(vec![status], "valid").await;
        let mut selector = crate::explorer::jev::Jev::mock(
            &url,
            8,
            1,
            Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let request = DecisionRequest::new(observation(), 8).unwrap();
        assert!(
            selector
                .select(&request, &initial_context(8))
                .await
                .is_err()
        );
        assert_eq!(selector.configuration()["max_attempts_per_decision"], 1);
        assert_eq!(selector.take_evidence().len(), 1);
        assert_eq!(server.await.unwrap(), 1);
    }
}
