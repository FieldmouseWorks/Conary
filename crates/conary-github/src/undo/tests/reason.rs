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
        &[store.ledger().change(x).unwrap()], &[], &[], "main", false, &marker,
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
    let pr = hub.open_pull(REMI, &branch, "main", "undo: #5", &marker.line()).unwrap();
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!((got.pr, got.how), (pr, How::Found));
    let undo = store.ledger().change(got.change).unwrap().undo.as_ref().unwrap();
    assert_eq!(serde_json::to_value(undo).unwrap()["reason"], REASON);
}
