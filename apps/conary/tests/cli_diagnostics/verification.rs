// apps/conary/tests/cli_diagnostics/verification.rs

use std::process::Command;

pub(super) fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    conary_core::ccs::SigningKeyPair,
) {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let destination = temp.path().join("out");
    std::fs::create_dir_all(project.join("bin")).unwrap();
    std::fs::write(project.join("bin/hello"), "hello\n").unwrap();
    let private = temp.path().join("key.private");
    let public = temp.path().join("key.public");
    let policy = temp.path().join("policy.toml");
    let signer = conary_core::ccs::SigningKeyPair::generate().with_key_id("fixture-signer");
    signer.save_to_files(&private, &public).unwrap();
    let other = conary_core::ccs::SigningKeyPair::generate();
    std::fs::write(
        &policy,
        format!("trusted_keys = [\"{}\"]\n", other.public_key_base64()),
    )
    .unwrap();
    for args in [
        vec![
            "ccs",
            "init",
            project.to_str().unwrap(),
            "--template",
            "minimal-file",
            "--name",
            "diagnostic",
            "--version",
            "1.0.0",
        ],
        vec![
            "ccs",
            "build",
            project.to_str().unwrap(),
            "--key",
            private.to_str().unwrap(),
            "--output",
            destination.to_str().unwrap(),
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_conary"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let package = destination.join("diagnostic-1.0.0-1.ccs");
    (temp, package, policy, signer)
}

#[test]
fn untrusted_archive_names_its_path_and_claimed_signer_once() {
    let (_temp, package, policy, signer) = fixture();
    let original = std::fs::read(&package).unwrap();
    let expected = format!(
        "error: CCS package signer is not trusted.\n  Package: {}\n  Claimed key ID: fixture-signer\n  Public key: {}\nnote: Verify the signing key through a trusted source and use a policy that authorizes this package.\n",
        package.display(),
        signer.public_key_base64()
    );
    for (tty, no_color) in [(false, true), (true, false), (true, true)] {
        let mut command = if tty {
            let mut c = Command::new("script");
            c.args(["-qec", "exec \"$CONARY_DIAGNOSTIC_EXE\" ccs verify \"$CONARY_DIAGNOSTIC_PACKAGE\" --policy \"$CONARY_DIAGNOSTIC_POLICY\"", "/dev/null"])
                .env("CONARY_DIAGNOSTIC_EXE", env!("CARGO_BIN_EXE_conary"))
                .env("CONARY_DIAGNOSTIC_PACKAGE", &package)
                .env("CONARY_DIAGNOSTIC_POLICY", &policy);
            c
        } else {
            let mut c = Command::new(env!("CARGO_BIN_EXE_conary"));
            c.args(["ccs", "verify"])
                .arg(&package)
                .arg("--policy")
                .arg(&policy);
            c
        };
        command
            .env("TERM", "xterm")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("RUST_LOG");
        if no_color {
            command.env("NO_COLOR", "1");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let text = if tty {
            assert!(output.stderr.is_empty());
            String::from_utf8(output.stdout).unwrap()
        } else {
            assert!(output.stdout.is_empty(), "{output:?}");
            String::from_utf8(output.stderr).unwrap()
        };
        assert_eq!(text.contains('\x1b'), tty && !no_color, "{text:?}");
        assert_eq!(
            console::strip_ansi_codes(&text).replace("\r\n", "\n"),
            expected
        );
        assert_eq!(std::fs::read(&package).unwrap(), original);
    }
    std::fs::write(
        &policy,
        format!("trusted_keys = [\"{}\"]\n", signer.public_key_base64()),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_conary"))
        .args(["ccs", "verify"])
        .arg(&package)
        .arg("--policy")
        .arg(&policy)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Signature: valid key=fixture-signer")
    );
}
