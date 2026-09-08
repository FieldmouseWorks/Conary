// apps/conary/tests/cli_diagnostics/machine.rs

use conary_agent_contract::{CcsVerificationCause, CcsVerificationOutcome, CcsVerificationReport};
use std::path::Path;
use std::process::Command;

fn capture(
    package: &Path,
    policy: Option<&Path>,
    data_home: &Path,
    tty: bool,
    no_color: bool,
) -> (i32, CcsVerificationReport) {
    let mut command = if tty {
        let mut command = Command::new("script");
        command.args(["-qec", "exec \"$CONARY_VERIFICATION_EXE\" ccs verify \"$CONARY_VERIFICATION_PACKAGE\" --policy \"$CONARY_VERIFICATION_POLICY\" --json", "/dev/null"])
            .env("CONARY_VERIFICATION_EXE", env!("CARGO_BIN_EXE_conary"))
            .env("CONARY_VERIFICATION_PACKAGE", package)
            .env("CONARY_VERIFICATION_POLICY", policy.unwrap());
        command
    } else {
        let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
        command.args(["ccs", "verify"]).arg(package).arg("--json");
        if let Some(policy) = policy {
            command.arg("--policy").arg(policy);
        }
        command
    };
    command
        .env("XDG_DATA_HOME", data_home)
        .env("TERM", "xterm")
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE");
    if no_color {
        command.env("NO_COLOR", "1");
    }
    let output = command.output().unwrap();
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(!output.stdout.contains(&0x1b), "{output:?}");
    let mut deserializer = serde_json::Deserializer::from_slice(&output.stdout);
    let report = <CcsVerificationReport as serde::Deserialize>::deserialize(&mut deserializer)
        .unwrap_or_else(|error| panic!("{error}: {output:?}"));
    deserializer
        .end()
        .expect("one object with no trailing human text");
    (output.status.code().unwrap(), report)
}

#[test]
fn json_verification_is_exact_in_terminal_pipe_and_no_color() {
    let (temp, package, policy, signer) = super::verification::fixture();
    let data_home = temp.path().join("unused-local-data");
    let bytes = std::fs::read(&package).unwrap();
    let mut previous = None;
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let (code, report) = capture(&package, Some(&policy), &data_home, tty, no_color);
        assert_eq!(code, 1);
        assert!(!report.is_verified());
        assert_eq!(report.package, package.to_str().unwrap());
        let CcsVerificationOutcome::Failed { failure } = &report.outcome else {
            panic!("expected refusal")
        };
        assert_eq!(
            failure.cause,
            CcsVerificationCause::UntrustedSigner {
                claimed_key_id: Some("fixture-signer".into()),
                public_key: signer.public_key_base64()
            }
        );
        assert_eq!(failure.archive_path.as_deref(), package.to_str());
        assert_eq!(failure.notes.len(), 1);
        if let Some(previous) = &previous {
            assert_eq!(&report, previous);
        }
        previous = Some(report);
    }
    assert_eq!(std::fs::read(&package).unwrap(), bytes);
    assert!(!data_home.exists());
    std::fs::write(
        &policy,
        format!("trusted_keys = [\"{}\"]\n", signer.public_key_base64()),
    )
    .unwrap();
    for (tty, no_color) in [(false, false), (true, false), (true, true)] {
        let (code, report) = capture(&package, Some(&policy), &data_home, tty, no_color);
        assert_eq!(code, 0);
        assert!(report.is_verified());
        let CcsVerificationOutcome::Verified { facts } = report.outcome else {
            panic!("expected verified facts")
        };
        assert_eq!(facts.archive_sha256, conary_core::hash::sha256(&bytes));
        assert_eq!(facts.archive_bytes, bytes.len() as u64);
        assert_eq!(facts.name, "diagnostic");
        assert_eq!(facts.version, "1.0.0");
        assert_eq!(facts.release, "1");
        assert_eq!(facts.version_scheme, "conary");
        assert_eq!(facts.public_key, signer.public_key_base64());
        assert!(facts.files_checked > 0);
    }
}

#[test]
fn json_setup_and_archive_failures_keep_nonzero_status_and_structured_details() {
    let (temp, package, policy, _) = super::verification::fixture();
    let data_home = temp.path().join("unused-local-data");
    let missing = temp.path().join("absent.ccs");
    let (code, report) = capture(&missing, Some(&policy), &data_home, false, true);
    assert_eq!(code, 1);
    let CcsVerificationOutcome::Failed { failure } = report.outcome else {
        panic!("expected failure")
    };
    assert_eq!(
        failure.cause,
        CcsVerificationCause::PackageNotFound {
            path: missing.to_str().unwrap().into()
        }
    );
    let (code, report) = capture(&package, None, &data_home, false, true);
    assert_eq!(code, 1);
    let CcsVerificationOutcome::Failed { failure } = report.outcome else {
        panic!("expected failure")
    };
    assert_eq!(failure.cause, CcsVerificationCause::MissingPolicy);
    assert!(!data_home.exists());
    std::fs::write(&policy, "trusted_keys = []\n").unwrap();
    let (code, report) = capture(&package, Some(&policy), &data_home, false, true);
    assert_eq!(code, 1);
    let CcsVerificationOutcome::Failed { failure } = report.outcome else {
        panic!("expected failure")
    };
    assert_eq!(failure.cause, CcsVerificationCause::NoTrustedKeys);
    assert_eq!(failure.policy_path.as_deref(), policy.to_str());
    std::fs::write(&policy, "not valid TOML !").unwrap();
    let (code, report) = capture(&package, Some(&policy), &data_home, false, true);
    assert_eq!(code, 1);
    let CcsVerificationOutcome::Failed { failure } = report.outcome else {
        panic!("expected failure")
    };
    assert!(
        matches!(failure.cause, CcsVerificationCause::Unclassified { causes } if !causes.is_empty())
    );
    assert_eq!(failure.policy_path.as_deref(), policy.to_str());
    let key = conary_core::ccs::SigningKeyPair::generate();
    std::fs::write(
        &policy,
        format!("trusted_keys = [\"{}\"]\n", key.public_key_base64()),
    )
    .unwrap();
    std::fs::write(&package, b"invalid archive").unwrap();
    let (code, report) = capture(&package, Some(&policy), &data_home, false, true);
    assert_eq!(code, 1);
    assert!(!report.is_verified());
}
