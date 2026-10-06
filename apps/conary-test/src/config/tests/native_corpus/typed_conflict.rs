// apps/conary-test/src/config/tests/native_corpus/typed_conflict.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use rusqlite::Connection;
use serde_json::json;

const COUNT_QUERY: &str = "sqlite3 -json ${DB_PATH} \"SELECT (SELECT COUNT(*) FROM troves WHERE name IN ('phase4-daily-driver-corpus', 'phase4-corpus-conflict')) AS installed, (SELECT COUNT(*) FROM troves WHERE name = 'phase4-corpus-conflict') AS conflicts\"";
const REFUSAL_STEP: &str = ". /tmp/native-pm-corpus-conflict/native-fixture.env && /opt/remi-tests/fixtures/native/assert-w7-command-rejection.sh phase4-corpus-conflict - ${DB_PATH} /conary /tmp/w7-payload-mutation.log 'is incompatible with package phase4-daily-driver-corpus' -- env CONARY_TEST_SKIP_GENERATION_MOUNT=1 ${CONARY_BIN} install \"$NATIVE_PKG_FILE\" --convert-to-ccs --db-path ${DB_PATH} --root /conary --no-deps --yes --sandbox always";
const SELECTED_GENERATION_STEP: &str = "/opt/remi-tests/fixtures/native/assert-selected-generation.py --root /conary --expect-sha256 /usr/bin/phase4-corpus=565d3f61ffdbbca53c7371588ce6884b680dfa1d1c1bd7fe3954b4ab957e0676 --expect-sha256 /var/lib/phase4-corpus/scriptlet.marker=a51a6c19a1ffc7416827e89adf20749d23ad42452c396cf7e627409f2896922c";
const EVIDENCE_STEP: &str = "/opt/remi-tests/fixtures/native/write-corpus-evidence.py /tmp/native-pm-corpus-conflict/native-fixture-manifest.json /tmp/conary-corpus-w7-payload-mutation-failure.json ${native_profile} ${native_corpus_source_format} phase4-corpus-conflict ${native_corpus_fixture_version} ${native_arch} transaction_preflight";

#[test]
fn native_corpus_conflict_count_is_typed_exactly_for_every_distro_override() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native corpus manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM17")
        .expect("TNPM17 must own the file conflict refusal proof");
    assert_eq!(
        test.step.len(),
        5,
        "TNPM17 must retain all five ordered steps"
    );
    assert_eq!(test.step[1].run.as_deref(), Some(REFUSAL_STEP));
    assert_eq!(test.step[2].run.as_deref(), Some(SELECTED_GENERATION_STEP));
    assert_eq!(test.step[4].run.as_deref(), Some(EVIDENCE_STEP));

    let count_step = &test.step[3];
    assert_eq!(count_step.run.as_deref(), Some(COUNT_QUERY));
    assert_eq!(
        test.step
            .iter()
            .filter(|step| step.run.as_deref() == Some(COUNT_QUERY))
            .count(),
        1,
        "TNPM17 must have one authoritative count query"
    );
    let query = sqlite_query(count_step);
    assert_eq!(
        query,
        "SELECT (SELECT COUNT(*) FROM troves WHERE name IN ('phase4-daily-driver-corpus', 'phase4-corpus-conflict')) AS installed, (SELECT COUNT(*) FROM troves WHERE name = 'phase4-corpus-conflict') AS conflicts"
    );
    assert!(!query.contains(';'));
    prove_count_query_predicates(query);

    let assertion = typed_root_assertion(count_step);
    let expected = json!([{ "installed": 1, "conflicts": 0 }]);
    let lanes = [
        ("fedora44", "fedora-44", "rpm"),
        ("ubuntu-26.04", "ubuntu-26.04", "deb"),
        ("arch", "arch", "alpm"),
    ];
    for (distro, profile, source_format) in lanes {
        let overrides = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        assert_eq!(
            overrides.get("native_profile").map(String::as_str),
            Some(profile)
        );
        assert_eq!(
            overrides
                .get("native_corpus_source_format")
                .map(String::as_str),
            Some(source_format)
        );
        let expanded = expand_assertion(assertion, overrides);
        reject_invalid_results(&expanded, &expected, distro);
    }
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.strip_suffix('"'))
        .expect("TNPM17 count must be one quoted sqlite3 -json command")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
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
        .expect("TNPM17 count must use exact JSON assertion");
    assert_eq!(checks.len(), 1, "compare one whole JSON document");
    assert_eq!(checks[0].pointer, "", "compare the root array");
    assert_eq!(
        checks[0].expected,
        JsonExpectation::Equals(json!([{ "installed": 1, "conflicts": 0 }]))
    );
    assertion
}

fn reject_invalid_results(assertion: &Assertion, expected: &serde_json::Value, distro: &str) {
    assert!(evaluate_assertion(assertion, 0, &expected.to_string(), "").is_ok());
    for (stdout, defect) in [
        (
            r#"[{"installed":0,"conflicts":0}]"#,
            "wrong installed count",
        ),
        (
            r#"[{"installed":2,"conflicts":0}]"#,
            "wrong installed count",
        ),
        (
            r#"[{"installed":11,"conflicts":0}]"#,
            "11 installed false positive",
        ),
        (r#"[{"installed":1,"conflicts":1}]"#, "wrong conflict count"),
        (
            r#"[{"installed":1,"conflicts":10}]"#,
            "10 conflicts false positive",
        ),
        (
            r#"[{"installed":"1","conflicts":0}]"#,
            "string installed count",
        ),
        (
            r#"[{"installed":1,"conflicts":"0"}]"#,
            "string conflict count",
        ),
        (
            r#"[{"installed":1.0,"conflicts":0}]"#,
            "decimal installed count",
        ),
        (
            r#"[{"installed":1,"conflicts":0.0}]"#,
            "decimal conflict count",
        ),
        (
            r#"[{"installed":null,"conflicts":0}]"#,
            "null installed count",
        ),
        (
            r#"[{"installed":1,"conflicts":null}]"#,
            "null conflict count",
        ),
        ("[]", "missing row"),
        (r#"[{}]"#, "missing both keys"),
        (r#"[{"conflicts":0}]"#, "missing installed key"),
        (r#"[{"installed":1}]"#, "missing conflicts key"),
        (
            r#"[{"installed":1,"conflicts":0,"extra":true}]"#,
            "extra key",
        ),
        (
            r#"[{"installed":1,"conflicts":0},{"installed":1,"conflicts":0}]"#,
            "extra row",
        ),
        ("not JSON", "malformed stdout"),
        (
            r#"[{"installed":1,"conflicts":0}][{"installed":1,"conflicts":0}]"#,
            "concatenated stdout",
        ),
        ("11 installed\n10 conflicts\n", "legacy count phrases"),
    ] {
        assert!(
            evaluate_assertion(assertion, 0, stdout, "").is_err(),
            "{distro} must reject {defect}: {stdout}"
        );
    }
    assert!(
        evaluate_assertion(assertion, 1, &expected.to_string(), "").is_err(),
        "{distro} must reject nonzero exit with valid JSON"
    );

    let old_matcher = Assertion {
        stdout_contains_all: Some(vec!["1 installed".into(), "0 conflicts".into()]),
        ..Assertion::default()
    };
    assert!(evaluate_assertion(&old_matcher, 0, "11 installed\n10 conflicts\n", "").is_ok());
}

fn prove_count_query_predicates(query: &str) {
    let connection = Connection::open_in_memory().expect("open disposable SQLite database");
    connection
        .execute_batch(
            "CREATE TABLE troves (name TEXT NOT NULL);
             INSERT INTO troves (name) VALUES
                 ('phase4-daily-driver-corpus'),
                 ('phase4-corpus-conflict'),
                 ('unrelated-package');",
        )
        .expect("seed SQLite trove fixture");
    assert_eq!(read_counts(&connection, query), (2, 1));
    connection
        .execute(
            "DELETE FROM troves WHERE name = 'phase4-corpus-conflict'",
            [],
        )
        .expect("remove refused conflict fixture");
    assert_eq!(read_counts(&connection, query), (1, 0));
}

fn read_counts(connection: &Connection, query: &str) -> (i64, i64) {
    connection
        .query_row(query, [], |row| {
            Ok((row.get("installed")?, row.get("conflicts")?))
        })
        .expect("execute TNPM17 scalar count query")
}
