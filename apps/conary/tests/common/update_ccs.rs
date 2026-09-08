// apps/conary/tests/common/update_ccs.rs

//! Signed RPM update artifacts shared by collection and CLI preview tests.

use conary_core::ccs::SigningKeyPair;
use conary_core::ccs::builder::{CcsBuilder, write_signed_current_ccs_package};
use conary_core::ccs::manifest::{CcsManifest, Platform};
use conary_core::ccs::native_lifecycle::{
    NATIVE_LIFECYCLE_SCHEMA_REVISION, NATIVE_LIFECYCLE_SCHEMA_V1, NativeLifecycleBundle,
    ScriptletFidelity, SourceFormat, VersionScheme,
};
use conary_core::db::models::{
    Repository, RepositoryPackage, RepositoryPackageKey, RepositoryPackageKeyStatus,
};
use std::path::Path;

pub fn repository(conn: &rusqlite::Connection) -> (i64, SigningKeyPair) {
    let key = SigningKeyPair::generate();
    let mut repo = Repository::new("variant-repo".into(), "https://example.test/variant".into());
    repo.source_profile = Some("fedora-44".into());
    repo.default_strategy = Some("static".into());
    let id = repo.insert(conn).unwrap();
    RepositoryPackageKey::replace_for_repository(
        conn,
        id,
        &[RepositoryPackageKey {
            repository_id: id,
            public_key: key.public_key_base64(),
            key_id: key.key_id().map(str::to_string),
            status: RepositoryPackageKeyStatus::Active,
            synced_at: None,
        }],
    )
    .unwrap();
    (id, key)
}

pub fn candidate(
    conn: &rusqlite::Connection,
    dir: &Path,
    repository_id: i64,
    key: &SigningKeyPair,
    name: &str,
    arch: &str,
) {
    let source = dir.join(format!("{name}-{arch}-source"));
    std::fs::create_dir_all(source.join("usr/share")).unwrap();
    std::fs::write(source.join("usr/share").join(name), name).unwrap();
    let mut manifest = CcsManifest::new_minimal(name, "1.0-2");
    manifest.package.version_scheme = conary_core::repository::versioning::VersionScheme::Rpm;
    manifest.package.platform = Some(Platform {
        os: "linux".into(),
        arch: Some(arch.into()),
        libc: "gnu".into(),
        abi: None,
    });
    manifest.native_lifecycle = Some(NativeLifecycleBundle {
        schema: NATIVE_LIFECYCLE_SCHEMA_V1.into(),
        schema_revision: NATIVE_LIFECYCLE_SCHEMA_REVISION,
        source_format: SourceFormat::Rpm,
        source_family: "fedora-rhel".into(),
        source_profile: Some("fedora-44".into()),
        source_release: Some("44".into()),
        source_arch: Some(arch.into()),
        source_package: name.into(),
        source_version: "1.0-2".into(),
        source_checksum: None,
        version_scheme: VersionScheme::Rpm,
        conversion_tool: "test".into(),
        conversion_tool_version: "1".into(),
        conversion_policy: "update-preview".into(),
        evidence_digest: None,
        scriptlet_fidelity: ScriptletFidelity::NativeLifecycle,
        entries: Vec::new(),
    });
    let result = CcsBuilder::new(manifest, &source).unwrap().build().unwrap();
    let path = dir.join(format!("{name}-{arch}.ccs"));
    write_signed_current_ccs_package(&result, &path, key, true).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut package = RepositoryPackage::new(
        repository_id,
        name.into(),
        "1.0-2".into(),
        conary_core::repository::versioning::VersionScheme::Rpm,
        conary_core::hash::sha256(&bytes),
        bytes.len() as i64,
        path.to_string_lossy().into_owned(),
    );
    package.architecture = Some(arch.into());
    package.source_profile = Some("fedora-44".into());
    package.insert(conn).unwrap();
}
