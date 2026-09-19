// apps/conary-test/src/explorer/tests/jev.rs
#![cfg(test)]

use super::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
            let probabilities = ids
                .iter()
                .enumerate()
                .map(|(i, id)| (id.clone(), if i == 0 { 1.0 } else { 0.0 }))
                .collect::<BTreeMap<_, _>>();
            let key = if variant == "stale" {
                "next_action_old"
            } else {
                key
            };
            let choice = if variant == "unknown" {
                "c999"
            } else {
                &ids[0]
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
        .select(&DecisionRequest::new(observation(), 10).unwrap())
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
        assert_eq!(selector.select(&request).await.is_ok(), valid, "{variant}");
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
            .select(&DecisionRequest::new(observation(), 1).unwrap())
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
    assert!(selector.select(&request).await.is_ok());
    assert!(selector.select(&request).await.is_err());
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
        assert!(Jev::live(key, requests, Arc::new(AtomicBool::new(false))).is_err());
    }
    let live = Jev::live("synthetic-test-only", 1, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(live.identity(), "jev-live");
    assert_eq!(live.configuration()["request_limit"], 1);
    assert!(
        !live
            .configuration()
            .to_string()
            .contains("synthetic-test-only")
    );
}
