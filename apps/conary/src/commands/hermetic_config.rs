// apps/conary/src/commands/hermetic_config.rs

//! Command-owned hermetic builder configuration.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use conary_core::recipe::Recipe;
use conary_core::recipe::hermetic::{BuilderEnvironmentIdentity, BuilderEnvironmentKind};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub(crate) struct HermeticBuilder {
    pub(crate) identity: BuilderEnvironmentIdentity,
    pub(crate) sysroot_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct HermeticConfigFile {
    default_builder: String,
    builders: BTreeMap<String, BuilderConfigFile>,
}

#[derive(Debug, Deserialize)]
struct BuilderConfigFile {
    kind: String,
    sysroot_path: PathBuf,
    #[serde(default)]
    sysroot_hash: Option<String>,
    #[serde(default)]
    toolchain_hash: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

pub(crate) fn load_default_hermetic_builder() -> Result<HermeticBuilder> {
    let path = resolve_default_config_path()?;
    load_default_hermetic_builder_from_path(&path)
}

pub(crate) fn load_default_hermetic_builder_from_path(
    path: impl AsRef<Path>,
) -> Result<HermeticBuilder> {
    let path = path.as_ref();
    let canonical_config = path
        .canonicalize()
        .with_context(|| format!("hermetic config is required at {}", path.display()))?;
    check_config_file_policy(&canonical_config)
        .with_context(|| format!("hermetic config policy check failed for {}", path.display()))?;

    let content = std::fs::read_to_string(&canonical_config)
        .with_context(|| format!("read hermetic config {}", path.display()))?;
    let parsed: HermeticConfigFile = toml::from_str(&content)
        .with_context(|| format!("parse hermetic config {}", path.display()))?;

    let builder = parsed
        .builders
        .get(&parsed.default_builder)
        .with_context(|| {
            format!(
                "hermetic config {} references unknown default_builder {:?}",
                path.display(),
                parsed.default_builder
            )
        })?;

    if builder.kind != "pristine" {
        bail!(
            "hermetic config {} builder {:?} has unsupported kind {:?}; M2a accepts only \"pristine\"",
            path.display(),
            parsed.default_builder,
            builder.kind
        );
    }

    if builder.sysroot_hash.is_none() && builder.toolchain_hash.is_none() {
        bail!(
            "hermetic config {} builder {:?} must set sysroot_hash or toolchain_hash",
            path.display(),
            parsed.default_builder
        );
    }

    validate_hash_field(
        path,
        &parsed.default_builder,
        "sysroot_hash",
        &builder.sysroot_hash,
    )?;
    validate_hash_field(
        path,
        &parsed.default_builder,
        "toolchain_hash",
        &builder.toolchain_hash,
    )?;

    let sysroot_path = builder.sysroot_path.canonicalize().with_context(|| {
        format!(
            "configured sysroot_path {} from hermetic config {} must exist",
            builder.sysroot_path.display(),
            path.display()
        )
    })?;
    if !sysroot_path.is_dir() {
        bail!(
            "configured sysroot_path {} from hermetic config {} is not a directory",
            sysroot_path.display(),
            path.display()
        );
    }
    check_sysroot_policy(&sysroot_path).with_context(|| {
        format!(
            "hermetic sysroot policy check failed for {} from {}",
            sysroot_path.display(),
            path.display()
        )
    })?;

    let _description = builder.description.as_deref();

    Ok(HermeticBuilder {
        identity: BuilderEnvironmentIdentity {
            kind: BuilderEnvironmentKind::Pristine,
            sysroot_hash: builder.sysroot_hash.clone(),
            toolchain_hash: builder.toolchain_hash.clone(),
            diagnostics: Vec::new(),
        },
        sysroot_path,
    })
}

pub(crate) fn ensure_no_build_dependencies_for_m2a(recipe: &Recipe) -> Result<()> {
    let deps = recipe.all_build_deps();
    if deps.is_empty() {
        return Ok(());
    }

    bail!(
        "recipe declares build dependencies ({}) but M2a hermetic cook/publish refuses them until dependency content locks are available",
        deps.join(", ")
    );
}

fn resolve_default_config_path() -> Result<PathBuf> {
    resolve_default_config_path_with(|key| std::env::var_os(key))
}

fn resolve_default_config_path_with(
    mut var: impl FnMut(&str) -> Option<OsString>,
) -> Result<PathBuf> {
    if let Some(path) = non_empty_os(var("CONARY_HERMETIC_CONFIG")) {
        return Ok(PathBuf::from(path));
    }

    if let Some(config_home) = non_empty_os(var("XDG_CONFIG_HOME")) {
        return Ok(PathBuf::from(config_home)
            .join("conary")
            .join("hermetic.toml"));
    }

    if let Some(home) = non_empty_os(var("HOME")) {
        return Ok(PathBuf::from(home)
            .join(".config")
            .join("conary")
            .join("hermetic.toml"));
    }

    bail!(
        "cannot determine hermetic config path; set CONARY_HERMETIC_CONFIG, XDG_CONFIG_HOME, or HOME"
    );
}

fn non_empty_os(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

fn validate_hash_field(
    config_path: &Path,
    builder_name: &str,
    field: &str,
    value: &Option<String>,
) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let rest = value.strip_prefix("sha256:").with_context(|| {
        format!(
            "hermetic config {} builder {:?} field {field} must be sha256:<64 hex>",
            config_path.display(),
            builder_name
        )
    })?;
    if rest.len() != 64 || !rest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!(
            "hermetic config {} builder {:?} field {field} must be sha256:<64 hex>",
            config_path.display(),
            builder_name
        );
    }
    Ok(())
}

#[cfg(unix)]
fn check_config_file_policy(path: &Path) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("read metadata for hermetic config {}", path.display()))?;
    ensure_owned_by_current_user_or_root(path, &metadata)?;
    ensure_not_group_or_world_writable(path, &metadata)?;
    if let Some(parent) = path.parent() {
        check_directory_trust_chain(parent)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_config_file_policy(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn check_sysroot_policy(path: &Path) -> Result<()> {
    check_directory_trust_chain(path)
}

#[cfg(not(unix))]
fn check_sysroot_policy(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn check_directory_trust_chain(start: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut current = start.to_path_buf();
    let mut first = true;
    loop {
        let metadata = std::fs::metadata(&current)
            .with_context(|| format!("read metadata for {}", current.display()))?;
        ensure_owned_by_current_user_or_root(&current, &metadata)?;
        let mode = metadata.permissions().mode();
        let writable = mode & 0o022 != 0;
        let sticky = mode & 0o1000 != 0;
        if writable {
            if !first && sticky {
                break;
            }
            bail!("{} must not be group- or world-writable", current.display());
        }

        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
        first = false;
    }

    Ok(())
}

#[cfg(unix)]
fn ensure_owned_by_current_user_or_root(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let owner = metadata.uid();
    let current = nix::unistd::Uid::effective().as_raw();
    if owner == 0 || owner == current {
        return Ok(());
    }

    bail!(
        "{} must be owned by the current user or root (uid {}, current uid {})",
        path.display(),
        owner,
        current
    );
}

#[cfg(unix)]
fn ensure_not_group_or_world_writable(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o022 == 0 {
        return Ok(());
    }

    bail!("{} must not be group- or world-writable", path.display());
}

#[cfg(test)]
mod tests;
