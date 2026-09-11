// crates/conary-core/src/resolver/canonical/tests.rs

use super::*;
use crate::db::models::{CanonicalMappingAuthority, CanonicalPackage, PackageImplementation};
use crate::db::testing::create_test_db;
use crate::repository::resolution_policy::{
    DependencyMixingPolicy, RequestScope, ResolutionPolicy,
};

#[test]
fn test_expand_canonical_name() {
    let (_t, conn) = create_test_db();
    let mut pkg = CanonicalPackage::new("apache-httpd".into(), "package".into());
    let cid = pkg.insert(&conn).unwrap();
    let mut i1 = PackageImplementation::new(
        cid,
        "fedora-44".into(),
        "httpd".into(),
        CanonicalMappingAuthority::Contract,
    );
    i1.insert_or_verify(&conn).unwrap();
    let mut i2 = PackageImplementation::new(
        cid,
        "ubuntu-26.04".into(),
        "apache2".into(),
        CanonicalMappingAuthority::Contract,
    );
    i2.insert_or_verify(&conn).unwrap();

    let resolver = CanonicalResolver::new(&conn);
    let candidates = resolver.expand("apache-httpd").unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().any(|c| c.distro_name == "httpd"));
    assert!(candidates.iter().any(|c| c.distro_name == "apache2"));
}

#[test]
fn test_expand_distro_name_resolves() {
    let (_t, conn) = create_test_db();
    let mut pkg = CanonicalPackage::new("apache-httpd".into(), "package".into());
    let cid = pkg.insert(&conn).unwrap();
    let mut i1 = PackageImplementation::new(
        cid,
        "fedora-44".into(),
        "httpd".into(),
        CanonicalMappingAuthority::Contract,
    );
    i1.insert_or_verify(&conn).unwrap();
    let mut i2 = PackageImplementation::new(
        cid,
        "ubuntu-26.04".into(),
        "apache2".into(),
        CanonicalMappingAuthority::Contract,
    );
    i2.insert_or_verify(&conn).unwrap();

    let resolver = CanonicalResolver::new(&conn);
    let candidates = resolver.expand("httpd").unwrap();
    assert_eq!(candidates.len(), 2);
}

#[test]
fn test_rank_uses_explicit_source_identity() {
    let (_t, conn) = create_test_db();

    let candidates = vec![
        ResolverCandidate {
            distro_name: "httpd".into(),
            distro: "fedora-44".into(),
            canonical_id: 1,
            repository_name: None,
        },
        ResolverCandidate {
            distro_name: "apache2".into(),
            distro: "ubuntu-26.04".into(),
            canonical_id: 1,
            repository_name: None,
        },
    ];

    let resolver = CanonicalResolver::new(&conn);
    let policy = ResolutionPolicy::new()
        .with_mixing(DependencyMixingPolicy::Guarded)
        .with_primary_source_identity("ubuntu-26.04");
    let ranked = resolver
        .rank_candidates_with_policy(&candidates, &policy)
        .unwrap();
    assert_eq!(ranked[0].distro, "ubuntu-26.04");
}

#[test]
fn measured_affinity_cannot_break_a_mutation_authority_tie() {
    let (_t, conn) = create_test_db();
    conn.execute(
        "INSERT INTO system_affinity (source_identity, package_count, percentage, updated_at) VALUES ('ubuntu-26.04', 80, 80.0, '2026-03-05')",
        [],
    )
    .unwrap();

    let candidates = vec![
        ResolverCandidate {
            distro_name: "curl".into(),
            distro: "fedora-44".into(),
            canonical_id: 1,
            repository_name: None,
        },
        ResolverCandidate {
            distro_name: "curl".into(),
            distro: "ubuntu-26.04".into(),
            canonical_id: 1,
            repository_name: None,
        },
    ];

    let resolver = CanonicalResolver::new(&conn);
    let error = resolver
        .select_candidate_with_policy(&candidates, &ResolutionPolicy::new())
        .unwrap_err();
    assert!(matches!(error, Error::AmbiguousPackageSelection { .. }));
}

#[test]
fn test_canonical_equivalents_conflict() {
    let (_t, conn) = create_test_db();
    let mut pkg = CanonicalPackage::new("apache-httpd".into(), "package".into());
    let cid = pkg.insert(&conn).unwrap();
    let mut i1 = PackageImplementation::new(
        cid,
        "fedora-44".into(),
        "httpd".into(),
        CanonicalMappingAuthority::Contract,
    );
    i1.insert_or_verify(&conn).unwrap();
    let mut i2 = PackageImplementation::new(
        cid,
        "ubuntu-26.04".into(),
        "apache2".into(),
        CanonicalMappingAuthority::Contract,
    );
    i2.insert_or_verify(&conn).unwrap();

    let resolver = CanonicalResolver::new(&conn);
    let conflicts = resolver.get_conflicts("httpd").unwrap();
    assert!(conflicts.contains(&"apache2".to_string()));
}

#[test]
fn test_no_conflict_for_different_canonicals() {
    let (_t, conn) = create_test_db();
    let mut c1 = CanonicalPackage::new("curl".into(), "package".into());
    c1.insert(&conn).unwrap();
    let mut c2 = CanonicalPackage::new("wget".into(), "package".into());
    c2.insert(&conn).unwrap();

    let resolver = CanonicalResolver::new(&conn);
    let conflicts = resolver.get_conflicts("curl").unwrap();
    assert!(!conflicts.contains(&"wget".to_string()));
}

#[test]
fn test_unknown_package_no_conflicts() {
    let (_t, conn) = create_test_db();
    let resolver = CanonicalResolver::new(&conn);
    let conflicts = resolver.get_conflicts("nonexistent").unwrap();
    assert!(conflicts.is_empty());
}

#[test]
fn test_rank_with_policy_scope_repo() {
    let (_t, conn) = create_test_db();
    let resolver = CanonicalResolver::new(&conn);
    let candidates = vec![
        ResolverCandidate {
            distro_name: "curl".into(),
            distro: "fedora-44".into(),
            canonical_id: 1,
            repository_name: Some("fedora-base".into()),
        },
        ResolverCandidate {
            distro_name: "curl".into(),
            distro: "ubuntu-26.04".into(),
            canonical_id: 1,
            repository_name: Some("ubuntu-main".into()),
        },
        ResolverCandidate {
            distro_name: "curl".into(),
            distro: "ubuntu-main".into(),
            canonical_id: 1,
            repository_name: None,
        },
    ];

    let policy = ResolutionPolicy::new().with_scope(RequestScope::Repository("ubuntu-main".into()));
    let ranked = resolver
        .rank_candidates_with_policy(&candidates, &policy)
        .unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].distro, "ubuntu-26.04");
}

#[test]
fn test_rank_with_exact_profile_scope() {
    let (_t, conn) = create_test_db();

    let mut pkg = CanonicalPackage::new("curl".into(), "package".into());
    let cid = pkg.insert(&conn).unwrap();
    let mut i1 = PackageImplementation::new(
        cid,
        "fedora-44".into(),
        "curl".into(),
        CanonicalMappingAuthority::Contract,
    );
    i1.insert_or_verify(&conn).unwrap();
    let mut i2 = PackageImplementation::new(
        cid,
        "ubuntu-26.04".into(),
        "curl".into(),
        CanonicalMappingAuthority::Contract,
    );
    i2.insert_or_verify(&conn).unwrap();

    let resolver = CanonicalResolver::new(&conn);
    let candidates = resolver.expand("curl").unwrap();

    let policy =
        ResolutionPolicy::new().with_scope(RequestScope::SourceIdentity("ubuntu-26.04".into()));
    let ranked = resolver
        .rank_candidates_with_policy(&candidates, &policy)
        .unwrap();
    assert_eq!(ranked[0].distro, "ubuntu-26.04");
}

#[test]
fn repology_status_cannot_break_a_mutation_authority_tie() {
    let (_t, conn) = create_test_db();
    let fresh = chrono::Utc::now().to_rfc3339();

    let mut pkg = CanonicalPackage::new("python".into(), "package".into());
    let cid = pkg.insert(&conn).unwrap();
    let mut fedora_impl = PackageImplementation::new(
        cid,
        "fedora-44".into(),
        "python3".into(),
        CanonicalMappingAuthority::Contract,
    );
    fedora_impl.insert_or_verify(&conn).unwrap();
    let mut arch_impl = PackageImplementation::new(
        cid,
        "arch".into(),
        "python".into(),
        CanonicalMappingAuthority::Contract,
    );
    arch_impl.insert_or_verify(&conn).unwrap();

    crate::db::models::RepologyCacheEntry::insert_or_replace(
        &conn,
        &crate::db::models::RepologyCacheEntry {
            project_name: "python".into(),
            distro: "fedora-44".into(),
            distro_name: "python3".into(),
            version: Some("3.12.0".into()),
            status: Some("outdated".into()),
            fetched_at: fresh.clone(),
        },
    )
    .unwrap();
    crate::db::models::RepologyCacheEntry::insert_or_replace(
        &conn,
        &crate::db::models::RepologyCacheEntry {
            project_name: "python".into(),
            distro: "arch".into(),
            distro_name: "python".into(),
            version: Some("3.13.0".into()),
            status: Some("newest".into()),
            fetched_at: fresh.clone(),
        },
    )
    .unwrap();

    let resolver = CanonicalResolver::new(&conn);
    let candidates = resolver.expand("python").unwrap();
    let error = resolver
        .select_candidate_with_policy(&candidates, &ResolutionPolicy::new())
        .unwrap_err();
    assert!(matches!(error, Error::AmbiguousPackageSelection { .. }));
}

#[test]
fn exact_source_scope_can_resolve_otherwise_ambiguous_candidates() {
    let (_t, conn) = create_test_db();
    let mut pkg = CanonicalPackage::new("python".into(), "package".into());
    let cid = pkg.insert(&conn).unwrap();
    let mut fedora_impl = PackageImplementation::new(
        cid,
        "fedora-44".into(),
        "python3".into(),
        CanonicalMappingAuthority::Contract,
    );
    fedora_impl.insert_or_verify(&conn).unwrap();
    let mut arch_impl = PackageImplementation::new(
        cid,
        "arch".into(),
        "python".into(),
        CanonicalMappingAuthority::Contract,
    );
    arch_impl.insert_or_verify(&conn).unwrap();

    let resolver = CanonicalResolver::new(&conn);
    let candidates = resolver.expand("python").unwrap();
    let policy = ResolutionPolicy::new()
        .with_scope(RequestScope::SourceIdentity("fedora-44".to_string()))
        .with_primary_source_identity("fedora-44");
    let selected = resolver
        .select_candidate_with_policy(&candidates, &policy)
        .unwrap();
    assert_eq!(
        selected.map(|candidate| candidate.distro),
        Some("fedora-44".into())
    );
}

#[test]
fn expansion_preserves_exact_repository_variants() {
    let (_t, conn) = create_test_db();

    let mut pkg = CanonicalPackage::new("httpd-web".into(), "package".into());
    let cid = pkg.insert(&conn).unwrap();

    // Two repos for the same distro, different priorities
    conn.execute(
        "INSERT INTO repositories (name, url, enabled, priority, source_profile)
         VALUES ('fedora-base', 'https://base.com', 1, 10, 'fedora-44')",
        [],
    )
    .unwrap();
    let base_repo = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO repositories (name, url, enabled, priority, source_profile)
         VALUES ('fedora-updates', 'https://updates.com', 1, 20, 'fedora-44')",
        [],
    )
    .unwrap();
    let updates_repo = conn.last_insert_rowid();

    // Same package in both repos, both linked to same canonical
    conn.execute(
        "INSERT INTO repository_packages (repository_id, name, version, checksum, size, download_url, version_scheme, canonical_id)
         VALUES (?1, 'httpd', '2.4.58', 'sha256:a', 100, 'https://base.com/httpd', 'rpm', ?2)",
        rusqlite::params![base_repo, cid],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO repository_packages (repository_id, name, version, checksum, size, download_url, version_scheme, canonical_id)
         VALUES (?1, 'httpd', '2.4.59', 'sha256:b', 100, 'https://updates.com/httpd', 'rpm', ?2)",
        rusqlite::params![updates_repo, cid],
    )
    .unwrap();

    // Create the implementation so expand() finds it
    let mut impl1 = PackageImplementation::new(
        cid,
        "fedora-44".into(),
        "httpd".into(),
        CanonicalMappingAuthority::Contract,
    );
    impl1.insert_or_verify(&conn).unwrap();

    let resolver = CanonicalResolver::new(&conn);
    let candidates = resolver.expand("httpd").unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[0].repository_name.as_deref(),
        Some("fedora-updates")
    );
    assert_eq!(
        candidates[1].repository_name.as_deref(),
        Some("fedora-base")
    );

    let policy = ResolutionPolicy::new().with_scope(RequestScope::Repository("fedora-base".into()));
    let ranked = resolver
        .rank_candidates_with_policy(&candidates, &policy)
        .unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].repository_name.as_deref(), Some("fedora-base"));
}
