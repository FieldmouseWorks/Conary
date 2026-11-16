//! Negotiated offers survive the fresh process used for work.
use conary_core::plugin::*;
use serde_json::{Value, json};
#[path = "support/scripted.rs"]
mod scripted;
use scripted::scripted;

fn describe() -> Value {
    json!({
        "contract": CONTRACT_V1,
        "by": {"engine": "forwarder", "session": "describe-session"},
        "kind": "describe", "versions": CONTRACTS,
        "kinds": ["review"],
        "models": [{"id": "fixture-model", "efforts": ["quick", "deep"],
                    "context_tokens": 123, "max_output_tokens": 45}],
        "capabilities": {"sandbox": "brings", "network": true, "devices": ["kvm"], "mcp": true},
        "meters": ["session", "week"]
    })
}

fn review() -> Task {
    Task::Review {
        repo: "o/r".into(),
        pr: 1,
        base: "1".repeat(40),
        head: "2".repeat(40),
        author: Identity {
            engine: "writer".into(),
            session: Some("writer-session".into()),
        },
    }
}

fn answer(contract: &str, engine: &str, model: &str) -> Value {
    json!({
        "contract": contract, "by": {"engine": engine, "session": "work-session"},
        "execution": {"model": model, "effort": "quick", "context": {"kind": "fresh"}},
        "kind": "review", "findings": []
    })
}

#[test]
fn work_carries_the_describe_snapshot_even_after_the_settings_change() {
    let described = describe();
    let plugin = scripted(
        described.clone(),
        &[answer(CONTRACT, "forwarder", "fixture-model")],
    );
    let handle = plugin.plugin().unwrap();
    // The next describe process would see different settings. Work must
    // still carry the original negotiation, without reading describe again.
    let mut changed = described.clone();
    changed["by"]["engine"] = json!("replacement");
    changed["models"] = json!([]);
    std::fs::write(plugin.path.with_extension("describe"), changed.to_string()).unwrap();
    handle.call(review()).unwrap();
    let requests = plugin.requests();
    assert_eq!(
        requests[0]["negotiated"],
        json!({
            "by": described["by"],
            "offers": {"models": described["models"], "capabilities": described["capabilities"],
                       "meters": described["meters"]}
        })
    );
    assert!(requests[0]["execution"]["choice"].is_null());
}

#[test]
fn partial_and_explicit_choices_use_the_same_snapshot_before_publication() {
    for choice in [
        None,
        Some(Choice {
            engine: "forwarder".into(),
            model: None,
            effort: None,
        }),
        Some(Choice {
            engine: "forwarder".into(),
            model: Some("fixture-model".into()),
            effort: None,
        }),
        Some(Choice {
            engine: "forwarder".into(),
            model: Some("fixture-model".into()),
            effort: Some("quick".into()),
        }),
    ] {
        let plugin = scripted(
            describe(),
            &[answer(CONTRACT, "forwarder", "fixture-model")],
        );
        let response = plugin
            .plugin()
            .unwrap()
            .call_with(review(), choice.clone(), Context::Fresh, None)
            .unwrap();
        let request: Request = serde_json::from_value(plugin.requests()[0].clone()).unwrap();
        let execution = request.execution.as_ref().unwrap();
        assert_eq!(execution.choice, choice);
        let negotiated = request.negotiated.as_ref().unwrap();
        negotiated
            .verify(execution, &response.provenance().unwrap())
            .unwrap();
        // Describe's session is not the work session. Only engine stability
        // is required; a work invocation creates its own session.
        assert_ne!(negotiated.by.session, response.by.session);
    }
}

#[test]
fn the_producer_and_runner_refuse_the_same_changed_engine_or_execution() {
    for (engine, model, effort, context, choice) in [
        (
            "replacement",
            "fixture-model",
            "quick",
            Context::Fresh,
            None,
        ),
        ("forwarder", "new-model", "quick", Context::Fresh, None),
        (
            "forwarder",
            "fixture-model",
            "new-effort",
            Context::Fresh,
            None,
        ),
        (
            "forwarder",
            "fixture-model",
            "quick",
            Context::Continue {
                session: "old".into(),
            },
            None,
        ),
        (
            "forwarder",
            "fixture-model",
            "deep",
            Context::Fresh,
            Some(Choice {
                engine: "forwarder".into(),
                model: Some("fixture-model".into()),
                effort: Some("quick".into()),
            }),
        ),
    ] {
        let mut response = answer(CONTRACT, engine, model);
        response["execution"]["effort"] = json!(effort);
        response["execution"]["context"] = serde_json::to_value(context).unwrap();
        let actual: Response = serde_json::from_value(response.clone()).unwrap();
        let plugin = scripted(describe(), &[response]);
        let error = plugin
            .plugin()
            .unwrap()
            .call_with(review(), choice, Context::Fresh, None)
            .unwrap_err();
        let request: Request = serde_json::from_value(plugin.requests()[0].clone()).unwrap();
        let producer_error = request
            .negotiated
            .as_ref()
            .unwrap()
            .verify(
                request.execution.as_ref().unwrap(),
                &actual.provenance().unwrap(),
            )
            .unwrap_err();
        assert_eq!(producer_error.0, error.0);
    }
}

#[test]
fn older_contracts_leave_the_snapshot_off_the_wire() {
    for contract in &CONTRACTS[..4] {
        let mut description = describe();
        description["versions"] = json!([contract]);
        let plugin = scripted(
            description,
            &[answer(contract, "forwarder", "fixture-model")],
        );
        plugin.plugin().unwrap().call(review()).unwrap();
        let wire = &plugin.requests()[0];
        assert_eq!(wire["contract"], *contract);
        assert!(wire.get("negotiated").is_none());
        let mut request: Request = serde_json::from_value(wire.clone()).unwrap();
        // Even a caller filling the field cannot add it to an old wire.
        let description = describe();
        request.negotiated = Some(
            serde_json::from_value(json!({
                "by": description["by"], "offers": {
                    "models": description["models"], "capabilities": description["capabilities"],
                    "meters": description["meters"]
                }
            }))
            .unwrap(),
        );
        assert_eq!(serde_json::to_value(request).unwrap(), *wire);
    }
}

#[test]
fn v5_work_requires_a_complete_valid_snapshot_and_execution() {
    let plugin = scripted(
        describe(),
        &[answer(CONTRACT, "forwarder", "fixture-model")],
    );
    plugin.plugin().unwrap().call(review()).unwrap();
    let wire = plugin.requests()[0].clone();
    for field in ["negotiated", "execution"] {
        let mut bad = wire.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<Request>(bad).is_err(),
            "missing {field}"
        );
        let mut bad = wire.clone();
        bad[field] = Value::Null;
        assert!(
            serde_json::from_value::<Request>(bad).is_err(),
            "null {field}"
        );
    }
    for field in ["by", "offers"] {
        let mut bad = wire.clone();
        bad["negotiated"].as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Request>(bad).is_err());
    }
    let mut bad = wire.clone();
    bad["negotiated"]["by"]["engine"] = json!(" ");
    assert!(serde_json::from_value::<Request>(bad).is_err());
    let mut bad = wire;
    bad["execution"]["choice"] = json!({"engine": "other", "model": null, "effort": null});
    assert!(serde_json::from_value::<Request>(bad).is_err());
    // Describe stays on v1 with neither work-only field.
    let describe = serde_json::to_value(Request::describe()).unwrap();
    assert_eq!(
        describe,
        json!({"contract": CONTRACT_V1, "kind": "describe"})
    );
    assert!(serde_json::from_value::<Request>(describe).is_ok());
}
