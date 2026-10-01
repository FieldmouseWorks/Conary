// apps/conary-test/src/config/tests/native_corpus/typed_sql.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestDef, TestManifest, TestStep};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use serde_json::{Value, json};

const REGULAR: &str = "json_extract(payload_node_json, '$.source.kind.type') = 'regular'";
const TROVE: &str = "trove_id = (SELECT id FROM troves WHERE name = 'phase4-runtime-fixture')";
const ERROR: &str = "forced generation rebuild failure for test: slice-d-forced";
mod fixtures;
use fixtures::{FIDELITY, LANES, Lane, VERSION, expected_documents};

#[test]
fn native_parity_sql_requires_exact_typed_rows_for_all_distro_lanes() {
    let manifest = load_manifest(&remi_manifest_path("phase4-native-pm-parity.toml")).unwrap();
    let metadata = get_test(&manifest, "TNPM04");
    let deferred = get_test(&manifest, "TNPM09");
    assert_eq!(metadata.step.len(), 14);
    assert!(deferred.step.len() >= 3);
    let publication = &deferred.step[2];
    for step in metadata.step.iter().chain(std::iter::once(publication)) {
        assert_typed_read(step);
    }

    // Keep the original table sequence and the predicates that define the rows.
    for (steps, table) in [
        (0..2, "FROM troves WHERE name"),
        (2..4, "FROM files WHERE"),
        (4..8, "FROM provides WHERE"),
        (8..10, "FROM package_requirement_groups WHERE"),
        (10..12, "FROM config_files WHERE"),
        (12..14, "FROM installed_native_lifecycle_bundles WHERE"),
    ] {
        assert!(
            metadata.step[steps]
                .iter()
                .all(|step| step.run.as_deref().unwrap().contains(table))
        );
    }
    for step in &metadata.step[2..] {
        assert!(step.run.as_deref().unwrap().contains(TROVE));
    }
    for index in [2, 3] {
        assert!(
            metadata.step[index]
                .run
                .as_deref()
                .unwrap()
                .contains(REGULAR)
        );
    }
    for (index, order) in [
        (3, "ORDER BY path"),
        (7, "ORDER BY capability"),
        (9, "ORDER BY id"),
        (11, "ORDER BY path"),
    ] {
        assert!(metadata.step[index].run.as_deref().unwrap().contains(order));
    }
    assert!(
        metadata.step[3]
            .run
            .as_deref()
            .unwrap()
            .contains("COALESCE(content_sha256, '-')")
    );
    assert!(
        metadata.step[3]
            .run
            .as_deref()
            .unwrap()
            .contains("COALESCE(content_size, '-')")
    );
    assert!(
        metadata.step[4]
            .run
            .as_deref()
            .unwrap()
            .contains("${native_provider_count}")
    );
    let package_providers = metadata.step[6].run.as_deref().unwrap();
    assert!(
        package_providers.contains("COUNT(*) - ${native_package_provider_count} AS count_delta")
    );
    assert!(package_providers.contains("AND kind = 'package'"));
    assert!(
        package_providers
            .contains("GROUP BY capability, COALESCE(version, ''), COALESCE(kind, '')")
    );
    assert!(
        metadata.step[8]
            .run
            .as_deref()
            .unwrap()
            .contains("${native_dependency_count}")
    );
    let publication_sql = publication.run.as_deref().unwrap();
    assert!(
        publication_sql.contains("generation_publications")
            && publication_sql.contains("ORDER BY id DESC LIMIT 1")
    );
    assert!(
        publication_sql
            .contains("description = 'Install phase4-runtime-fixture-${native_fixture_version}'")
    );

    for lane in LANES {
        let vars = &manifest.distro_overrides[lane.distro];
        for (key, expected) in [
            ("native_target", lane.target),
            ("native_arch", lane.arch),
            ("native_scheme", lane.scheme),
            ("native_profile", lane.profile),
            ("native_fixture_version", VERSION),
            ("native_dependency_count", lane.dependency_count),
            ("native_dependency_probe", lane.dependency_probe),
            ("native_config_source", lane.config_source),
            ("native_lifecycle_fidelity", FIDELITY),
            ("native_provider_count", lane.provider_count),
            ("native_package_provider_count", lane.package_provider_count),
        ] {
            assert_eq!(
                vars.get(key).map(String::as_str),
                Some(expected),
                "{} {key}",
                lane.distro
            );
        }
        let checks = metadata
            .step
            .iter()
            .chain(std::iter::once(publication))
            .map(|step| expand_assertion(step.assert.as_ref().unwrap(), vars))
            .collect::<Vec<_>>();
        let documents = expected_documents(lane);
        assert_eq!(documents.len(), 15);
        for (check, document) in checks.iter().zip(&documents) {
            assert!(
                evaluate_assertion(check, 0, &document.to_string(), "").is_ok(),
                "{} rejects {document}",
                lane.distro
            );
        }
        reject_mutations(&checks, lane);
    }
    let old_count = Assertion {
        stdout_contains: Some("1 troves".into()),
        ..Assertion::default()
    };
    assert!(evaluate_assertion(&old_count, 0, "11 troves", "").is_ok());
    let old_recovery = Assertion {
        stdout_contains: Some(format!("failed|{ERROR}|1|1")),
        ..Assertion::default()
    };
    assert!(evaluate_assertion(&old_recovery, 0, &format!("failed|{ERROR}|1|10"), "").is_ok());
}

fn get_test<'a>(manifest: &'a TestManifest, id: &str) -> &'a TestDef {
    manifest
        .test
        .iter()
        .find(|test| test.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
}

fn assert_typed_read(step: &TestStep) {
    let command = step.run.as_deref().unwrap();
    assert!(command.starts_with("sqlite3 -json ${DB_PATH} \"SELECT ") && !command.contains(';'));
    let assertion = step.assert.as_ref().unwrap();
    assert_eq!(assertion.exit_code, Some(0));
    assert!(
        ![
            assertion.stdout_contains.is_some(),
            assertion.stdout_not_contains.is_some(),
            assertion.stdout_contains_all.is_some(),
            assertion.stdout_contains_any.is_some(),
            assertion.stdout_contains_if_success.is_some(),
            assertion.stdout_contains_any_if_success.is_some(),
            assertion.stderr_contains.is_some(),
            assertion.stderr_not_contains.is_some(),
        ]
        .into_iter()
        .any(|uses_text| uses_text)
    );
    let checks = assertion.stdout_json.as_ref().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assert!(matches!(&checks[0].expected, JsonExpectation::Equals(_)));
}

fn rejects(assertion: &Assertion, actual: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err(),
        "must reject {defect}"
    );
}

fn reject_mutations(checks: &[Assertion], lane: &Lane) {
    // Each control starts from fresh literal documents, never another test's mutation.
    let controls = [
        (0, "/0/troves_count", json!(11)),
        (0, "/0/troves_count", json!("1")),
        (0, "/0/troves_count", json!(1.0)),
        (1, "/0/name", json!("wrong-package")),
        (1, "/0/version", json!("9.9.9")),
        (1, "/0/source_profile", Value::Null),
        (2, "/0/regular_files", json!(4)),
        (3, "/0/content_sha256", json!("wrong-hash")),
        (3, "/0/content_size", json!("61")),
        (3, "/0/content_size", json!(61.0)),
        (3, "/0/mode", json!("33188")),
        (3, "/0/mode", json!(33188.0)),
        (4, "/0/count_delta", json!(10)),
        (4, "/0/count_delta", json!("0")),
        (4, "/0/count_delta", json!(0.0)),
        (5, "/0/file_provides", json!(4)),
        (6, "/0/count_delta", json!(-1)),
        (6, "/0/count_delta", json!(1)),
        (6, "/0/count_delta", json!("0")),
        (6, "/0/count_delta", json!(0.0)),
        (6, "/0/capability", json!("wrong-package")),
        (6, "/0/version", json!("9.9.9")),
        (6, "/0/kind", json!("wrong-kind")),
        (8, "/0/count_delta", json!(1)),
        (8, "/0/count_delta", json!("0")),
        (8, "/0/count_delta", json!(0.0)),
        (9, "/0/dependency_probe", json!("changed")),
        (9, "/0/dependency_probe", Value::Null),
        (10, "/0/config_rows", json!(2)),
        (11, "/0/source", json!("wrong-format")),
        (11, "/0/current_hash", Value::Null),
        (12, "/0/lifecycle_bundles", json!(2)),
        (13, "/0/source_format", json!("wrong-format")),
        (14, "/0/status", json!("applied")),
        (14, "/0/last_error", json!("different failure")),
        (14, "/0/retry_count", json!("1")),
        (14, "/0/retry_count", json!(1.0)),
        (14, "/0/recoverable", json!(10)),
        (14, "/0/recoverable", json!("1")),
        (14, "/0/recoverable", json!(1.0)),
    ];
    for (step, pointer, replacement) in controls {
        let mut documents = expected_documents(lane);
        *documents[step].pointer_mut(pointer).unwrap() = replacement;
        rejects(&checks[step], &documents[step], pointer);
    }
    let documents = expected_documents(lane);
    for (step, baseline) in documents.iter().enumerate() {
        let mut missing_row = baseline.clone();
        missing_row.as_array_mut().unwrap().pop();
        rejects(&checks[step], &missing_row, "missing row");
        let mut extra_row = baseline.clone();
        let first = extra_row[0].clone();
        extra_row.as_array_mut().unwrap().push(first);
        rejects(&checks[step], &extra_row, "extra row");
        let mut missing_key = baseline.clone();
        let object = missing_key[0].as_object_mut().unwrap();
        let key = object.keys().next().unwrap().clone();
        object.remove(&key).unwrap();
        rejects(&checks[step], &missing_key, "missing key");
        let mut extra_key = baseline.clone();
        extra_key[0]
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!(true));
        rejects(&checks[step], &extra_key, "extra key");
    }
    for step in [3, 7] {
        let mut unordered = documents[step].clone();
        unordered.as_array_mut().unwrap().swap(0, 1);
        rejects(&checks[step], &unordered, "unordered rows");
    }
    for (code, stdout, defect) in [
        (0, "not JSON", "malformed JSON"),
        (
            0,
            "[{\"troves_count\":1}]\n[{\"troves_count\":1}]",
            "concatenated JSON",
        ),
        (1, "[{\"troves_count\":1}]", "nonzero exit"),
    ] {
        assert!(
            evaluate_assertion(&checks[0], code, stdout, "").is_err(),
            "must reject {defect}"
        );
    }
}
