// apps/conary/tests/cli_update_summary.rs

pub mod common;

#[path = "cli_update_summary/selection.rs"]
mod selection;

use conary_core::db::models::{
    CollectionMember, InstallSource, Repository, RepositoryPackage, Trove, TroveType,
};
use std::process::Command;

fn fixture() -> (tempfile::TempDir, String) {
    let (temp, db_path, conn) = common::create_test_db();
    let mut repo = Repository::new(
        "variant-repo".to_string(),
        "https://example.test/variant".to_string(),
    );
    repo.source_profile = Some("solus".to_string());
    let repo_id = repo.insert(&conn).unwrap();

    let mut collection = Trove::new(
        "base".to_string(),
        "1.0.0".to_string(),
        TroveType::Collection,
        conary_core::repository::versioning::VersionScheme::Conary,
    );
    let collection_id = collection.insert(&conn).unwrap();
    for name in ["demo", "tools"] {
        let arch = "x86_64";
        CollectionMember::new(collection_id, name.to_string())
            .insert(&conn)
            .unwrap();
        let mut installed = Trove::new_with_source(
            name.to_string(),
            "1.0-1".to_string(),
            TroveType::Package,
            InstallSource::Repository,
            conary_core::repository::versioning::VersionScheme::Eopkg,
        );
        installed.architecture = Some(arch.to_string());
        installed.native_package_identity = Some(
            conary_core::packages::InstalledPackageIdentity::eopkg(name, name, "1.0", 1, arch)
                .unwrap(),
        );
        installed.source_profile = Some("solus".to_string());
        installed.installed_from_repository_id = Some(repo_id);
        installed.insert(&conn).unwrap();

        let mut candidate = RepositoryPackage::new(
            repo_id,
            name.to_string(),
            "1.0-2".to_string(),
            conary_core::repository::versioning::VersionScheme::Eopkg,
            format!("sha256:{name}-{arch}"),
            123,
            format!("https://example.test/variant/{name}-1.0.1-{arch}.ccs"),
        );
        candidate.architecture = Some(arch.to_string());
        candidate.source_profile = Some("solus".to_string());
        candidate.insert(&conn).unwrap();
    }
    drop(conn);
    (temp, db_path)
}

#[test]
fn collection_preview_never_claims_applied_updates_and_preserves_database() {
    let (_temp, db_path) = fixture();
    let before = common::database_snapshot(&db_path);
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut command = if tty {
            let mut command = Command::new("script");
            command.args(["-qec", "exec \"$CONARY_UPDATE_EXE\" update @base --dry-run --db-path \"$CONARY_UPDATE_DB\"", "/dev/null"])
                .env("CONARY_UPDATE_EXE", env!("CARGO_BIN_EXE_conary"))
                .env("CONARY_UPDATE_DB", &db_path);
            command
        } else {
            let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
            command.args(["update", "@base", "--dry-run", "--db-path", &db_path]);
            command
        };
        command
            .env_remove("RUST_LOG")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env("TERM", "xterm");
        if no_color {
            command.env("NO_COLOR", "1");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let text = String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n");
        if no_color || !tty {
            assert!(!text.contains('\u{1b}'), "{text}");
        }
        let text = console::strip_ansi_codes(&text);
        assert!(!text.contains("Updated:"), "{text}");
        assert!(!text.contains("Collection update complete"), "{text}");
        assert!(text.contains("Collection update preview"), "{text}");
        assert!(text.contains("  Planned packages: 2\n"), "{text}");
        for name in ["demo", "tools"] {
            assert!(text.contains(&format!("{name} 1.0-1 [x86_64]")), "{text}");
        }
        assert!(
            text.ends_with("Dry run: no updates were applied.\n"),
            "{text}"
        );
        assert_eq!(common::database_snapshot(&db_path), before);
    }
}
