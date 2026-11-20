//! A seal key given as a file (`judge_key_file`, `major_key_file`): read
//! into its key each time the configuration loads, so a machine's rendered
//! configuration can be public while the key stays readable by the flow
//! alone.
use conary_github::flow::schema;
use conary_github::flow::{Config, SealKey};
use serde_json::json;
use std::path::Path;

const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn write(dir: &Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn config(dir: &Path, settings: serde_json::Value) -> std::path::PathBuf {
    write(
        dir,
        "flow.json",
        &json!({ "settings": settings }).to_string(),
    )
}

#[test]
fn each_key_reads_from_its_file_at_each_load() {
    let dir = tempfile::tempdir().unwrap();
    // As a secret manager writes one: a trailing newline.
    let judge = write(dir.path(), "judge", &format!("{KEY}\n"));
    let major = write(dir.path(), "major", &format!("  {}\n", "f".repeat(64)));
    let flow = config(
        dir.path(),
        json!({ "judge_key_file": judge, "major_key_file": major }),
    );
    let loaded = Config::load(&flow).unwrap();
    assert_eq!(
        loaded.settings.judge_key,
        Some(SealKey::from_hex(KEY).unwrap())
    );
    assert_eq!(
        loaded.settings.major_key,
        Some(SealKey::from_hex(&"f".repeat(64)).unwrap())
    );

    // A key changed in its file is the next load's.
    let other = "a".repeat(64);
    std::fs::write(&judge, &other).unwrap();
    let reloaded = Config::load(&flow).unwrap();
    assert_eq!(
        reloaded.settings.judge_key,
        Some(SealKey::from_hex(&other).unwrap())
    );
}

#[test]
fn a_key_given_both_ways_or_a_file_without_one_is_refused_unquoted() {
    let dir = tempfile::tempdir().unwrap();
    let judge = write(dir.path(), "judge", KEY);
    let both = config(
        dir.path(),
        json!({ "judge_key": KEY, "judge_key_file": judge }),
    );
    let e = Config::load(&both).unwrap_err().to_string();
    assert!(e.contains("both set"), "{e}");
    assert!(!e.contains(KEY), "{e}");

    // One digit short, and upper case: neither is quoted.
    for bad in [&KEY[1..], &KEY.to_uppercase()] {
        std::fs::write(&judge, bad).unwrap();
        let flow = config(dir.path(), json!({ "judge_key_file": judge }));
        let e = Config::load(&flow).unwrap_err().to_string();
        assert!(e.contains("judge_key_file"), "{e}");
        assert!(!e.contains(bad), "{e}");
    }

    let flow = config(
        dir.path(),
        json!({ "major_key_file": dir.path().join("missing") }),
    );
    let e = Config::load(&flow).unwrap_err().to_string();
    assert!(
        e.contains("major_key_file") && e.contains("can't be read"),
        "{e}"
    );
}

/// `cn flow settings` reads the configuration as a machine's build does,
/// where the key's file isn't: it lists the path, not the key, and opens
/// nothing.
#[test]
fn the_settings_listing_names_the_file_without_opening_it() {
    let missing = "/nonexistent/judge-seal";
    let file = json!({ "settings": { "judge_key_file": missing } }).to_string();
    let listing = serde_json::to_value(schema::effective::<Config>(file.as_bytes()).unwrap())
        .unwrap()
        .to_string();
    assert!(listing.contains(missing), "{listing}");
}
