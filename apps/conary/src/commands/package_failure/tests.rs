// apps/conary/src/commands/package_failure/tests.rs

use super::*;

pub(crate) fn refusal(cause: NativeLifecyclePreflightError) -> anyhow::Error {
    with_scope(
        anyhow::Error::new(cause)
            .context(NativePreflightContext {
                package: "event-owner\nforged".into(),
                version: "2".into(),
                architecture: Some("x86_64".into()),
                source_format: "rpm".into(),
                root: "/tmp/materialized".into(),
                stage: NativeEventStage::PackagePreInstall,
                recovery: false,
                program: NativeEventProgram::BundleEntry {
                    entry_id: "rpm:%pre".into(),
                },
            })
            .context("outer context does not replace the cause"),
        "/selected' root",
        "/state' db",
    )
}

#[test]
fn native_refusal_report_retains_typed_cause_and_requested_scope() {
    let error = refusal(NativeLifecyclePreflightError::MissingInterpreter {
        interpreter: "/bin/missing\x1b[31m".into(),
        entry_id: "rpm:%pre".into(),
        projected: true,
    });
    let report = package_failure_report(&error).unwrap();
    let native = report.failures[0].native_preflight.as_ref().unwrap();
    assert_eq!(native.requested_root.as_deref(), Some("/selected' root"));
    assert_eq!(native.database.as_deref(), Some("/state' db"));
    assert_eq!(native.execution_root, "/tmp/materialized");
    assert_eq!(native.package, "event-owner\nforged");
    assert!(
        matches!(&native.cause, NativePreflightCause::MissingInterpreter { interpreter, projected: true, .. } if interpreter == "/bin/missing\x1b[31m")
    );
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(
        serde_json::from_str::<PackageFailureReport>(&json).unwrap(),
        report
    );
    assert!(report.committed_changesets.is_none());
}

#[test]
fn aggregated_failures_keep_each_cause_and_only_observed_commits() {
    use crate::commands::update::failure::UpdatePackageFailure;
    let core = NativeLifecyclePreflightError::TimeoutOutOfRange {
        entry_id: "rpm:%pre".into(),
        timeout_ms: 7,
        minimum_ms: 1000,
        maximum_ms: 300000,
    };
    let error = anyhow::Error::new(UpdateFailures {
        total_requested: 3,
        committed_changesets: vec![17],
        failures: vec![
            UpdatePackageFailure {
                package: "requested".into(),
                version: "3".into(),
                error: refusal(core.clone()),
            },
            UpdatePackageFailure {
                package: "other".into(),
                version: "4".into(),
                error: anyhow::anyhow!("leaf").context("outer"),
            },
        ],
    })
    .context("update wrapper");
    let displayed = error.downcast_ref::<UpdateFailures>().unwrap().to_string();
    for detail in [
        "2 of 3",
        "requested 3",
        "TimeoutOutOfRange",
        "other 4",
        "outer: leaf",
    ] {
        assert!(displayed.contains(detail), "{displayed}");
    }
    let updates = error.downcast_ref::<UpdateFailures>().unwrap();
    assert_eq!(
        updates.failures[0]
            .error
            .downcast_ref::<NativeLifecyclePreflightError>(),
        Some(&core)
    );
    let report = package_failure_report(&error).unwrap();
    assert_eq!(report.committed_changesets, Some(vec![17]));
    assert_eq!(report.requested_updates, Some(3));
    assert_eq!(report.failures[0].package, "requested");
    assert_eq!(
        report.failures[0]
            .native_preflight
            .as_ref()
            .unwrap()
            .package,
        "event-owner\nforged"
    );
    assert_eq!(report.failures[1].causes, ["outer", "leaf"]);
    assert!(report.failures[1].native_preflight.is_none());
}

#[test]
fn text_that_resembles_a_refusal_cannot_establish_typed_facts() {
    let error =
        anyhow::anyhow!("Interpreter not found: /bin/sh. native transaction preflight failed");
    assert!(package_failure_report(&error).is_none());
    let error = error.context(NativePreflightContext {
        package: "owner".into(),
        version: "1".into(),
        architecture: None,
        source_format: "rpm".into(),
        root: "/selected".into(),
        stage: NativeEventStage::PackagePreInstall,
        recovery: false,
        program: NativeEventProgram::BundleEntry {
            entry_id: "rpm:%pre".into(),
        },
    });
    let original_causes = causes(&error);
    let error = with_scope(with_scope(error, "/root", "/db"), "/root", "/db");
    let report = package_failure_report(&error).unwrap();
    let native = report.failures[0].native_preflight.as_ref().unwrap();
    assert_eq!(
        native.cause,
        NativePreflightCause::Unclassified {
            causes: original_causes
        }
    );
    assert!(native.notes.is_empty());
}

#[test]
fn typed_scope_preserves_plain_refusal_through_repeated_context() {
    let error = refusal(NativeLifecyclePreflightError::MissingInterpreter {
        interpreter: "/bin/missing".into(),
        entry_id: "rpm:%pre".into(),
        projected: false,
    });
    let summary = error.to_string();
    for detail in [
        "event-owner",
        "at stage PackagePreInstall",
        "Interpreter not found: /bin/missing",
        "rpm:%pre",
    ] {
        assert!(summary.contains(detail), "{summary}");
    }
    let rescoped = with_scope(error, "/outer-root", "/outer-db");
    assert_eq!(rescoped.to_string(), summary);
    assert_eq!(plain_failure_summary(&rescoped), summary);
    let report = package_failure_report(&rescoped).unwrap();
    assert_eq!(
        report.failures[0]
            .native_preflight
            .as_ref()
            .unwrap()
            .requested_root
            .as_deref(),
        Some("/outer-root")
    );
}
