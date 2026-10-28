use super::*;
use crate::Store;
use crate::id::CommitId;
use crate::op::{Actor, ActorKind, Authority, Body, CheckKind, Outcome};

fn c(n: &str) -> CommitId {
    n.repeat(40 / n.len()).parse().unwrap()
}

/// A ledger holding two changes on o/r: 1, landed with evidence on two
/// revisions, from session s; and 2, open, by a person.
fn ledger() -> (Ledger, ChangeId, RevisionId) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(
        crate::log::Lock::take(dir.path()).unwrap(),
        ed25519_dalek::SigningKey::from_bytes(&[4; 32]),
        None,
    )
    .unwrap();
    let (one, two) = (ChangeId::new().unwrap(), ChangeId::new().unwrap());
    let (old, landed) = (
        RevisionId::new(&c("a"), &c("c")),
        RevisionId::new(&c("a"), &c("b")),
    );
    let observe = |change, pr, kind, session: Option<&str>| Body::Observe {
        change,
        repo: "o/r".parse().unwrap(),
        pr,
        author: Actor {
            kind,
            login: "someone".into(),
            session: session.map(Into::into),
        },
    };
    let evidence = |revision, check: &str| Body::Evidence {
        change: one,
        revision,
        kind: CheckKind::Ci,
        check: check.into(),
        result: Outcome::Pass,
        source: "https://github.com/o/r/runs/1".into(),
    };
    for body in [
        observe(one, 1, ActorKind::Agent, Some("s")),
        Body::Revision {
            change: one,
            base: c("a"),
            head: c("c"),
        },
        evidence(old, "stale"),
        Body::Revision {
            change: one,
            base: c("a"),
            head: c("b"),
        },
        evidence(landed, "flake"),
        Body::Land {
            change: one,
            revision: landed,
            before: c("a"),
            after: c("d"),
            by: Authority::Status {
                login: "steward".into(),
            },
        },
        observe(two, 2, ActorKind::Person, None),
    ] {
        store.record(body).unwrap();
    }
    (store.ledger().clone(), one, landed)
}

fn prs(v: &Value) -> Vec<u64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| c["pr"].as_u64().unwrap())
        .collect()
}

#[test]
fn a_change_is_found_by_its_id_or_its_pr() {
    let (ledger, one, _) = ledger();
    let by_id = call(&ledger, "change.get", &json!({"change": one.to_string()})).unwrap();
    assert_eq!(by_id["pr"], 1);
    assert_eq!(by_id["url"], "https://github.com/o/r/pull/1");
    assert_eq!(by_id["status"], "landed");
    let by_pr = call(&ledger, "change.get", &json!({"repo": "o/r", "pr": 1})).unwrap();
    assert_eq!(by_pr, by_id);
    // One that isn't there is nothing, not an error.
    let missing = call(&ledger, "change.get", &json!({"repo": "o/r", "pr": 3})).unwrap();
    assert!(missing.is_null());
}

#[test]
fn bad_params_are_refused_as_bad_params() {
    let (ledger, one, _) = ledger();
    for (method, params) in [
        ("change.get", json!({})),
        ("change.get", json!({"change": one.to_string(), "pr": 1})),
        ("change.get", json!({"change": "not an id"})),
        ("change.list", json!({"status": "merged"})),
        ("change.list", json!({"author": "someone"})),
        ("evidence.query", json!(null)),
        ("landings", json!({})),
        ("landings", json!({"repo": "not a repo"})),
    ] {
        let e = call(&ledger, method, &params).unwrap_err();
        assert_eq!(e.code, Error::INVALID_PARAMS, "{method} {params}: {e:?}");
    }
    let e = call(&ledger, "change.delete", &json!({})).unwrap_err();
    assert_eq!(e.code, Error::NO_METHOD);
}

#[test]
fn landings_are_the_notes_of_a_repos_landed_changes() {
    let (ledger, one, landed) = ledger();
    let got = call(&ledger, "landings", &json!({"repo": "O/R"})).unwrap();
    let notes: Vec<crate::notes::Note> = serde_json::from_value(got).unwrap();
    // The open change has no landing, so no note.
    assert_eq!(notes.len(), 1);
    assert_eq!((notes[0].change, notes[0].landing.revision), (one, landed));
    let elsewhere = call(&ledger, "landings", &json!({"repo": "o/other"})).unwrap();
    assert_eq!(elsewhere, json!([]));
}

#[test]
fn nothing_is_served_that_runs_git() {
    let (ledger, _, _) = ledger();
    let params = json!({"dir": "/", "path": "f", "line": 1});
    let e = call(&ledger, "why", &params).unwrap_err();
    assert_eq!(e.code, Error::NO_METHOD);
    assert!(METHODS.iter().all(|m| m.name != "why"));
}

#[test]
fn the_list_narrows_by_each_filter() {
    let (ledger, _, _) = ledger();
    let list = |p| prs(&call(&ledger, "change.list", &p).unwrap());
    assert_eq!(list(json!(null)), [1, 2]);
    assert_eq!(list(json!({"status": "open"})), [2]);
    assert_eq!(list(json!({"session": "s"})), [1]);
    assert_eq!(list(json!({"repo": "o/other"})), Vec::<u64>::new());
    assert_eq!(list(json!({"since": i64::MAX})), Vec::<u64>::new());
    assert_eq!(list(json!({"repo": "o/r", "since": 0})), [1, 2]);
}

#[test]
fn evidence_is_every_revisions_or_one() {
    let (ledger, one, landed) = ledger();
    let checks = |p| -> Vec<String> {
        call(&ledger, "evidence.query", &p)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["check"].as_str().unwrap().to_owned())
            .collect()
    };
    let id = one.to_string();
    assert_eq!(checks(json!({"change": id})), ["stale", "flake"]);
    let only = json!({"change": id, "revision": landed.to_string()});
    assert_eq!(checks(only), ["flake"]);
}

#[test]
fn every_method_listed_is_one_that_answers() {
    let (ledger, _, _) = ledger();
    for m in METHODS {
        assert_eq!((m.params)()["type"], "object", "{}", m.name);
        let answered = call(&ledger, m.name, &json!({"nothing": 1}));
        assert_eq!(
            answered.unwrap_err().code,
            Error::INVALID_PARAMS,
            "{}",
            m.name
        );
    }
}

fn reply(line: &str, ledger: &Ledger) -> Value {
    serde_json::from_str(&answer(line, || Ok(ledger)).unwrap()).unwrap()
}

#[test]
fn a_line_gets_a_response_with_its_id() {
    let (ledger, _, _) = ledger();
    let r = reply(
        r#"{"jsonrpc":"2.0","id":"x","method":"change.get","params":{"repo":"o/r","pr":2}}"#,
        &ledger,
    );
    assert_eq!((&r["id"], &r["result"]["pr"]), (&json!("x"), &json!(2)));
    let r = reply(r#"{"jsonrpc":"2.0","id":4,"method":"nope"}"#, &ledger);
    assert_eq!(
        (&r["id"], &r["error"]["code"]),
        (&json!(4), &json!(Error::NO_METHOD))
    );
}

#[test]
fn a_null_id_is_a_request_and_gets_its_response() {
    let (ledger, _, _) = ledger();
    let r = reply(
        r#"{"jsonrpc":"2.0","id":null,"method":"change.list"}"#,
        &ledger,
    );
    assert!(r.as_object().unwrap().contains_key("id"), "{r}");
    assert_eq!((&r["id"], &r["result"][0]["pr"]), (&Value::Null, &json!(1)));
}

#[test]
fn a_malformed_line_is_answered_without_reading_the_ledger() {
    for (line, code) in [
        ("{not json", Error::PARSE),
        (r#"{"jsonrpc":"2.0","id":1}"#, Error::INVALID_REQUEST),
        (
            r#"{"jsonrpc":"1.0","id":1,"method":"why"}"#,
            Error::INVALID_REQUEST,
        ),
        (r#"[1]"#, Error::INVALID_REQUEST),
        (
            r#"{"jsonrpc":"2.0","id":true,"method":"change.list"}"#,
            Error::INVALID_REQUEST,
        ),
        (
            r#"{"jsonrpc":"2.0","id":[1],"method":"change.list"}"#,
            Error::INVALID_REQUEST,
        ),
        (
            r#"{"jsonrpc":"2.0","id":{},"method":"change.list"}"#,
            Error::INVALID_REQUEST,
        ),
    ] {
        let r = answer::<&Ledger>(line, || panic!("read the ledger for {line}")).unwrap();
        let r: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(r["error"]["code"], code, "{line}");
    }
}

#[test]
fn a_ledger_that_cant_be_read_fails_the_call() {
    let r = answer::<&Ledger>(r#"{"jsonrpc":"2.0","id":1,"method":"change.list"}"#, || {
        Err("the log doesn't verify".into())
    })
    .unwrap();
    let r: Value = serde_json::from_str(&r).unwrap();
    assert_eq!(r["error"]["code"], Error::FAILED);
    assert_eq!(r["error"]["message"], "the log doesn't verify");
}

#[test]
fn a_notification_gets_no_response() {
    let (ledger, _, _) = ledger();
    let line = r#"{"jsonrpc":"2.0","method":"change.list"}"#;
    assert!(answer(line, || Ok(&ledger)).is_none());
    // Not even when it fails.
    assert!(answer(r#"{"jsonrpc":"2.0","method":"nope"}"#, || Ok(&ledger)).is_none());
}
