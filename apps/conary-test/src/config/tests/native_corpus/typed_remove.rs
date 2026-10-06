// apps/conary-test/src/config/tests/native_corpus/typed_remove.rs

#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestDef, TestManifest, TestStep};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use rusqlite::Connection;
use serde_json::{Value, json};

const COUNT_COMMAND: &str = "sqlite3 -json ${DB_PATH} \"SELECT (SELECT COUNT(*) FROM troves WHERE name = 'phase4-daily-driver-corpus') AS installed, (SELECT COUNT(*) FROM config_files WHERE path IN ('/etc/phase4-corpus/app.conf', '/etc/phase4-corpus/app-local.conf', '/etc/phase4-corpus/app-deleted.conf', '/etc/phase4-corpus/app-unmatched.conf')) AS config_rows\"";
const REMOVE_COMMAND: &str = "CONARY_TEST_SKIP_GENERATION_MOUNT=1 ${CONARY_BIN} remove phase4-daily-driver-corpus --db-path ${DB_PATH} --root /conary --sandbox always --purge --yes";
const EVIDENCE_COMMAND: &str = "/opt/remi-tests/fixtures/native/write-corpus-evidence.py /tmp/native-pm-corpus/native-fixture-manifest.json /tmp/conary-corpus-daily-driver-removal.json ${native_profile} ${native_corpus_source_format} phase4-daily-driver-corpus ${native_corpus_fixture_version} ${native_arch} removal --update-fixture-manifest /tmp/native-pm-corpus-v2/native-fixture-manifest.json --update-name phase4-daily-driver-corpus --update-version ${native_corpus_update_version} --update-architecture ${native_arch}";

#[test]
fn native_corpus_removal_count_is_typed_exact_and_global_for_all_lanes() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native daily-driver corpus manifest");
    let test = get_test(&manifest, "TNPM19");
    assert_eq!(test.step.len(), 5, "TNPM19 must retain five ordered steps");
    assert_eq!(test.step[0].run.as_deref(), Some(REMOVE_COMMAND));

    let selected = test.step[1].run.as_deref().unwrap();
    assert!(selected.starts_with(
        "/opt/remi-tests/fixtures/native/assert-selected-generation.py --root /conary"
    ));
    assert!(selected.contains("/var/lib/phase4-corpus/remove.marker="));
    assert!(selected.contains("--absent /etc/phase4-corpus/app.conf"));
    let per_format = test.step[2].run.as_deref().unwrap();
    assert!(per_format.starts_with("case \"${native_target}\" in rpm) "));
    assert!(per_format.contains(" ;; deb) "));
    assert!(per_format.contains(" ;; arch) "));
    assert!(per_format.ends_with(" ;; esac"));

    let count_step = &test.step[3];
    assert_eq!(count_step.run.as_deref(), Some(COUNT_COMMAND));
    assert_eq!(
        test.step
            .iter()
            .filter(|step| step.run.as_deref() == Some(COUNT_COMMAND))
            .count(),
        1,
        "TNPM19 must have one count query"
    );
    let query = sqlite_query(count_step);
    assert_eq!(
        query,
        "SELECT (SELECT COUNT(*) FROM troves WHERE name = 'phase4-daily-driver-corpus') AS installed, (SELECT COUNT(*) FROM config_files WHERE path IN ('/etc/phase4-corpus/app.conf', '/etc/phase4-corpus/app-local.conf', '/etc/phase4-corpus/app-deleted.conf', '/etc/phase4-corpus/app-unmatched.conf')) AS config_rows"
    );
    assert!(!query.contains(';'));
    assert_eq!(test.step[4].run.as_deref(), Some(EVIDENCE_COMMAND));

    let assertion = count_assertion(count_step);
    let expected = json!([{ "installed": 0, "config_rows": 0 }]);
    for (distro, profile, source_format) in [
        ("fedora44", "fedora-44", "rpm"),
        ("ubuntu-26.04", "ubuntu-26.04", "deb"),
        ("arch", "arch", "alpm"),
    ] {
        let overrides = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        assert_eq!(
            overrides.get("native_profile").map(String::as_str),
            Some(profile),
            "{distro} profile"
        );
        assert_eq!(
            overrides
                .get("native_corpus_source_format")
                .map(String::as_str),
            Some(source_format),
            "{distro} source format"
        );
        let expanded = expand_assertion(assertion, overrides);
        reject_invalid_results(&expanded, &expected, distro);
    }

    let legacy_matcher = Assertion {
        stdout_contains_all: Some(vec!["0 installed".into(), "0 config rows".into()]),
        ..Assertion::default()
    };
    let legacy_output = "10 installed\n10 config rows\n";
    assert!(
        evaluate_assertion(&legacy_matcher, 0, legacy_output, "").is_ok(),
        "constructed legacy substring matcher must expose its false positive"
    );
    assert!(
        evaluate_assertion(assertion, 0, legacy_output, "").is_err(),
        "loaded typed JSON assertion must reject the constructed legacy false positive"
    );

    prove_query_predicates(query);
}

fn get_test<'a>(manifest: &'a TestManifest, id: &str) -> &'a TestDef {
    manifest
        .test
        .iter()
        .find(|test| test.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.strip_suffix('"'))
        .expect("count must be one quoted sqlite3 -json command")
}

fn count_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("count step needs an assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    assert!(assertion.stderr_contains.is_none());
    assert!(assertion.stderr_not_contains.is_none());
    let checks = assertion
        .stdout_json
        .as_ref()
        .expect("count must use exact JSON assertion");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "", "compare the root array");
    assert_eq!(
        checks[0].expected,
        JsonExpectation::Equals(json!([{ "installed": 0, "config_rows": 0 }]))
    );
    assertion
}

fn reject_invalid_results(assertion: &Assertion, expected: &Value, distro: &str) {
    assert!(evaluate_assertion(assertion, 0, &expected.to_string(), "").is_ok());
    for (stdout, defect) in [
        (r#"[{"installed":1,"config_rows":0}]"#, "installed count"),
        (r#"[{"installed":0,"config_rows":1}]"#, "config row count"),
        (r#"[{"installed":"0","config_rows":0}]"#, "string count"),
        (r#"[{"installed":0,"config_rows":"0"}]"#, "string row count"),
        (r#"[{"installed":0.0,"config_rows":0}]"#, "decimal count"),
        (r#"[{"installed":0,"config_rows":null}]"#, "null row count"),
        ("[]", "missing row"),
        (
            r#"[{"installed":0,"config_rows":0},{"installed":0,"config_rows":0}]"#,
            "extra row",
        ),
        ("[{}]", "missing keys"),
        (r#"[{"installed":0}]"#, "missing config_rows key"),
        (
            r#"[{"installed":0,"config_rows":0,"extra":true}]"#,
            "extra key",
        ),
        ("not JSON", "malformed JSON"),
        (
            r#"[{"installed":0,"config_rows":0}][{"installed":0,"config_rows":0}]"#,
            "trailing JSON",
        ),
    ] {
        assert!(
            evaluate_assertion(assertion, 0, stdout, "").is_err(),
            "{distro} must reject {defect}: {stdout}"
        );
    }
    assert!(
        evaluate_assertion(assertion, 1, &expected.to_string(), "").is_err(),
        "{distro} must reject a nonzero command exit"
    );
}

fn prove_query_predicates(query: &str) {
    let connection = Connection::open_in_memory().expect("open disposable SQLite database");
    connection
        .execute_batch(
            "CREATE TABLE troves (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE config_files (trove_id INTEGER, path TEXT NOT NULL);
             INSERT INTO troves (id, name) VALUES
                 (1, 'phase4-daily-driver-corpus'), (2, 'unrelated-package');
             INSERT INTO config_files (trove_id, path) VALUES
                 (1, '/etc/phase4-corpus/app.conf'), (2, '/tmp/unrelated.conf');",
        )
        .expect("seed SQLite fixture");
    assert_eq!(read_counts(&connection, query), (1, 1));

    connection
        .execute(
            "DELETE FROM troves WHERE name = 'phase4-daily-driver-corpus'",
            [],
        )
        .expect("remove target package row");
    connection
        .execute("DELETE FROM config_files WHERE trove_id = 1", [])
        .expect("remove target-owned config row");
    connection
        .execute(
            "INSERT INTO config_files (trove_id, path) VALUES (NULL, '/etc/phase4-corpus/app-local.conf'), (999, '/etc/phase4-corpus/app-deleted.conf')",
            [],
        )
        .expect("seed matching orphan config rows");
    assert_eq!(
        read_counts(&connection, query),
        (0, 2),
        "matching paths with NULL or foreign trove IDs remain in the global path count"
    );
    assert_eq!(
        connection
            .execute(
                "DELETE FROM config_files WHERE path IN ('/etc/phase4-corpus/app-local.conf', '/etc/phase4-corpus/app-deleted.conf')",
                [],
            )
            .expect("remove matching orphan config rows"),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM config_files WHERE path = '/tmp/unrelated.conf'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count unrelated config row"),
        1,
        "unrelated config row must remain after target-path cleanup"
    );
    assert_eq!(
        read_counts(&connection, query),
        (0, 0),
        "post-purge state must have no target package or matching config paths"
    );
}

fn read_counts(connection: &Connection, query: &str) -> (i64, i64) {
    connection
        .query_row(query, [], |row| {
            Ok((row.get("installed")?, row.get("config_rows")?))
        })
        .expect("execute TNPM19 scalar count query")
}
