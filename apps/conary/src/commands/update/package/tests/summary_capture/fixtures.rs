// apps/conary/src/commands/update/package/tests/summary_capture/fixtures.rs

use super::*;

#[derive(Clone, Copy)]
pub(super) enum FixtureFailure {
    Lifecycle,
    MissingInterpreter,
}

pub(super) fn add_candidate(
    conn: &rusqlite::Connection,
    dir: &Path,
    name: &str,
    failure: Option<FixtureFailure>,
    relation: bool,
    replacement: Option<&str>,
    named: bool,
) {
    let mut bundle = rpm_upgrade_bundle(name, "2.0.0");
    if let Some(failure) = failure {
        bundle.entries[0].body = "error('forced update summary lifecycle failure')\n".into();
        bundle.entries[0].body_sha256 =
            conary_core::hash::sha256_prefixed(bundle.entries[0].body.as_bytes());
        if matches!(failure, FixtureFailure::MissingInterpreter) {
            bundle.entries[0].interpreter = "/missing/summary-interpreter".into();
            bundle.entries[0].rpm_runtime.as_mut().unwrap().program = RpmProgram::External;
        }
    }
    if named {
        let (uid, gid) = (unsafe { libc::geteuid() }, unsafe { libc::getegid() });
        bundle.entries[0].body = format!(
            "local p = assert(io.open('/etc/passwd', 'a'))\np:write('summary-late-user:x:{uid}:{gid}:fixture:/:/sbin/nologin\\n')\np:close()\nlocal g = assert(io.open('/etc/group', 'a'))\ng:write('summary-late-group:x:{gid}:\\n')\ng:close()\n"
        );
        bundle.entries[0].body_sha256 =
            conary_core::hash::sha256_prefixed(bundle.entries[0].body.as_bytes());
    }
    let artifact_dir = dir.join(name);
    std::fs::create_dir_all(&artifact_dir).unwrap();
    let relations = if let Some(replacement) = replacement {
        vec![
            conary_core::repository::package_relation::parse_native_relation(
                conary_core::repository::dependency_model::RepositoryRequirementKind::Obsolete,
                conary_core::repository::versioning::VersionScheme::Rpm,
                replacement,
            )
            .unwrap(),
        ]
    } else if relation {
        use conary_core::repository::{
            dependency_model::RepositoryRequirementKind, versioning::VersionScheme,
        };
        let mut old = Trove::new(
            "summary-obsolete".into(),
            "1".into(),
            TroveType::Package,
            VersionScheme::Debian,
        );
        old.debian_multi_arch =
            Some(conary_core::repository::dependency_model::DebianMultiArch::No);
        old.architecture = Some("amd64".into());
        let obsolete_id = old.insert(conn).unwrap();
        let mut consumer = Trove::new(
            "summary-consumer".into(),
            "1".into(),
            TroveType::Package,
            VersionScheme::Debian,
        );
        consumer.architecture = Some("amd64".into());
        consumer.debian_multi_arch =
            Some(conary_core::repository::dependency_model::DebianMultiArch::No);
        let consumer_id = consumer.insert(conn).unwrap();
        let dependency = conary_core::repository::requirement::parse_native_requirement(
            RepositoryRequirementKind::Depends,
            VersionScheme::Debian,
            "summary-obsolete",
        )
        .unwrap();
        conary_core::db::models::InstalledRequirementGroup::insert_groups(
            conn,
            consumer_id,
            VersionScheme::Debian,
            &[dependency],
        )
        .unwrap();
        let mut consumer_bundle = rpm_upgrade_bundle("summary-consumer", "1");
        consumer_bundle.source_format = SourceFormat::Deb;
        consumer_bundle.source_family = "debian".into();
        consumer_bundle.source_profile = Some("ubuntu-26.04".into());
        consumer_bundle.source_release = Some("26.04".into());
        consumer_bundle.source_arch = Some("amd64".into());
        consumer_bundle.evidence_digest = None;
        consumer_bundle.version_scheme = conary_core::ccs::native_lifecycle::VersionScheme::Deb;
        consumer_bundle.entries.clear();
        let mut obsolete_bundle = consumer_bundle.clone();
        obsolete_bundle.source_package = "summary-obsolete".into();
        conary_core::db::models::InstalledNativeLifecycleBundle::new(
            obsolete_id,
            None,
            &obsolete_bundle,
        )
        .unwrap()
        .insert_or_replace(conn)
        .unwrap();
        conary_core::db::models::InstalledNativeLifecycleBundle::new(
            consumer_id,
            None,
            &consumer_bundle,
        )
        .unwrap()
        .insert_or_replace(conn)
        .unwrap();
        vec![
            conary_core::repository::package_relation::parse_native_relation(
                RepositoryRequirementKind::Obsolete,
                VersionScheme::Rpm,
                "summary-obsolete",
            )
            .unwrap(),
        ]
    } else {
        Vec::new()
    };
    let path = build_test_ccs_package_with_owners(
        &artifact_dir,
        name,
        "2.0.0",
        Some(bundle),
        relations,
        named,
    );
    let bytes = std::fs::read(&path).unwrap();
    let (url, _) = serve_test_file(path);
    let repo = insert_test_static_ccs_repository(conn, name, &url);
    let mut old = Trove::new_with_source(
        name.into(),
        "1.0.0".into(),
        TroveType::Package,
        InstallSource::Repository,
        conary_core::repository::versioning::VersionScheme::Rpm,
    );
    old.architecture = Some("x86_64".into());
    old.source_profile = Some("fedora-44".into());
    old.installed_from_repository_id = Some(repo);
    old.insert(conn).unwrap();
    let mut candidate = RepositoryPackage::new(
        repo,
        name.into(),
        "2.0.0".into(),
        conary_core::repository::versioning::VersionScheme::Rpm,
        conary_core::hash::sha256(&bytes),
        bytes.len() as i64,
        url,
    );
    candidate.architecture = Some("x86_64".into());
    candidate.source_profile = Some("fedora-44".into());
    let id = candidate.insert(conn).unwrap();
    let mut resolution = PackageResolution::new(
        repo,
        name.into(),
        vec![ResolutionStrategy::RepositoryPackage {
            repository_package_id: id,
        }],
    );
    resolution.version = Some("2.0.0".into());
    resolution.primary_strategy = PrimaryStrategy::RepositoryPackage;
    resolution.insert(conn).unwrap();
}

pub(super) fn add_failing_delta(conn: &rusqlite::Connection, dir: &Path, download_failure: bool) {
    let from_hash = conary_core::hash::sha256(b"ordered update base");
    let to_hash = conary_core::hash::sha256(b"ordered update target");
    for hash in [&from_hash, &to_hash] {
        conn.execute(
            "INSERT INTO file_contents (sha256_hash, content_path, size) VALUES (?1, ?2, 0)",
            rusqlite::params![hash, format!("objects/{hash}")],
        )
        .unwrap();
    }
    let path = dir.join("a-summary-update.delta");
    let bytes = b"invalid delta fixture";
    std::fs::write(&path, bytes).unwrap();
    let (url, _) = serve_test_file(path);
    let checksum = conary_core::hash::sha256(if download_failure {
        b"different downloaded bytes"
    } else {
        bytes
    });
    PackageDelta::new(
        "a-summary-update".into(),
        "1.0.0".into(),
        "2.0.0".into(),
        from_hash,
        to_hash,
        url,
        bytes.len() as i64,
        checksum,
        4096,
    )
    .insert(conn)
    .unwrap();
}
