// crates/conary-core/src/db/models/repository_capability/tests.rs

#![cfg(test)]

use super::*;
use crate::db::models::{Repository, RepositoryPackage};
use crate::db::schema;
use rusqlite::Connection;

fn test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
    schema::ensure_current(&conn).unwrap();
    conn
}

fn seed_repo_and_package(conn: &Connection) {
    conn.execute(
        "INSERT INTO repositories (name, url) VALUES ('repo', 'https://example.test')",
        [],
    )
    .unwrap();
    conn.execute(
            "INSERT INTO repository_packages (repository_id, name, version, checksum, size, download_url, version_scheme)
             VALUES (1, 'pkg', '1.0', 'sha256:test', 1, 'https://example.test/pkg', 'rpm')",
            [],
        )
        .unwrap();
}

/// Every statement this module issues against `repository_provides`, with
/// the batch template resolved to a bound list. The query-plan proof below
/// reads this list, so a statement added without a plan is a test failure,
/// not an unnoticed table scan.
fn owned_statements() -> Vec<(&'static str, String)> {
    vec![
        ("insert", INSERT_PROVIDE_SQL.to_string()),
        (
            "find_by_repository_package",
            SELECT_BY_PACKAGE_SQL.to_string(),
        ),
        (
            "find_by_repository_packages",
            SELECT_BY_PACKAGES_TEMPLATE.replace("{placeholders}", "?1, ?2"),
        ),
        ("find_by_capability", SELECT_BY_CAPABILITY_SQL.to_string()),
        (
            "find_by_cli_exact_query",
            SELECT_BY_CLI_EXACT_QUERY_SQL.to_string(),
        ),
        (
            "find_by_cli_raw_query",
            SELECT_BY_CLI_RAW_QUERY_SQL.to_string(),
        ),
        (
            "validate_cli_exact_references",
            SELECT_CLI_EXACT_ORPHAN_SQL.to_string(),
        ),
        (
            "validate_cli_raw_references",
            SELECT_CLI_RAW_ORPHAN_SQL.to_string(),
        ),
        (
            "validate_cli_typed_references",
            SELECT_CLI_TYPED_ORPHAN_SQL.to_string(),
        ),
        (
            "find_by_capability_and_kind",
            SELECT_BY_CAPABILITY_AND_KIND_SQL.to_string(),
        ),
        ("delete_by_package", DELETE_BY_PACKAGE_SQL.to_string()),
        ("delete_by_repository", DELETE_BY_REPOSITORY_SQL.to_string()),
    ]
}

fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap_or_else(|error| panic!("prepare plan for {sql}: {error}"));
    for index in 1..=stmt.parameter_count() {
        stmt.raw_bind_parameter(index, rusqlite::types::Null)
            .unwrap();
    }
    let mut rows = stmt.raw_query();
    let mut plan = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        plan.push(row.get::<_, String>(3).unwrap());
    }
    plan
}

/// `repository_provides` is the largest table Conary persists, so its index
/// inventory is a contract. A `(kind, capability)` composite cannot serve a
/// capability-only seek, which is what every provider lookup issues. The
/// raw-spelling arm has its own partial index, so exactly three indexes are
/// carried: the package key, capability key, and raw-spelling key.
#[test]
fn repository_provides_carries_exactly_the_package_capability_and_raw_indexes() {
    let conn = test_db();

    let mut indexes = conn
        .prepare(
            "SELECT name FROM sqlite_master
                 WHERE type = 'index' AND tbl_name = 'repository_provides'
                 ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    indexes.sort();

    assert_eq!(
        indexes,
        vec![
            "idx_repository_provides_capability".to_string(),
            "idx_repository_provides_pkg".to_string(),
            "idx_repository_provides_raw".to_string(),
        ],
        "the raw-spelling index is partial and has its own query-plan proof"
    );
}

/// The property, not the instance: no statement this module owns may reach
/// `repository_provides` by scanning it, and every statement that filters
/// `capability` or `raw` must seek the corresponding index.
#[test]
fn every_owned_statement_reaches_repository_provides_through_an_index() {
    let conn = test_db();

    for (label, sql) in owned_statements() {
        let plan = query_plan(&conn, &sql);
        for step in &plan {
            assert!(
                !(step.starts_with("SCAN") && step.contains("repository_provides")),
                "{label} scans repository_provides: {plan:?}"
            );
        }
    }

    for (label, expected_index, expected_constraint) in [
        (
            "find_by_capability",
            "idx_repository_provides_capability",
            "(capability=?)",
        ),
        (
            "find_by_cli_exact_query",
            "idx_repository_provides_capability",
            "(capability=?)",
        ),
        (
            "find_by_capability_and_kind",
            "idx_repository_provides_capability",
            "(capability=?)",
        ),
        (
            "find_by_cli_raw_query",
            "idx_repository_provides_raw",
            "(raw=?)",
        ),
        (
            "validate_cli_exact_references",
            "idx_repository_provides_capability",
            "(capability=?)",
        ),
        (
            "validate_cli_raw_references",
            "idx_repository_provides_raw",
            "(raw=?)",
        ),
        (
            "validate_cli_typed_references",
            "idx_repository_provides_capability",
            "(capability=?)",
        ),
    ] {
        let (_, sql) = owned_statements()
            .into_iter()
            .find(|(name, _)| *name == label)
            .expect("statement is inventoried");
        let plan = query_plan(&conn, &sql);
        assert!(
            plan.iter().any(|step| {
                step.starts_with("SEARCH ")
                    && step.contains(&format!("USING INDEX {expected_index}"))
                    && step.ends_with(expected_constraint)
            }),
            "{label} must seek its expected index: {plan:?}"
        );
    }
}

#[test]
fn cli_orphan_probes_seek_both_branches_of_an_immutable_attached_universe() {
    let conn = test_db();
    let index_file = tempfile::NamedTempFile::new().unwrap();
    let index = Connection::open(index_file.path()).unwrap();
    index
        .execute_batch(
            "CREATE TABLE repository_provides (
                 id INTEGER PRIMARY KEY, repository_package_id INTEGER NOT NULL,
                 capability TEXT NOT NULL, kind TEXT NOT NULL, raw TEXT
             );
             CREATE INDEX idx_repository_provides_capability
                 ON repository_provides(capability);
             CREATE INDEX idx_repository_provides_raw
                 ON repository_provides(raw) WHERE raw IS NOT NULL AND raw != '';
             CREATE TABLE repository_packages (
                 id INTEGER PRIMARY KEY, repository_id INTEGER NOT NULL
             );",
        )
        .unwrap();
    drop(index);

    let mut uri = url::Url::from_file_path(index_file.path()).unwrap();
    uri.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    conn.execute("ATTACH DATABASE ?1 AS remi_universe_index", [uri.as_str()])
        .unwrap();
    conn.execute_batch(
        "CREATE TEMP VIEW resolved_repository_provides AS
             SELECT id, repository_package_id, capability, kind, raw
             FROM main.repository_provides
             UNION ALL
             SELECT id, repository_package_id, capability, kind, raw
             FROM remi_universe_index.repository_provides;
         CREATE TEMP VIEW resolved_repository_packages AS
             SELECT id, repository_id FROM main.repository_packages
             UNION ALL
             SELECT id, repository_id FROM remi_universe_index.repository_packages;",
    )
    .unwrap();

    for (label, sql, index_name, constraint) in [
        (
            "normalized package-name",
            SELECT_CLI_EXACT_ORPHAN_SQL,
            "idx_repository_provides_capability",
            "(capability=?)",
        ),
        (
            "raw spelling",
            SELECT_CLI_RAW_ORPHAN_SQL,
            "idx_repository_provides_raw",
            "(raw=?)",
        ),
        (
            "typed capability",
            SELECT_CLI_TYPED_ORPHAN_SQL,
            "idx_repository_provides_capability",
            "(capability=?)",
        ),
    ] {
        let plan = query_plan(&conn, sql);
        let indexed_branches = plan
            .iter()
            .filter(|step| step.contains(index_name) && step.ends_with(constraint))
            .count();
        assert_eq!(
            indexed_branches, 2,
            "{label} must seek main and attached provide indexes: {plan:?}"
        );
        assert!(
            !plan.iter().any(|step| {
                step.starts_with("SCAN ")
                    && (step.contains("repository_provides")
                        || step.contains("repository_packages")
                        || step.contains("pkg"))
            }),
            "{label} scans the attached universe: {plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.starts_with("MATERIALIZE ")),
            "{label} materializes the attached universe: {plan:?}"
        );
        assert!(
            plan.iter().any(|step| {
                step.starts_with("SEARCH ")
                    && (step.contains("repository_packages") || step.contains("pkg"))
            }),
            "{label} must seek package identity by key: {plan:?}"
        );
    }
}

#[test]
fn repository_provide_round_trip() {
    let conn = test_db();
    seed_repo_and_package(&conn);

    let mut provide = RepositoryProvide::new(
        1,
        "mail-transport-agent".to_string(),
        None,
        "package".to_string(),
        Some("mail-transport-agent".to_string()),
        VersionScheme::Rpm,
    )
    .with_architecture_qualifier(ProvideArchitectureQualifier::Exact("arm64".to_string()))
    .with_provenance(CapabilityProvenance::SourceDeclared {
        format: crate::repository::dependency_model::SourcePackageFormat::Rpm,
        record_index: 7,
    });
    provide.insert(&conn).unwrap();

    let found = RepositoryProvide::find_by_repository_package(&conn, 1).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].capability, "mail-transport-agent");
    assert_eq!(found[0].version_scheme, VersionScheme::Rpm);
    assert_eq!(
        found[0].architecture_qualifier,
        ProvideArchitectureQualifier::Exact("arm64".to_string())
    );
    assert_eq!(found[0].provenance, provide.provenance);
}

#[test]
fn repository_provide_with_version_scheme() {
    let conn = test_db();
    seed_repo_and_package(&conn);

    let mut provide = RepositoryProvide::new(
        1,
        "libc.so.6".to_string(),
        Some("2.34".to_string()),
        "soname".to_string(),
        None,
        VersionScheme::Rpm,
    );
    provide.insert(&conn).unwrap();

    let found = RepositoryProvide::find_by_repository_package(&conn, 1).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].version_scheme, VersionScheme::Rpm);
}

#[test]
fn conversion_cache_digest_tracks_only_exact_repository_projection() {
    let conn = test_db();
    seed_repo_and_package(&conn);

    let empty = RepositoryProvide::conversion_capabilities_digest(&conn, 1).unwrap();
    let mut shell = RepositoryProvide::new(
        1,
        "/usr/bin/sh".to_string(),
        None,
        "file".to_string(),
        Some("first diagnostic spelling".to_string()),
        VersionScheme::Rpm,
    );
    shell.insert(&conn).unwrap();
    let with_shell = RepositoryProvide::conversion_capabilities_digest(&conn, 1).unwrap();
    assert_ne!(empty, with_shell);

    let mut duplicate = RepositoryProvide::new(
        1,
        "/usr/bin/sh".to_string(),
        None,
        "file".to_string(),
        Some("different diagnostic spelling".to_string()),
        VersionScheme::Rpm,
    );
    duplicate.insert(&conn).unwrap();
    assert_eq!(
        RepositoryProvide::conversion_capabilities_digest(&conn, 1).unwrap(),
        with_shell,
        "raw diagnostic text and duplicate rows do not change the cache projection"
    );

    let mut kernel_install = RepositoryProvide::new(
        1,
        "/usr/bin/kernel-install".to_string(),
        None,
        "file".to_string(),
        None,
        VersionScheme::Rpm,
    );
    kernel_install.insert(&conn).unwrap();
    assert_ne!(
        RepositoryProvide::conversion_capabilities_digest(&conn, 1).unwrap(),
        with_shell
    );
}

#[test]
fn multi_package_lookup_chunks_at_the_runtime_sqlite_variable_limit() {
    let conn = test_db();
    seed_repo_and_package(&conn);
    for package_id in 2..=5 {
        conn.execute(
            "INSERT INTO repository_packages
                 (id, repository_id, name, version, checksum, size, download_url, version_scheme)
                 VALUES (?1, 1, ?2, '1.0', 'sha256:test', 1, ?3, 'rpm')",
            params![
                package_id,
                format!("pkg-{package_id}"),
                format!("https://example.test/pkg-{package_id}")
            ],
        )
        .unwrap();
    }
    for package_id in 1..=5 {
        let mut provide = RepositoryProvide::new(
            package_id,
            format!("cap-{package_id}"),
            None,
            "package".to_string(),
            None,
            VersionScheme::Rpm,
        );
        provide.insert(&conn).unwrap();
    }
    conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 2)
        .unwrap();

    let found = RepositoryProvide::find_by_repository_packages(&conn, &[5, 4, 3, 2, 1, 1]).unwrap();

    assert_eq!(found.len(), 5);
    assert_eq!(
        found
            .iter()
            .map(|provide| provide.repository_package_id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
}

#[test]
fn find_by_capability_and_kind_filters_correctly() {
    let conn = test_db();
    seed_repo_and_package(&conn);

    let mut p1 = RepositoryProvide::new(
        1,
        "foo".to_string(),
        None,
        "package".to_string(),
        None,
        VersionScheme::Rpm,
    );
    p1.insert(&conn).unwrap();
    let mut p2 = RepositoryProvide::new(
        1,
        "foo".to_string(),
        None,
        "virtual".to_string(),
        None,
        VersionScheme::Rpm,
    );
    p2.insert(&conn).unwrap();

    let pkg_only = RepositoryProvide::find_by_capability_and_kind(&conn, "foo", "package").unwrap();
    assert_eq!(pkg_only.len(), 1);
    assert_eq!(pkg_only[0].kind, "package");
}

#[test]
fn cli_exact_query_matches_raw_or_package_rows_only() {
    let conn = test_db();
    seed_repo_and_package(&conn);

    let mut typed = RepositoryProvide::new(
        1,
        "libssl.so.3".to_string(),
        None,
        "soname".to_string(),
        Some("libssl.so.3()(64bit)".to_string()),
        VersionScheme::Rpm,
    );
    typed.insert(&conn).unwrap();
    let mut package = RepositoryProvide::new(
        1,
        "openssl".to_string(),
        None,
        "package".to_string(),
        None,
        VersionScheme::Rpm,
    );
    package.insert(&conn).unwrap();
    let mut empty_raw = RepositoryProvide::new(
        1,
        "empty-raw-cap".to_string(),
        None,
        "package".to_string(),
        Some(String::new()),
        VersionScheme::Rpm,
    );
    empty_raw.insert(&conn).unwrap();

    let untyped = RepositoryProvide::find_by_cli_exact_query(&conn, "libssl.so.3").unwrap();
    assert!(untyped.is_empty());

    let raw = RepositoryProvide::find_by_cli_exact_query(&conn, "libssl.so.3()(64bit)").unwrap();
    assert_eq!(raw.len(), 1);

    let package = RepositoryProvide::find_by_cli_exact_query(&conn, "openssl").unwrap();
    assert_eq!(package.len(), 1);

    let empty_raw = RepositoryProvide::find_by_cli_exact_query(&conn, "empty-raw-cap").unwrap();
    assert_eq!(
        empty_raw.len(),
        1,
        "empty-raw rows belong to the capability arm exactly once"
    );

    // The empty query is answered without consulting either arm: the
    // fixture's empty-raw row must not resurface through raw equality the
    // way the pre-split OR accidentally allowed.
    let empty_query = RepositoryProvide::find_by_cli_exact_query(&conn, "").unwrap();
    assert!(
        empty_query.is_empty(),
        "an empty query names nothing: {empty_query:?}"
    );
}

#[test]
fn cli_reference_probes_reject_matching_orphans_without_failing_unrelated_queries() {
    let conn = test_db();
    seed_repo_and_package(&conn);
    RepositoryProvide::new(
        1,
        "intact-package".to_string(),
        None,
        "package".to_string(),
        None,
        VersionScheme::Rpm,
    )
    .insert(&conn)
    .unwrap();

    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    RepositoryProvide::new(
        900,
        "liborphan.so.1".to_string(),
        None,
        "soname".to_string(),
        Some("raw-orphan".to_string()),
        VersionScheme::Rpm,
    )
    .insert(&conn)
    .unwrap();
    RepositoryProvide::new(
        900,
        "orphan-package".to_string(),
        None,
        "package".to_string(),
        None,
        VersionScheme::Rpm,
    )
    .insert(&conn)
    .unwrap();
    let mut missing_repository = RepositoryPackage::new(
        901,
        "missing-repo-pkg".to_string(),
        "1.0".to_string(),
        VersionScheme::Rpm,
        "0".repeat(64),
        1,
        "https://example.test/missing-repo-pkg".to_string(),
    );
    let package_id = missing_repository.insert(&conn).unwrap();
    RepositoryProvide::new(
        package_id,
        "missing-repo-pkg".to_string(),
        None,
        "package".to_string(),
        None,
        VersionScheme::Rpm,
    )
    .insert(&conn)
    .unwrap();

    assert!(RepositoryProvide::validate_cli_exact_references(&conn, "intact-package").is_ok());
    assert!(
        RepositoryProvide::validate_cli_typed_references(&conn, "intact-package", "package")
            .is_ok()
    );
    for query in ["raw-orphan", "orphan-package", "missing-repo-pkg"] {
        assert!(
            RepositoryProvide::validate_cli_exact_references(&conn, query).is_err(),
            "{query} must refuse an orphaned matching provide"
        );
    }
    assert!(
        RepositoryProvide::validate_cli_typed_references(&conn, "liborphan.so.1", "soname")
            .is_err()
    );
    assert!(
        RepositoryProvide::find_by_cli_exact_query(&conn, "raw-orphan")
            .unwrap()
            .is_empty(),
        "the existing joined lookup alone silently hides the orphan"
    );
    assert!(
        RepositoryProvide::find_by_capability_and_kind(&conn, "liborphan.so.1", "soname")
            .unwrap()
            .is_empty(),
        "the shared resolver lookup remains unchanged"
    );
}

#[test]
fn cli_reference_probes_preserve_disabled_repository_exclusion() {
    let conn = test_db();
    let mut repository = Repository::new(
        "disabled-provider-source".to_string(),
        "https://example.test/disabled".to_string(),
    );
    repository.enabled = false;
    let repository_id = repository.insert(&conn).unwrap();
    let mut package = RepositoryPackage::new(
        repository_id,
        "disabled-package".to_string(),
        "1.0".to_string(),
        VersionScheme::Rpm,
        "0".repeat(64),
        1,
        "https://example.test/disabled-package".to_string(),
    );
    let package_id = package.insert(&conn).unwrap();
    RepositoryProvide::new(
        package_id,
        "disabled-package".to_string(),
        None,
        "package".to_string(),
        None,
        VersionScheme::Rpm,
    )
    .insert(&conn)
    .unwrap();
    RepositoryProvide::new(
        package_id,
        "libdisabled.so.1".to_string(),
        None,
        "soname".to_string(),
        Some("raw-disabled".to_string()),
        VersionScheme::Rpm,
    )
    .insert(&conn)
    .unwrap();

    for query in ["disabled-package", "raw-disabled"] {
        RepositoryProvide::validate_cli_exact_references(&conn, query).unwrap();
        assert!(
            RepositoryProvide::find_by_cli_exact_query(&conn, query)
                .unwrap()
                .is_empty(),
            "{query} must not expose a disabled repository"
        );
    }
    RepositoryProvide::validate_cli_typed_references(&conn, "libdisabled.so.1", "soname").unwrap();
    assert!(
        RepositoryProvide::find_by_capability_and_kind(&conn, "libdisabled.so.1", "soname")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn selected_repository_provide_requires_canonical_kind_and_valid_capability() {
    let valid = RepositoryProvide::new(
        1,
        "libssl.so.3".to_string(),
        None,
        "soname".to_string(),
        Some("libssl.so.3()(64bit)".to_string()),
        VersionScheme::Rpm,
    );
    assert_eq!(
        valid.validated_capability().unwrap().kind,
        crate::repository::dependency_model::RepositoryCapabilityKind::Soname
    );

    let invalid_kind = RepositoryProvide::new(
        1,
        "libssl.so.3".to_string(),
        None,
        "invented-kind".to_string(),
        Some("libssl.so.3()(64bit)".to_string()),
        VersionScheme::Rpm,
    );
    assert!(
        format!("{}", invalid_kind.validated_capability().unwrap_err())
            .contains("unsupported normalized repository provide kind")
    );

    let invalid_version = RepositoryProvide::new(
        1,
        "openssl".to_string(),
        Some("bad version!".to_string()),
        "package".to_string(),
        None,
        VersionScheme::Conary,
    );
    assert!(
        format!("{}", invalid_version.validated_capability().unwrap_err())
            .contains("invalid conary provider version")
    );
}

#[test]
fn delete_by_package_removes_provides() {
    let conn = test_db();
    seed_repo_and_package(&conn);

    let mut provide = RepositoryProvide::new(
        1,
        "cap".to_string(),
        None,
        "virtual".to_string(),
        None,
        VersionScheme::Rpm,
    );
    provide.insert(&conn).unwrap();

    RepositoryProvide::delete_by_package(&conn, 1).unwrap();
    let found = RepositoryProvide::find_by_repository_package(&conn, 1).unwrap();
    assert!(found.is_empty());
}

#[test]
fn delete_by_repository_removes_provides() {
    let conn = test_db();
    seed_repo_and_package(&conn);

    let mut provide = RepositoryProvide::new(
        1,
        "cap".to_string(),
        None,
        "virtual".to_string(),
        None,
        VersionScheme::Rpm,
    );
    provide.insert(&conn).unwrap();

    RepositoryProvide::delete_by_repository(&conn, 1).unwrap();
    let found = RepositoryProvide::find_by_repository_package(&conn, 1).unwrap();
    assert!(found.is_empty());
}
