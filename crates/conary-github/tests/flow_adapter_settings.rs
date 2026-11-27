//! Configuration loading, published schema, effective listing, and validation
//! for `settings.review_adapter` and `settings.red_fix_branch` (#618).
use conary_github::flow::schema::{self, From};
use conary_github::flow::{Config, CustomReviewAdapter, DEFAULT_RED_FIX_BRANCH, ReviewAdapter};
use serde_json::json;

fn load(file: &str) -> Result<Config, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flow.json");
    std::fs::write(&path, file).unwrap();
    Config::load(&path).map_err(|e| e.to_string())
}

#[test]
fn review_adapter_and_red_fix_branch_load_and_appear_in_schema_and_effective() {
    let default_cfg = load("{}").unwrap();
    assert_eq!(default_cfg.settings.review_adapter, ReviewAdapter::Conary);
    assert_eq!(default_cfg.settings.red_fix_branch, DEFAULT_RED_FIX_BRANCH);
    assert_eq!(default_cfg.settings.red_fix_branch, "conary/red-");

    let codex_cfg =
        load(r#"{"settings": {"review_adapter": "codex", "red_fix_branch": "autoland/red-"}}"#)
            .unwrap();
    assert_eq!(codex_cfg.settings.review_adapter, ReviewAdapter::Codex);
    assert_eq!(codex_cfg.settings.red_fix_branch, "autoland/red-");

    let custom_cfg = load(
        r#"{"settings": {
            "review_adapter": {
                "custom": {
                    "check_name": "acme-review",
                    "ask_comment": "@acme review",
                    "state_marker": "acme-state:v1",
                    "receipt_marker": "acme-receipt:v1"
                }
            },
            "red_fix_branch": "ops/trunk-fix-"
        }}"#,
    )
    .unwrap();
    assert_eq!(
        custom_cfg.settings.review_adapter,
        ReviewAdapter::Custom(CustomReviewAdapter {
            check_name: "acme-review".into(),
            ask_comment: Some("@acme review".into()),
            state_marker: "acme-state:v1".into(),
            receipt_marker: "acme-receipt:v1".into(),
        })
    );
    assert_eq!(custom_cfg.settings.red_fix_branch, "ops/trunk-fix-");

    // Both options appear in the published schema with descriptions, x-spender: false, x-live: true.
    let flow_schema = schema::flow().unwrap();
    let props = flow_schema["$defs"]["Settings"]["properties"]
        .as_object()
        .unwrap();
    for field in ["review_adapter", "red_fix_branch"] {
        let opt = &props[field];
        assert!(opt["description"].is_string(), "{field}");
        assert_eq!(opt["x-spender"], json!(false), "{field}");
        assert_eq!(opt["x-live"], json!(true), "{field}");
    }
    assert_eq!(props["review_adapter"]["default"], json!("conary"));
    assert_eq!(props["red_fix_branch"]["default"], json!("conary/red-"));

    // Effective listing distinguishes Default from File.
    let listing = schema::effective::<Config>(
        br#"{"settings": {"review_adapter": "codex", "red_fix_branch": "ops/red-"}}"#,
    )
    .unwrap();
    let get = |name: &str| {
        let o = listing
            .options
            .iter()
            .find(|o| o.name == name)
            .unwrap_or_else(|| panic!("{name} not listed"));
        (o.value.clone(), o.from)
    };
    assert_eq!(get("settings.review_adapter"), (json!("codex"), From::File));
    assert_eq!(
        get("settings.red_fix_branch"),
        (json!("ops/red-"), From::File)
    );

    let default_listing = schema::effective::<Config>(b"{}").unwrap();
    let get_default = |name: &str| {
        let o = default_listing
            .options
            .iter()
            .find(|o| o.name == name)
            .unwrap_or_else(|| panic!("{name} not listed"));
        (o.value.clone(), o.from)
    };
    assert_eq!(
        get_default("settings.review_adapter"),
        (json!("conary"), From::Default)
    );
    assert_eq!(
        get_default("settings.red_fix_branch"),
        (json!("conary/red-"), From::Default)
    );
}

#[test]
fn invalid_review_adapter_and_red_fix_branch_are_refused_without_quoting_values() {
    for (file, expected) in [
        (
            r#"{"settings": {"red_fix_branch": ""}}"#,
            "settings.red_fix_branch",
        ),
        (
            r#"{"settings": {"red_fix_branch": "   "}}"#,
            "settings.red_fix_branch",
        ),
        (
            r#"{"settings": {"red_fix_branch": "/secret-prefix"}}"#,
            "settings.red_fix_branch",
        ),
        (
            r#"{"settings": {"red_fix_branch": "secret..prefix"}}"#,
            "settings.red_fix_branch",
        ),
        (
            r#"{"settings": {"red_fix_branch": "secret prefix"}}"#,
            "settings.red_fix_branch",
        ),
        (
            r#"{"settings": {"review_adapter": "secret-adapter"}}"#,
            "type or form",
        ),
        (
            r#"{"settings": {"review_adapter": {"custom": {"check_name": "", "state_marker": "s", "receipt_marker": "r"}}}}"#,
            "settings.review_adapter.custom.check_name",
        ),
        (
            r#"{"settings": {"review_adapter": {"custom": {"check_name": "secret name", "state_marker": "s", "receipt_marker": "r"}}}}"#,
            "settings.review_adapter.custom.check_name",
        ),
        (
            r#"{"settings": {"review_adapter": {"custom": {"check_name": "acme", "ask_comment": "   ", "state_marker": "s", "receipt_marker": "r"}}}}"#,
            "settings.review_adapter.custom.ask_comment",
        ),
        (
            r#"{"settings": {"review_adapter": {"custom": {"check_name": "acme", "state_marker": "secret-->bad", "receipt_marker": "r"}}}}"#,
            "settings.review_adapter.custom.state_marker",
        ),
        (
            r#"{"settings": {"review_adapter": {"custom": {"check_name": "acme", "state_marker": "s", "receipt_marker": "secret-->bad"}}}}"#,
            "settings.review_adapter.custom.receipt_marker",
        ),
        (
            r#"{"settings": {"review_adapter": {"custom": {"check_name": "acme", "state_marker": "secret-same", "receipt_marker": "secret-same"}}}}"#,
            "must differ",
        ),
    ] {
        let err = load(file).unwrap_err();
        assert!(
            err.contains(expected),
            "{file} -> {err} (expected {expected})"
        );
        assert!(
            !err.to_lowercase().contains("secret"),
            "error must not quote configured values: {err}"
        );
        assert!(schema::effective::<Config>(file.as_bytes()).is_err());
    }
}

#[test]
fn cli_review_adapter_parser_accepts_named_and_custom_json_and_refuses_bad_input() {
    assert_eq!(
        ReviewAdapter::parse("conary").unwrap(),
        ReviewAdapter::Conary
    );
    assert_eq!(ReviewAdapter::parse("codex").unwrap(), ReviewAdapter::Codex);
    let custom = ReviewAdapter::parse(
        r#"{"custom":{"check_name":"acme","ask_comment":null,"state_marker":"acme-s","receipt_marker":"acme-r"}}"#,
    )
    .unwrap();
    assert_eq!(custom.check_name(), "acme");
    assert_eq!(custom.ask_comment(), None);
    assert!(ReviewAdapter::parse("unknown").is_err());
    assert!(
        ReviewAdapter::parse(
            r#"{"custom":{"check_name":"acme","state_marker":"same","receipt_marker":"same"}}"#
        )
        .is_err()
    );
}
