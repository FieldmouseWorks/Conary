use super::*;

const REASON: &str = "Red run https://github.com/o/r/actions/runs/123: check / build failed.\nerror[E0425]: cannot find value `missing` in this scope";

fn marker_with_reason(x: ChangeId) -> Marker {
    serde_json::from_value(serde_json::json!({
        "reverts": [x], "asked": {"by": "change"}, "reason": REASON,
    }))
    .unwrap()
}

#[test]
fn the_red_run_reason_opens_the_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let x = landed(&mut store, None);
    let marker = marker_with_reason(x);
    let body = super::super::body(
        &[store.ledger().change(x).unwrap()],
        &[],
        &[],
        "main",
        false,
        &marker,
    );
    assert!(body.starts_with(&format!("{REASON}\n\n")), "{body}");
}

#[test]
fn recovery_records_the_original_red_run_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, None);
    let revert = a_revert(&hub);
    let branch = undo_branch(x, 1);
    hub.create_branch(REMI, &branch, &revert).unwrap();
    let marker = marker_with_reason(x);
    let pr = hub
        .open_pull(REMI, &branch, "main", "undo: #5", &marker.line())
        .unwrap();
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!((got.pr, got.how), (pr, How::Found));
    let undo = store
        .ledger()
        .change(got.change)
        .unwrap()
        .undo
        .as_ref()
        .unwrap();
    assert_eq!(serde_json::to_value(undo).unwrap()["reason"], REASON);
}

#[test]
fn a_request_keeps_the_red_run_reason_in_the_body_and_signed_operation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, None);
    let got = undo(&mut store, &hub, &hub, &config(), x, REASON).unwrap();
    let pull = hub.pull_json(REMI, got.pr);
    assert!(
        pull["body"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{REASON}\n\n"))
    );
    let marker = Marker::read(&undo_branch(x, 1), pull["body"].as_str().unwrap()).unwrap();
    assert_eq!(marker.reason, REASON);
    // Another request and an edited marker cannot replace the recorded reason.
    let retry = undo(&mut store, &hub, &hub, &config(), x, "A later retry").unwrap();
    assert_eq!(retry.change, got.change);
    hub.edit_pull(REMI, got.pr, |p| {
        p["body"] = Marker {
            reason: "An edited reason".into(),
            ..marker
        }
        .line()
        .into();
    });
    ingest::pull(&mut store, &hub, &config(), &repo(), got.pr).unwrap();
    drop(store);
    let ops = conary_core::log::read(
        dir.path(),
        &SigningKey::from_bytes(&[5; 32]).verifying_key(),
        None,
    )
    .unwrap();
    let reasons: Vec<_> = ops
        .iter()
        .filter_map(|op| match &op.op.body {
            Body::Undo { reason, .. } => Some(reason.as_deref()),
            _ => None,
        })
        .collect();
    assert_eq!(reasons, [Some(REASON)]);
    let replayed = conary_core::Ledger::replay(&ops).unwrap();
    assert_eq!(
        replayed
            .change(got.change)
            .unwrap()
            .undo
            .as_ref()
            .unwrap()
            .reason
            .as_deref(),
        Some(REASON)
    );
}

#[test]
fn grouped_requests_keep_the_reason_in_every_repo_and_recovery() {
    for by_actor in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github();
        let session = "session with broken deployments";
        super::group::chain(
            &mut store,
            &hub,
            &repo(),
            10,
            &[Some(session), Some(session)],
        );
        super::group::chain(
            &mut store,
            &hub,
            &"FieldmouseWorks/burrow".parse().unwrap(),
            20,
            &[Some(session)],
        );
        let request = |store: &mut Store, reason: &str| {
            if by_actor {
                undo_actor(store, &hub, &hub, &config(), session, reason)
            } else {
                undo_since(store, &hub, &hub, &config(), &repo(), 0, reason)
            }
        };
        // Opened but not recorded: taking it up must use the original reason.
        hub.fail(Call::Pull.pr(100));
        let stopped = request(&mut store, REASON).unwrap_err();
        let pr = stopped.unrecorded.unwrap();
        hub.heal(Call::Pull);
        let got = request(&mut store, "A different retry reason").unwrap();
        assert_eq!(got.undos.len(), if by_actor { 2 } else { 1 });
        let recovered = store.ledger().by_pr(&pr.repo, pr.pr).unwrap();
        assert_eq!(
            recovered.undo.as_ref().unwrap().reason.as_deref(),
            Some(REASON)
        );
        for undone in got.undos {
            let u = store.ledger().change(undone.change).unwrap();
            let reason = u.undo.as_ref().unwrap().reason.as_ref().unwrap();
            let pull = hub.pull_json(&u.repo.to_string(), undone.pr);
            assert!(
                pull["body"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("{reason}\n\n"))
            );
            assert_eq!(
                Marker::read(
                    pull["head"]["ref"].as_str().unwrap(),
                    pull["body"].as_str().unwrap()
                )
                .unwrap()
                .reason,
                *reason
            );
        }
    }
}

#[test]
fn blank_library_reasons_are_refused_before_refresh_or_github_work() {
    for reason in ["", " \t\n\u{2003}"] {
        for selection in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let mut store = store(&dir);
            let hub = github();
            let session = "session with broken deployments";
            let ids = super::group::chain(&mut store, &hub, &repo(), 10, &[Some(session)]);
            // This would be ingested by the first refresh if validation were late.
            stopped(&hub, ids[0]);
            let head = store.head();
            let branches = hub.names(REMI);
            let made = hub.made();
            let calls = [
                Call::Pulls,
                Call::Pull,
                Call::DefaultBranch,
                Call::MakeCommit,
                Call::OpenPull,
            ];
            let before = calls.map(|c| hub.count(c));
            let err = match selection {
                0 => undo(&mut store, &hub, &hub, &config(), ids[0], reason).map(|_| ()),
                1 => undo_actor(&mut store, &hub, &hub, &config(), session, reason).map(|_| ()),
                _ => undo_since(&mut store, &hub, &hub, &config(), &repo(), 0, reason).map(|_| ()),
            }
            .unwrap_err();
            assert!(err.refused());
            assert!(matches!(*err.error, UndoError::Reason));
            assert_eq!(store.head(), head);
            assert_eq!(hub.names(REMI), branches);
            assert_eq!(hub.made(), made);
            assert_eq!(calls.map(|c| hub.count(c)), before);
            assert_eq!(pulls(&hub), 1);
        }
    }
}

#[test]
fn reasonless_markers_are_refused_before_recording_instead_of_becoming_ordinary_prs() {
    for reason in [None, Some(""), Some(" \t\n")] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github();
        let x = landed(&mut store, None);
        let pr = stopped(&hub, x);
        let mut marker = serde_json::json!({"reverts": [x], "asked": {"by": "change"}});
        if let Some(reason) = reason {
            marker["reason"] = reason.into();
        }
        hub.edit_pull(REMI, pr, |p| {
            p["body"] = format!("<!-- conary-undo: {marker} -->").into()
        });
        let head = store.head();
        let err = ingest::pull(&mut store, &hub, &config(), &repo(), pr).unwrap_err();
        assert!(matches!(
            err,
            IngestError::Store(StoreError::Invalid(Invalid::UndoReason))
        ));
        assert_eq!(store.head(), head);
        assert!(store.ledger().by_pr(&repo(), pr).is_none());
        // A new request refuses this occupied branch too, before opening anything.
        assert!(undo(&mut store, &hub, &hub, &config(), x, REASON).is_err());
        assert_eq!(pulls(&hub), 1);
        assert_eq!(store.head(), head);
    }
}

#[test]
fn a_recorded_undo_keeps_its_rules_when_its_marker_lacks_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, None);
    let got = undo(&mut store, &hub, &hub, &config(), x, REASON).unwrap();
    let marker = serde_json::json!({"reverts": [x], "asked": {"by": "change"}});
    hub.edit_pull(REMI, got.pr, |p| {
        p["body"] = format!("<!-- conary-undo: {marker} -->").into()
    });
    hub.steward_lands(REMI, got.pr);
    ingest::pull(&mut store, &hub, &config(), &repo(), got.pr).unwrap();
    assert_eq!(store.ledger().change(x).unwrap().status(), Kept::Reverted);
    assert_eq!(
        store
            .ledger()
            .change(got.change)
            .unwrap()
            .undo
            .as_ref()
            .unwrap()
            .reason
            .as_deref(),
        Some(REASON)
    );
}
