//! Stage label configuration (`settings.stage_labels`), schema, and validation
//! through the one loader (`flow::schema::load`, #492).
use conary_github::flow::schema::{self, From};
use conary_github::flow::{Config, Stage, StageLabels};
use serde_json::json;

fn load(file: &str) -> Result<Config, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flow.json");
    std::fs::write(&path, file).unwrap();
    Config::load(&path).map_err(|e| e.to_string())
}

#[test]
fn stage_labels_load_full_and_partial_and_appear_in_schema_and_effective() {
    let full = load(
        r#"{"settings": {"stage_labels": {
            "review": "s/review",
            "fix": "s/fix",
            "ci": "s/ci",
            "proof_slot": "s/proof-slot",
            "proof": "s/proof",
            "release_age": "s/release-age",
            "train": "s/train",
            "merge": "s/merge",
            "owner": "waiting: Alice",
            "base": "s/base"
        }}}"#,
    )
    .unwrap();
    assert_eq!(full.settings.stage_labels.owner, "waiting: Alice");
    assert_eq!(
        full.settings.stage_labels.label(Stage::ProofSlot),
        Some("s/proof-slot")
    );
    assert_eq!(
        full.settings.stage_labels.stage("waiting: Alice"),
        Some(Stage::Owner)
    );
    assert_eq!(full.settings.stage_labels.stage("waiting: owner"), None);

    // Omitted stages keep their defaults, which name no person.
    let partial = load(r#"{"settings": {"stage_labels": {"owner": "waiting: Alice"}}}"#).unwrap();
    assert_eq!(
        partial.settings.stage_labels,
        StageLabels {
            owner: "waiting: Alice".into(),
            ..StageLabels::default()
        }
    );

    // Every stage label field is listed in the schema and in effective settings.
    let flow_schema = schema::flow().unwrap();
    let props = flow_schema["$defs"]["StageLabels"]["properties"]
        .as_object()
        .unwrap();
    assert_eq!(props.len(), Stage::LABELLED.len());
    for (name, option) in props {
        assert!(option["description"].is_string(), "{name}");
        assert_eq!(option["x-spender"], json!(false), "{name}");
        assert_eq!(option["x-live"], json!(true), "{name}");
    }

    let listing = schema::effective::<Config>(
        br#"{"settings": {"stage_labels": {"owner": "waiting: Alice"}}}"#,
    )
    .unwrap();
    let get = |name: &str| {
        let o = listing
            .options
            .iter()
            .find(|o| o.name == name)
            .unwrap_or_else(|| panic!("{name} isn't listed"));
        (o.value.clone(), o.from)
    };
    assert_eq!(
        get("settings.stage_labels.owner"),
        (json!("waiting: Alice"), From::File)
    );
    assert_eq!(
        get("settings.stage_labels.review"),
        (json!("waiting: review"), From::Default)
    );
}

#[test]
fn empty_duplicate_or_colliding_stage_labels_are_refused_without_quoting_values() {
    for (file, expected_option) in [
        (
            r#"{"settings": {"stage_labels": {"unknown_stage": "x"}}}"#,
            "option the schema doesn't declare",
        ),
        (
            r#"{"settings": {"stage_labels": {"owner": "   "}}}"#,
            "settings.stage_labels.owner",
        ),
        (
            r#"{"settings": {"stage_labels": {"owner": ""}}}"#,
            "settings.stage_labels.owner",
        ),
        // Case-insensitive collision between two stages.
        (
            r#"{"settings": {"stage_labels": {"review": "SecretLabel", "fix": "secretlabel"}}}"#,
            "settings.stage_labels.fix collides with settings.stage_labels.review",
        ),
        // Collision with hold_label, owner_label, and priority_labels.
        (
            r#"{"settings": {"hold_label": "SecretHold", "stage_labels": {"owner": "secrethold"}}}"#,
            "settings.stage_labels.owner collides with settings.hold_label",
        ),
        (
            r#"{"settings": {"owner_label": "SecretOwner", "stage_labels": {"owner": "secretowner"}}}"#,
            "settings.stage_labels.owner collides with settings.owner_label",
        ),
        (
            r#"{"settings": {"priority_labels": {"SecretPrio": "high"}, "stage_labels": {"train": "secretprio"}}}"#,
            "settings.stage_labels.train collides with settings.priority_labels",
        ),
    ] {
        let err = load(file).unwrap_err();
        assert!(
            err.contains(expected_option),
            "{file} -> {err} (expected {expected_option})"
        );
        assert!(
            !err.to_lowercase().contains("secret"),
            "error must not quote configured values: {err}"
        );
        assert!(schema::effective::<Config>(file.as_bytes()).is_err());
    }
}
