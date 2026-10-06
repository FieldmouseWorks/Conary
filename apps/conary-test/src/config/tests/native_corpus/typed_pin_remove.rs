// apps/conary-test/src/config/tests/native_corpus/typed_pin_remove.rs

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use serde_json::json;

const COUNT_QUERY: &str = "sqlite3 -json ${DB_PATH} \"SELECT COUNT(*) AS installed FROM troves WHERE name = 'phase4-runtime-fixture'\"";

#[test]
fn native_parity_pin_remove_requires_exact_typed_counts_for_all_distro_lanes() {
    let manifest = load_manifest(&remi_manifest_path("phase4-native-pm-parity.toml")).unwrap();
    let cases = [("TNPM06", 3, 1), ("TNPM08", 2, 0)];

    for (test_id, step_index, expected_count) in cases {
        let test = manifest
            .test
            .iter()
            .find(|test| test.id == test_id)
            .unwrap_or_else(|| panic!("missing {test_id}"));
        let count_step = test
            .step
            .get(step_index)
            .unwrap_or_else(|| panic!("{test_id} is missing count step {step_index}"));
        assert_eq!(
            count_step.run.as_deref(),
            Some(COUNT_QUERY),
            "{test_id} must retain the count at its established position"
        );
        assert_eq!(
            test.step
                .iter()
                .filter(|step| step.run.as_deref() == Some(COUNT_QUERY))
                .count(),
            1,
            "{test_id} must have one authoritative count step"
        );
        assert_count_step_position(test_id, &test.step, step_index);

        let assertion = count_step
            .assert
            .as_ref()
            .unwrap_or_else(|| panic!("{test_id} count must be asserted"));
        assert_eq!(assertion.exit_code, Some(0));
        assert!(assertion.stdout_contains.is_none());
        assert!(assertion.stdout_contains_all.is_none());
        assert!(assertion.stdout_contains_any.is_none());
        assert!(assertion.stdout_not_contains.is_none());
        let checks = assertion
            .stdout_json
            .as_ref()
            .unwrap_or_else(|| panic!("{test_id} must use typed JSON assertion"));
        assert_eq!(checks.len(), 1, "{test_id} must compare one whole document");
        assert_eq!(
            checks[0].pointer, "",
            "{test_id} must compare the root array"
        );
        assert_eq!(
            checks[0].expected,
            JsonExpectation::Equals(json!([{ "installed": expected_count }])),
            "{test_id} must assert the exact integer count and one-row shape"
        );

        for distro in ["fedora44", "ubuntu-26.04", "arch"] {
            let vars = manifest
                .distro_overrides
                .get(distro)
                .unwrap_or_else(|| panic!("missing {distro} overrides"));
            let expanded = expand_assertion(assertion, vars);
            let (good, wrong_opposite, false_positive, legacy_phrase) =
                count_documents(expected_count);
            assert!(
                evaluate_assertion(&expanded, 0, good, "").is_ok(),
                "{distro} must accept the exact count {expected_count} in {test_id}"
            );

            let legacy = Assertion {
                stdout_contains: Some(legacy_phrase.to_owned()),
                ..Assertion::default()
            };
            let legacy_text = if expected_count == 1 {
                "11 installed"
            } else {
                "10 installed"
            };
            assert!(
                evaluate_assertion(&legacy, 0, legacy_text, "").is_ok(),
                "control: the old substring check accepts {legacy_text}"
            );

            for (stdout, defect) in [
                (wrong_opposite, "opposite count"),
                (false_positive, "legacy substring false positive"),
                (string_count(expected_count), "string count"),
                (float_count(expected_count), "floating-point count"),
                (null_count(), "null count"),
                ("[]".to_owned(), "missing row"),
                ("[{}]".to_owned(), "missing count key"),
                (extra_row(expected_count), "extra row"),
                (extra_key(expected_count), "extra object key"),
                ("not JSON".to_owned(), "malformed JSON"),
                (concatenated_documents(expected_count), "concatenated JSON"),
            ] {
                assert!(
                    evaluate_assertion(&expanded, 0, &stdout, "").is_err(),
                    "{distro} {test_id} must reject {defect}: {stdout}"
                );
            }
            assert!(
                evaluate_assertion(&expanded, 1, good, "").is_err(),
                "{distro} {test_id} must reject a nonzero command exit"
            );
        }
    }
}

fn assert_count_step_position(
    test_id: &str,
    steps: &[crate::config::manifest::TestStep],
    step_index: usize,
) {
    match test_id {
        "TNPM06" => {
            assert_eq!(steps.len(), 5);
            assert_eq!(
                steps[0].conary.as_deref(),
                Some("pin phase4-runtime-fixture")
            );
            assert!(
                steps[1]
                    .run
                    .as_deref()
                    .is_some_and(|command| command.contains("remove phase4-runtime-fixture"))
            );
            assert!(steps[2]
                .run
                .as_deref()
                .is_some_and(|command| command.contains("--expect-sha256 /usr/bin/phase4-runtime-fixture=517631de24336343a6aaf1a8f704d326299c14c619fe8c8d75d17824d074bd7f")));
            assert_eq!(step_index, 3);
            assert_eq!(
                steps[4].conary.as_deref(),
                Some("query whatbreaks phase4-runtime-fixture")
            );
        }
        "TNPM08" => {
            assert_eq!(steps.len(), 4);
            assert!(
                steps[0]
                    .run
                    .as_deref()
                    .is_some_and(|command| command.contains("remove phase4-runtime-fixture"))
            );
            assert!(steps[1].run.as_deref().is_some_and(|command| {
                command.contains("--absent /usr/bin/phase4-runtime-fixture")
            }));
            assert_eq!(step_index, 2);
            assert_eq!(steps[3].conary.as_deref(), Some("system history"));
        }
        _ => panic!("unexpected test id {test_id}"),
    }
}

fn count_documents(expected: i64) -> (&'static str, String, String, &'static str) {
    match expected {
        1 => (
            r#"[{"installed":1}]"#,
            r#"[{"installed":0}]"#.to_owned(),
            r#"[{"installed":11}]"#.to_owned(),
            "1 installed",
        ),
        0 => (
            r#"[{"installed":0}]"#,
            r#"[{"installed":1}]"#.to_owned(),
            r#"[{"installed":10}]"#.to_owned(),
            "0 installed",
        ),
        other => panic!("unexpected expected count {other}"),
    }
}

fn string_count(count: i64) -> String {
    format!(r#"[{{"installed":"{count}"}}]"#)
}

fn float_count(count: i64) -> String {
    format!(r#"[{{"installed":{count}.0}}]"#)
}

fn null_count() -> String {
    r#"[{"installed":null}]"#.to_owned()
}

fn extra_row(count: i64) -> String {
    format!(r#"[{{"installed":{count}}},{{"installed":{count}}}]"#)
}

fn extra_key(count: i64) -> String {
    format!(r#"[{{"installed":{count},"unexpected":true}}]"#)
}

fn concatenated_documents(count: i64) -> String {
    format!(r#"[{{"installed":{count}}}][{{"installed":{count}}}]"#)
}
