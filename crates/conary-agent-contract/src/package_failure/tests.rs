// crates/conary-agent-contract/src/package_failure/tests.rs

use super::*;
use serde_json::json;

#[test]
fn package_failure_schema_is_closed_and_preserves_all_refusal_facts() {
    let causes = [
        NativePreflightCause::MissingInterpreter {
            interpreter: "/bin/sh\n".into(),
            entry_id: "rpm:%pre".into(),
            projected: true,
        },
        NativePreflightCause::InvalidExecutionRoot {
            root: "relative".into(),
        },
        NativePreflightCause::TimeoutOutOfRange {
            entry_id: "rpm:%pre".into(),
            timeout_ms: 3,
            minimum_ms: 1000,
            maximum_ms: 300000,
        },
        NativePreflightCause::Unclassified {
            causes: vec!["outer".into(), "leaf".into()],
        },
    ];
    for cause in causes {
        let report = PackageFailureReport {
            schema: PackageFailureSchema::V1,
            requested_updates: Some(2),
            committed_changesets: Some(vec![41]),
            failures: vec![PackageFailure {
                package: "requested".into(),
                version: "2".into(),
                causes: vec![],
                native_preflight: Some(NativePreflightFailure {
                    package: "owner".into(),
                    version: "1".into(),
                    architecture: None,
                    source_format: "rpm".into(),
                    requested_root: Some("/selected".into()),
                    database: Some("/state/db".into()),
                    execution_root: "/temporary/materialized".into(),
                    stage: "package-pre-install".into(),
                    program: NativePreflightProgram::BundleEntry {
                        entry_id: "rpm:%pre".into(),
                    },
                    cause,
                    notes: vec!["A scoped next action".into()],
                }),
            }],
        };
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            serde_json::from_value::<PackageFailureReport>(value.clone()).unwrap(),
            report
        );
        for pointer in [
            "",
            "/failures/0",
            "/failures/0/native_preflight",
            "/failures/0/native_preflight/cause",
            "/failures/0/native_preflight/program",
        ] {
            let mut bad = value.clone();
            bad.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("install_authorized".into(), json!(true));
            assert!(
                serde_json::from_value::<PackageFailureReport>(bad).is_err(),
                "{pointer}"
            );
        }
        let mut bad = value.clone();
        bad["schema"] = json!("conary.package.failure.v0");
        assert!(serde_json::from_value::<PackageFailureReport>(bad).is_err());
        let mut bad = value.clone();
        bad["failures"][0]["native_preflight"]["cause"]["kind"] = json!("new_refusal");
        assert!(serde_json::from_value::<PackageFailureReport>(bad).is_err());
        let mut bad = value;
        bad["committed_changesets"] = json!(["41"]);
        assert!(serde_json::from_value::<PackageFailureReport>(bad).is_err());
    }
}

#[test]
fn native_program_schema_preserves_arguments_and_optional_sysusers_source() {
    for program in [
        NativePreflightProgram::Command {
            argv: vec!["/usr/bin/tool".into(), "--".into(), "a b\n".into()],
        },
        NativePreflightProgram::RpmSysusers { source_path: None },
        NativePreflightProgram::RpmSysusers {
            source_path: Some("/usr/lib/sysusers.d/fixture.conf".into()),
        },
    ] {
        let value = serde_json::to_value(&program).unwrap();
        assert_eq!(
            serde_json::from_value::<NativePreflightProgram>(value).unwrap(),
            program
        );
    }
}
