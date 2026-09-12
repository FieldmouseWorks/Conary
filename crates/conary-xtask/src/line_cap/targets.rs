// crates/conary-xtask/src/line_cap/targets.rs

//! Cargo owns target membership, including explicit paths and auto-discovery
//! switches. Consume its versioned metadata instead of inferring test authority
//! from a directory name. No manifest means no Cargo target evidence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use super::exemption::CargoTarget;
use super::path_text;

pub(super) type TargetRoots = BTreeMap<String, CargoTarget>;

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    kind: Vec<String>,
    src_path: PathBuf,
}

pub(super) fn read_targets(root: &Path) -> Result<TargetRoots, String> {
    let manifest = root.join("Cargo.toml");
    if !manifest.exists() {
        return Ok(TargetRoots::new());
    }
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version=1",
            "--no-deps",
            "--offline",
            "--locked",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .map_err(|error| format!("cannot inspect Cargo targets: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Cargo target inspection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    decode_targets(root, &output.stdout)
}

fn decode_targets(root: &Path, bytes: &[u8]) -> Result<TargetRoots, String> {
    let metadata: Metadata = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid Cargo metadata v1: {error}"))?;
    let mut roots = TargetRoots::new();
    for target in metadata
        .packages
        .into_iter()
        .flat_map(|package| package.targets)
    {
        // Only the exact integration-test kind can certify a test context.
        // Every other (including future) kind is conservatively production.
        let kind = if target.kind == ["test"] {
            CargoTarget::Test
        } else {
            CargoTarget::Other
        };
        let path = target.src_path.canonicalize().map_err(|error| {
            format!(
                "cannot resolve Cargo target {}: {error}",
                target.src_path.display()
            )
        })?;
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        roots
            .entry(path_text(relative))
            .and_modify(|prior| {
                if kind == CargoTarget::Other {
                    *prior = kind;
                }
            })
            .or_insert(kind);
    }
    Ok(roots)
}
