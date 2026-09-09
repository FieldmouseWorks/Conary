// apps/conary/tests/cli_repository_details.rs
//! Repository detail observations must retain every installed package variant.

pub mod common;

use conary_core::db::models::{
    Repository, RepositoryPackage, RepositoryRequirementGroup, Trove, TroveType,
};
use conary_core::repository::versioning::VersionScheme;
use std::process::Command;

fn fixture() -> (
    tempfile::TempDir,
    String,
    rusqlite::Connection,
    RepositoryPackage,
) {
    let (temp, db, conn) = common::create_test_db();
    let mut repo = Repository::new("detail-source".into(), "https://example.invalid".into());
    let now = chrono::Utc::now().to_rfc3339();
    repo.last_checked_at = Some(now.clone());
    repo.last_published_at = Some(now);
    let repo_id = repo.insert(&conn).unwrap();
    repo.update(&conn).unwrap();
    let mut package = RepositoryPackage::new(
        repo_id,
        "detail-package".into(),
        "2.0-1".into(),
        VersionScheme::Rpm,
        "a".repeat(64),
        17,
        "https://example.invalid/detail.ccs".into(),
    );
    package.package_release = "3".into();
    package.architecture = Some("x86_64".into());
    package.source_profile = Some("fedora-44".into());
    package.insert(&conn).unwrap();
    (temp, db, conn, package)
}

fn installed(
    conn: &rusqlite::Connection,
    version: &str,
    release: Option<&str>,
    arch: Option<&str>,
    kind: TroveType,
) -> i64 {
    let mut trove = Trove::new(
        "detail-package".into(),
        version.into(),
        kind,
        VersionScheme::Rpm,
    );
    trove.package_release = release.map(str::to_owned);
    trove.architecture = arch.map(str::to_owned);
    trove.insert(conn).unwrap()
}

/// Compare all four terminal modes and the complete database, including epochs.
fn capture(db: &str) -> String {
    let before = common::database_snapshot(db);
    let mut frames = Vec::new();
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut command = if tty {
            let mut command = Command::new("script");
            command.args(["-qec", "exec \"$CONARY_DETAIL_EXE\" query repquery --info --db-path \"$CONARY_DETAIL_DB\"", "/dev/null"])
                .env("CONARY_DETAIL_EXE", env!("CARGO_BIN_EXE_conary"))
                .env("CONARY_DETAIL_DB", db);
            command
        } else {
            let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
            command.args(["query", "repquery", "--info", "--db-path", db]);
            command
        };
        // Child-owned environment: inherited test controls cannot alter the query.
        command
            .env_clear()
            .env("CONARY_DETAIL_EXE", env!("CARGO_BIN_EXE_conary"))
            .env("CONARY_DETAIL_DB", db)
            .env("TERM", "xterm");
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        if no_color {
            command.env("NO_COLOR", "1");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let text = String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n");
        if !tty || no_color {
            assert!(!text.contains('\x1b'), "{text:?}");
        }
        frames.push(console::strip_ansi_codes(&text).into_owned());
        assert_eq!(common::database_snapshot(db), before);
    }
    assert!(
        frames.windows(2).all(|pair| pair[0] == pair[1]),
        "{frames:?}"
    );
    frames.remove(0)
}

#[test]
fn details_retain_separate_releases_and_every_installed_package_variant() {
    let (_temp, db, conn, _) = fixture();
    let variants = [
        ("1.0-1", Some("9"), Some("x86_64")),
        ("2.0-1", Some("2"), Some("x86_64")),
        ("2.0-1", Some("3"), Some("aarch64")),
        ("2.0-1", Some("3"), Some("x86_64")),
        ("2.0-1", None, None),
    ];
    let ids: Vec<_> = variants
        .iter()
        .map(|(version, release, arch)| {
            installed(&conn, version, *release, *arch, TroveType::Package)
        })
        .collect();
    installed(&conn, "99-1", None, None, TroveType::Collection);
    let text = capture(&db);
    let (candidate, records) = text
        .split_once("Installed packages with this name:\n")
        .unwrap();
    assert!(
        candidate.contains("  Version: 2.0-1\n  Release: 3\n  Architecture: x86_64\n"),
        "{candidate}"
    );
    assert!(
        candidate.contains("  Repository: detail-source\n"),
        "{candidate}"
    );
    assert!(
        candidate.contains("  Source profile: fedora-44\n"),
        "{candidate}"
    );
    assert!(records.contains("  Installed packages: 5\n"), "{records}");
    assert!(!records.contains("99-1"), "{records}");
    for (id, (version, release, arch)) in ids.iter().zip(variants) {
        let expected = format!(
            "  Trove ID: {id}\n  Version: {version}\n  Release: {}\n  Architecture: {}\n  Version scheme: rpm\n",
            release.unwrap_or("Unspecified"),
            arch.unwrap_or("Unspecified")
        );
        assert!(records.contains(&expected), "{records}");
    }
    assert!(!text.contains("Status"), "{text}");
}

#[test]
fn details_do_not_treat_other_versions_or_non_packages_as_the_candidate() {
    let (_temp, db, conn, _) = fixture();
    installed(
        &conn,
        "1.0-1",
        Some("2"),
        Some("aarch64"),
        TroveType::Package,
    );
    let text = capture(&db);
    assert!(text.contains("  Installed packages: 1\n"), "{text}");
    assert!(
        text.contains("  Version: 1.0-1\n  Release: 2\n  Architecture: aarch64\n"),
        "{text}"
    );
    conn.execute("DELETE FROM troves", []).unwrap();
    installed(
        &conn,
        "2.0-1",
        Some("3"),
        Some("x86_64"),
        TroveType::Collection,
    );
    installed(&conn, "1.0-1", None, None, TroveType::Component);
    let text = capture(&db);
    assert!(
        text.contains("No installed packages with this name.\n  Installed packages: 0\n"),
        "{text}"
    );
}

#[test]
fn details_preserve_absent_metadata_and_empty_requirements() {
    let (_temp, db, conn, _) = fixture();
    conn.execute("UPDATE repository_packages SET architecture = NULL, package_release = '', source_profile = NULL", []).unwrap();
    let text = capture(&db);
    for fact in [
        "  Release: Unspecified\n",
        "  Architecture: Unspecified\n",
        "  Source profile: Unspecified\n",
        "No installed packages with this name.\n",
        "Requirements (0):\nNo requirements recorded in cached metadata.\n",
    ] {
        assert!(text.contains(fact), "{text}");
    }
    assert!(!text.contains("noarch"), "{text}");
}

#[test]
fn details_escape_metadata_and_both_requirement_representations() {
    let (_temp, db, conn, package) = fixture();
    conn.execute(
        "UPDATE repositories SET name = 'source' || char(10) || '[ok] injected'",
        [],
    )
    .unwrap();
    conn.execute("UPDATE repository_packages SET description = 'description' || char(10) || '[ok] injected', download_url = 'https://example.invalid/' || char(27) || '[31m', architecture = 'arch' || char(9) || 'tail'", []).unwrap();
    let mut native = RepositoryRequirementGroup::new(
        package.id.unwrap(),
        "depends".into(),
        "hard".into(),
        "{}".into(),
    );
    native.native_text = Some("library >= 1\n[ok] injected".into());
    native.insert(&conn).unwrap();
    RepositoryRequirementGroup::new(
        package.id.unwrap(),
        "optional".into(),
        "conditional".into(),
        "{\n\"fixture\":true\n}".into(),
    )
    .insert(&conn)
    .unwrap();
    let text = capture(&db);
    for fact in [
        "  Repository: source\\n[ok] injected\n",
        "  Description: description\\n[ok] injected\n",
        "  Architecture: arch\\ttail\n",
        "  URL: https://example.invalid/\\u{1b}[31m\n",
        "Requirements (2):\n",
        "  depends: library >= 1\\n[ok] injected\n",
        "  optional: {\\n\"fixture\":true\\n}\n",
    ] {
        assert!(text.contains(fact), "missing {fact:?}: {text}");
    }
}
