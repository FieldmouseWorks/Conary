// crates/conary-core/src/source_root/tests/classification.rs

use super::super::registry::root_directory_defect;
use super::super::*;
use super::{fresh_registry, name, set_mode};
use std::collections::BTreeMap;
use std::path::Path;

/// Classify every registry entry by directory name.
fn classified(
    registry: &SourceRootRegistry,
) -> BTreeMap<String, Result<SourceRootName, SourceRootNonAuthority>> {
    registry
        .list()
        .unwrap()
        .into_iter()
        .map(|entry| match entry {
            SourceRootEntry::Root(root) => (root.name().to_string(), Ok(root.name().clone())),
            SourceRootEntry::NonAuthority { directory, reason } => (
                directory
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                Err(reason),
            ),
        })
        .collect()
}

fn private_dir(path: &Path) {
    std::fs::create_dir(path).unwrap();
    set_mode(path, SOURCE_ROOT_DIR_MODE);
}

fn mutate_root_db(registry: &SourceRootRegistry, root: &str, sql: &str) {
    let conn = crate::db::open(registry.base().join(root).join(SOURCE_ROOT_DB_FILE)).unwrap();
    conn.execute_batch(sql).unwrap();
}

#[test]
fn every_defect_is_typed_non_authority_beside_a_valid_root() {
    let (_temp, registry) = fresh_registry();
    let base = registry.base().to_path_buf();
    // Positive control.
    let arch = registry.create(&name("arch")).unwrap();

    // Pin/name mismatch: a root moved to another profile's name.
    registry.create(&name("fedora-44")).unwrap();
    std::fs::rename(base.join("fedora-44"), base.join("ubuntu-26.04")).unwrap();

    // Rebuild-required schema.
    registry.create(&name("old-schema")).unwrap();
    mutate_root_db(
        &registry,
        "old-schema",
        "UPDATE schema_identity SET revision = 57",
    );

    // Wrong permissions.
    registry.create(&name("loose")).unwrap();
    set_mode(&base.join("loose"), 0o755);

    // Malformed pin: only reachable by removing the schema's own guards.
    registry.create(&name("malformed")).unwrap();
    mutate_root_db(
        &registry,
        "malformed",
        "DROP TRIGGER source_root_identity_immutable_update;
         PRAGMA ignore_check_constraints = ON;
         UPDATE source_root_identity SET identity = 'Not Valid';",
    );

    // Missing pin: a current-schema database that never recorded one.
    private_dir(&base.join("no-pin"));
    crate::db::init(base.join("no-pin").join(SOURCE_ROOT_DB_FILE)).unwrap();

    private_dir(&base.join("no-db"));
    private_dir(&base.join("empty-db"));
    std::fs::write(base.join("empty-db").join(SOURCE_ROOT_DB_FILE), b"").unwrap();
    private_dir(&base.join("garbage-db"));
    std::fs::write(
        base.join("garbage-db").join(SOURCE_ROOT_DB_FILE),
        b"this is not an SQLite database, only some bytes",
    )
    .unwrap();
    private_dir(&base.join("db-dir"));
    std::fs::create_dir(base.join("db-dir").join(SOURCE_ROOT_DB_FILE)).unwrap();

    std::fs::write(base.join("plain-file"), b"").unwrap();
    std::os::unix::fs::symlink(base.join("arch"), base.join("linked")).unwrap();
    private_dir(&base.join("Bad Name"));
    private_dir(&base.join(".creating-interrupted"));

    let entries = classified(&registry);
    let expect = |dir: &str| {
        entries
            .get(dir)
            .unwrap_or_else(|| panic!("{dir} not listed"))
    };

    assert_eq!(expect("arch"), &Ok(name("arch")));
    assert_eq!(
        expect("ubuntu-26.04"),
        &Err(SourceRootNonAuthority::PinMismatch {
            pinned: name("fedora-44")
        })
    );
    assert!(matches!(
        expect("old-schema"),
        Err(SourceRootNonAuthority::SchemaRebuildRequired { observed })
            if observed == "schema epoch conary-current-v1 revision 57"
    ));
    assert_eq!(
        expect("loose"),
        &Err(SourceRootNonAuthority::UnsafePermissions { mode: 0o755 })
    );
    assert!(matches!(
        expect("malformed"),
        Err(SourceRootNonAuthority::MalformedPin { identity, error: SourceRootNameError::InvalidFirstByte { found: 'N' } })
            if identity == "Not Valid"
    ));
    assert_eq!(expect("no-pin"), &Err(SourceRootNonAuthority::MissingPin));
    assert_eq!(
        expect("no-db"),
        &Err(SourceRootNonAuthority::MissingDatabase)
    );
    assert_eq!(
        expect("empty-db"),
        &Err(SourceRootNonAuthority::UninitializedDatabase)
    );
    assert!(matches!(
        expect("garbage-db"),
        Err(SourceRootNonAuthority::UnreadableDatabase { .. })
    ));
    assert_eq!(
        expect("db-dir"),
        &Err(SourceRootNonAuthority::DatabaseNotRegularFile)
    );
    assert_eq!(
        expect("plain-file"),
        &Err(SourceRootNonAuthority::NotADirectory)
    );
    assert_eq!(
        expect("linked"),
        &Err(SourceRootNonAuthority::NotADirectory)
    );
    assert_eq!(
        expect("Bad Name"),
        &Err(SourceRootNonAuthority::InvalidName(
            SourceRootNameError::InvalidFirstByte { found: 'B' }
        ))
    );
    assert_eq!(
        expect(".creating-interrupted"),
        &Err(SourceRootNonAuthority::InvalidName(
            SourceRootNameError::InvalidFirstByte { found: '.' }
        ))
    );
    assert_eq!(entries.len(), 14, "{entries:#?}");

    // `open` refuses the same states with the same typed reason.
    for (dir, expected) in [
        (
            "ubuntu-26.04",
            SourceRootNonAuthority::PinMismatch {
                pinned: name("fedora-44"),
            },
        ),
        (
            "loose",
            SourceRootNonAuthority::UnsafePermissions { mode: 0o755 },
        ),
        ("no-pin", SourceRootNonAuthority::MissingPin),
        ("no-db", SourceRootNonAuthority::MissingDatabase),
        ("linked", SourceRootNonAuthority::NotADirectory),
    ] {
        match registry.open(&name(dir)) {
            Err(SourceRootError::NonAuthority { name: got, reason }) => {
                assert_eq!(got, name(dir));
                assert_eq!(reason, expected, "{dir}");
            }
            other => panic!("{dir}: expected non-authority, got {other:?}"),
        }
    }
    assert_eq!(registry.open(&name("arch")).unwrap(), arch);

    // Listing and refusing never repaired or rewrote anything.
    assert_eq!(
        crate::db::schema::inspect(base.join("old-schema").join(SOURCE_ROOT_DB_FILE)).unwrap(),
        crate::db::schema::SchemaCompatibility::RebuildRequired {
            observed: "schema epoch conary-current-v1 revision 57".to_string()
        }
    );
    assert_eq!(super::mode_of(&base.join("loose")), 0o755);
}

#[test]
fn root_directory_defects_are_ordered_and_typed() {
    const OWNER: u32 = 1000;
    // Positive control.
    assert_eq!(root_directory_defect(true, OWNER, 0o040700, OWNER), None);

    assert_eq!(
        root_directory_defect(false, OWNER, 0o120777, OWNER),
        Some(SourceRootNonAuthority::NotADirectory)
    );
    assert_eq!(
        root_directory_defect(true, 0, 0o040700, OWNER),
        Some(SourceRootNonAuthority::WrongOwner {
            uid: 0,
            expected: OWNER
        })
    );
    for mode in [0o040755, 0o040750, 0o040600, 0o042700, 0o041700] {
        assert_eq!(
            root_directory_defect(true, OWNER, mode, OWNER),
            Some(SourceRootNonAuthority::UnsafePermissions {
                mode: mode & 0o7777
            }),
            "mode {mode:o}"
        );
    }
}
