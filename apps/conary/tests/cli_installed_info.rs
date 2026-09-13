// apps/conary/tests/cli_installed_info.rs

#![cfg(test)]
//! Installed detail frames retain selected facts and fail before partial output.

pub mod common;

use conary_core::db::{
    self,
    models::{ProvideEntry, Trove, TroveType},
};
use conary_core::repository::dependency_model::{
    CapabilityProvenance, ProvideArchitectureQualifier, ProvideVersionRelation, ProvidedCapability,
    RepositoryCapabilityKind, SourcePackageFormat,
};
use conary_core::repository::versioning::VersionScheme;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn tested_binary() -> PathBuf {
    std::env::var_os("CONARY_INFO_TEST_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_conary")))
}

fn capture(binary: &Path, db_path: &str, package: &str, tty: bool, no_color: bool) -> Output {
    let mut command = if tty {
        let mut command = Command::new("script");
        command.args(["-qec", "exec \"$CONARY_INFO_EXE\" list \"$CONARY_INFO_PACKAGE\" --info --db-path \"$CONARY_INFO_DB\"", "/dev/null"]);
        command
            .env("CONARY_INFO_EXE", binary)
            .env("CONARY_INFO_PACKAGE", package)
            .env("CONARY_INFO_DB", db_path);
        command
    } else {
        let mut command = Command::new(binary);
        command.args(["list", package, "--info", "--db-path", db_path]);
        command
    };
    let root = Path::new(db_path).parent().unwrap();
    command
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("TERM", "xterm")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE");
    if no_color {
        command.env("NO_COLOR", "1");
    }
    command
        .output()
        .expect("capture installed info (requires util-linux script)")
}

fn frame(output: &Output, tty: bool, no_color: bool) -> String {
    assert!(output.status.success(), "{output:?}");
    let raw = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .replace("\r\n", "\n");
    if tty && !no_color {
        assert!(
            raw.contains('\x1b'),
            "terminal styling not exercised: {raw:?}"
        );
    } else {
        assert!(!raw.contains('\x1b'), "unexpected terminal escape: {raw:?}");
    }
    console::strip_ansi_codes(&raw).into_owned()
}

fn retain(name: &str, text: &str, db_path: &str) {
    if let Ok(directory) = std::env::var("CONARY_INFO_CAPTURE_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        let root = Path::new(db_path).parent().unwrap().to_str().unwrap();
        std::fs::write(
            PathBuf::from(directory).join(name),
            text.replace(root, "<fixture>"),
        )
        .unwrap();
    }
}

#[test]
fn selected_package_facts_survive_terminal_pipe_and_no_color_without_mutation() {
    let (_temp, db_path) = common::setup_command_test_db();
    let conn = db::open(&db_path).unwrap();
    conn.execute("INSERT INTO repositories (name, url, enabled, priority) VALUES ('recorded-repository', 'https://repository.example.test', 1, 10)", []).unwrap();
    let repository = conn.last_insert_rowid();
    conn.execute("UPDATE troves SET package_release = '7', source_profile = 'fedora-44', version_scheme = 'rpm', installed_from_repository_id = ?1, install_source = 'repository', installed_at = '2026-01-01 00:00:00', selection_reason = 'explicitly selected fixture', pinned = 1 WHERE name = 'nginx'", [repository]).unwrap();
    conn.execute(
        "UPDATE components SET is_installed = 0 WHERE name = 'config'",
        [],
    )
    .unwrap();
    drop(conn);
    let before = common::database_snapshot(&db_path);
    if let Some(baseline) = std::env::var_os("CONARY_INFO_BASELINE_BIN") {
        let output = capture(Path::new(&baseline), &db_path, "nginx", false, true);
        assert!(output.status.success(), "{output:?}");
        retain(
            "installed-info-before.txt",
            &String::from_utf8_lossy(&output.stdout),
            &db_path,
        );
        assert_eq!(common::database_snapshot(&db_path), before);
    }
    let mut reference = None;
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let text = frame(
            &capture(&tested_binary(), &db_path, "nginx", tty, no_color),
            tty,
            no_color,
        );
        if let Some(reference) = &reference {
            assert_eq!(&text, reference);
        }
        for expected in [
            "Installed package:\n  Name: nginx\n  Version: 1.24.0\n  CCS release: 7\n  Type: package\n",
            "  Authority: conary-owned\n  Install source: repository\n  Source profile: fedora-44\n",
            "  Version scheme: rpm\n  Repository: recorded-repository\n  Architecture: x86_64\n",
            "  Description: High performance web server\n  Installed: 2026-01-01 00:00:00\n",
            "  Selection reason: explicitly selected fixture\n  Install reason: explicit\n  Pinned: yes\n",
            "  File records: 6\n  Payload size: 1026048 bytes\n",
            "Dependencies (1):\n",
            "Provides (2):\n  Capability: nginx\n  Kind: package\n  Capability version: 1.24.0\n  Capability version relation: =\n  Capability version scheme: conary\n  Architecture qualifier: implicit\n  Provenance: exact-identity\n\n  Capability: webserver\n  Kind: package\n  Capability version: -\n  Capability version relation: -\n  Capability version scheme: conary\n  Architecture qualifier: implicit\n  Provenance: exact-identity\n",
            "Components (2):\n  Component: :config\n  Installed: no\n  Component: :runtime\n  Installed: yes\n",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        let conn = db::open(&db_path).unwrap();
        let id = Trove::find_by_name(&conn, "nginx").unwrap()[0].id.unwrap();
        for dependency in
            conary_core::db::models::InstalledRequirementAtom::find_by_trove(&conn, id).unwrap()
        {
            assert!(text.contains(&format!("  {}\n", dependency.to_typed_string())));
        }
        drop(conn);
        assert_eq!(common::database_snapshot(&db_path), before);
        retain(
            &format!("installed-info-tty-{tty}-no-color-{no_color}.txt"),
            &text,
            &db_path,
        );
        reference = Some(text);
    }
}

#[test]
fn missing_observations_and_recorded_controls_remain_explicit() {
    let (_temp, db_path, conn) = common::create_test_db();
    let mut trove = Trove::new(
        "minimal-info".into(),
        "1.0.0".into(),
        TroveType::Package,
        VersionScheme::Conary,
    );
    trove.description = Some("line one\nforged\u{1b}[31m".into());
    trove.selection_reason = None;
    trove.insert(&conn).unwrap();
    drop(conn);
    let before = common::database_snapshot(&db_path);
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let text = frame(
            &capture(&tested_binary(), &db_path, "minimal-info", tty, no_color),
            tty,
            no_color,
        );
        for expected in [
            "  CCS release: -\n",
            "  Description: line one\\nforged\\u{1b}[31m\n",
            "  File records: 0\n  Payload size: 0 bytes\n",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        for absent in [
            "  Architecture:",
            "  Repository:",
            "  Source profile:",
            "  Selection reason:",
            "Dependencies (",
            "Provides (",
            "Components (",
        ] {
            assert!(
                !text.contains(absent),
                "invented observation {absent}: {text}"
            );
        }
        assert_eq!(common::database_snapshot(&db_path), before);
    }
}

#[test]
fn late_component_read_failure_emits_no_partial_detail_frame() {
    let (_temp, db_path) = common::setup_command_test_db();
    let conn = db::open(&db_path).unwrap();
    // Keep the current schema while simulating a malformed persisted component value.
    conn.execute_batch("PRAGMA ignore_check_constraints = ON; UPDATE components SET is_installed = 'invalid-boolean' WHERE name = 'config';").unwrap();
    drop(conn);
    let before = common::database_snapshot(&db_path);
    let output = capture(&tested_binary(), &db_path, "nginx", false, true);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is_installed"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "partial frame: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(common::database_snapshot(&db_path), before);
}

#[test]
fn provide_contracts_keep_versions_qualifiers_and_provenance_in_every_output_mode() {
    let (_temp, db_path) = common::setup_command_test_db();
    let conn = db::open(&db_path).unwrap();
    let id = Trove::find_by_name(&conn, "nginx").unwrap()[0].id.unwrap();
    ProvideEntry::delete_by_trove(&conn, id).unwrap();
    let provides = [
        ProvidedCapability {
            kind: RepositoryCapabilityKind::PackageName,
            name: "nginx".into(),
            version: Some("1.24.0".into()),
            version_relation: Some(ProvideVersionRelation::Equal),
            version_scheme: VersionScheme::Conary,
            architecture_qualifier: ProvideArchitectureQualifier::Implicit,
            provenance: CapabilityProvenance::ExactIdentity,
        },
        ProvidedCapability {
            kind: RepositoryCapabilityKind::Virtual,
            name: "abi-virtual".into(),
            version: Some("2:1.0-3".into()),
            version_relation: Some(ProvideVersionRelation::Equal),
            version_scheme: VersionScheme::Debian,
            architecture_qualifier: ProvideArchitectureQualifier::Exact("native".into()),
            provenance: CapabilityProvenance::SourceDeclared {
                format: SourcePackageFormat::Debian,
                record_index: 7,
            },
        },
        ProvidedCapability {
            kind: RepositoryCapabilityKind::Virtual,
            name: "wildcard-abi".into(),
            version: None,
            version_relation: None,
            version_scheme: VersionScheme::Debian,
            architecture_qualifier: ProvideArchitectureQualifier::Any,
            provenance: CapabilityProvenance::SourceDeclared {
                format: SourcePackageFormat::Debian,
                record_index: 0,
            },
        },
        ProvidedCapability::payload_file(SourcePackageFormat::Rpm, "/usr/bin/fixture"),
        ProvidedCapability::promised_path(SourcePackageFormat::Rpm, "/run/fixture"),
        ProvidedCapability {
            kind: RepositoryCapabilityKind::Generic,
            name: "control\ncap\u{1b}[31m".into(),
            version: Some("2:3.0~rc1-4".into()),
            version_relation: Some(ProvideVersionRelation::GreaterOrEqual),
            version_scheme: VersionScheme::Rpm,
            architecture_qualifier: ProvideArchitectureQualifier::Implicit,
            provenance: CapabilityProvenance::AuthorDeclared,
        },
    ];
    for provide in &provides {
        provide.validate().unwrap();
        ProvideEntry::from_declared(id, provide)
            .insert(&conn)
            .unwrap();
    }
    drop(conn);
    let before = common::database_snapshot(&db_path);
    let mut reference = None;
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let text = frame(
            &capture(&tested_binary(), &db_path, "nginx", tty, no_color),
            tty,
            no_color,
        );
        if let Some(reference) = &reference {
            assert_eq!(&text, reference);
        }
        assert!(text.contains("Provides (6):\n"));
        for expected in [
            "  Capability: nginx\n  Kind: package\n  Capability version: 1.24.0\n  Capability version relation: =\n  Capability version scheme: conary\n  Architecture qualifier: implicit\n  Provenance: exact-identity\n",
            "  Capability: abi-virtual\n  Kind: virtual\n  Capability version: 2:1.0-3\n  Capability version relation: =\n  Capability version scheme: debian\n  Architecture qualifier: exact\n  Capability architecture: native\n  Provenance: source-declared\n  Source format: deb\n  Source record index: 7\n",
            "  Capability: wildcard-abi\n  Kind: virtual\n  Capability version: -\n  Capability version relation: -\n  Capability version scheme: debian\n  Architecture qualifier: any\n  Provenance: source-declared\n  Source format: deb\n  Source record index: 0\n",
            "  Capability: /usr/bin/fixture\n  Kind: file\n  Capability version: -\n  Capability version relation: -\n  Capability version scheme: rpm\n  Architecture qualifier: implicit\n  Provenance: source-derived-file\n  Source format: rpm\n",
            "  Capability: /run/fixture\n  Kind: file\n  Capability version: -\n  Capability version relation: -\n  Capability version scheme: rpm\n  Architecture qualifier: implicit\n  Provenance: source-promised-path\n  Source format: rpm\n",
            "  Capability: control\\ncap\\u{1b}[31m\n  Kind: generic\n  Capability version: 2:3.0~rc1-4\n  Capability version relation: >=\n  Capability version scheme: rpm\n  Architecture qualifier: implicit\n  Provenance: author-declared\n",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        assert_eq!(common::database_snapshot(&db_path), before);
        retain(
            &format!("installed-provides-tty-{tty}-no-color-{no_color}.txt"),
            &text,
            &db_path,
        );
        reference = Some(text);
    }
}
