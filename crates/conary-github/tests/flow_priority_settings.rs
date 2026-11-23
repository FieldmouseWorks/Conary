//! The priority options as a configuration file gives them, read through the
//! one loader (`flow::schema::load`), the way a rendered file is (#415).
use conary_github::flow::{Config, Level, StallAfter};

fn load(file: &str) -> Result<Config, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flow.json");
    std::fs::write(&path, file).unwrap();
    Config::load(&path).map_err(|e| e.to_string())
}

#[test]
fn levels_and_their_deadlines_load_from_the_file() {
    let config = load(
        r#"{"settings": {
            "priority_labels": {"first": "high", "later": "low"},
            "default_priority": "low",
            "stall_after": {"high": 600, "medium": 3600, "low": 86400}
        }}"#,
    )
    .unwrap();
    let s = &config.settings;
    assert_eq!(
        s.priority_labels.iter().collect::<Vec<_>>(),
        vec![
            (&"first".to_owned(), &Level::High),
            (&"later".to_owned(), &Level::Low)
        ]
    );
    assert_eq!(s.default_priority, Level::Low);
    assert_eq!(
        s.stall_after,
        StallAfter {
            high: 600,
            medium: 3600,
            low: 86400
        }
    );
    // A level left out keeps its default.
    let partial = load(r#"{"settings": {"stall_after": {"high": 900}}}"#).unwrap();
    assert_eq!(
        partial.settings.stall_after,
        StallAfter {
            high: 900,
            ..StallAfter::default()
        }
    );
}

#[test]
fn the_binary_first_label_a_scalar_deadline_and_an_unknown_level_are_refused() {
    for refused in [
        r#"{"settings": {"first_label": "first"}}"#,
        r#"{"settings": {"stall_after": 7200}}"#,
        r#"{"settings": {"stall_after": {"urgent": 60}}}"#,
        r#"{"settings": {"priority_labels": {"first": "urgent"}}}"#,
        r#"{"settings": {"default_priority": "first"}}"#,
        // One label given two levels.
        r#"{"settings": {"priority_labels": {"first": "high", "first": "low"}}}"#,
    ] {
        assert!(load(refused).is_err(), "{refused}");
    }
}
