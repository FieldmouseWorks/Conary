// apps/conary-test/src/explorer/fixtures.rs

use super::contract::Fixture;
use anyhow::{Result, ensure};
use conary_core::ccs::{
    SigningKeyPair,
    builder::{CcsBuilder, write_signed_current_ccs_package},
    manifest::CcsManifest,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

pub const MEMBERS: [&str; 4] = ["app-v1.ccs", "app-v2.ccs", "companion.ccs", "policy.toml"];

/// Build inert signed CCS v3 fixtures with existing Conary authoring authority.
/// Only the public verification key is retained; no host signing material is read.
pub fn build(directory: &Path) -> Result<BTreeMap<String, String>> {
    std::fs::create_dir(directory)?;
    let key = SigningKeyPair::generate();
    for fixture in [Fixture::AppV1, Fixture::AppV2, Fixture::Companion] {
        let source = tempfile::tempdir()?;
        let path = source
            .path()
            .join(fixture.package().path().trim_start_matches('/'));
        std::fs::create_dir_all(path.parent().expect("registered fixture path has parent"))?;
        std::fs::write(path, fixture.payload())?;
        let manifest = CcsManifest::new_minimal(fixture.package().name(), fixture.version());
        let built = CcsBuilder::new(manifest, source.path())?.build()?;
        write_signed_current_ccs_package(&built, &directory.join(fixture.filename()), &key, false)?;
    }
    std::fs::write(
        directory.join("policy.toml"),
        format!(
            "trusted_keys = [\"{}\"]\nrequire_timestamp = true\nmax_signature_age = 0\n",
            key.public_key_base64()
        ),
    )?;
    hashes(directory)
}

pub fn hashes(directory: &Path) -> Result<BTreeMap<String, String>> {
    MEMBERS
        .into_iter()
        .map(|name| {
            let path = directory.join(name);
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= 1024 * 1024,
                "invalid fixture member"
            );
            Ok((
                name.into(),
                hex::encode(Sha256::digest(std::fs::read(path)?)),
            ))
        })
        .collect()
}
