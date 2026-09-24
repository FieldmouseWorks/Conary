// apps/conary-test/src/container/image.rs

use anyhow::{Context, Result, bail};
use conary_core::repository::supported_profiles::ProfilePackageFormat;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::backend::ContainerBackend;
use crate::config::{DistroBuildContext, DistroConfig};

#[derive(Debug)]
struct StagedBuildContext {
    root: PathBuf,
    dockerfile: PathBuf,
}

#[derive(Debug, Clone, Copy)]
struct NativePackageArtifact<'a> {
    path: &'a Path,
    format: ProfilePackageFormat,
}

impl Drop for StagedBuildContext {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn find_project_root(start: &Path) -> Result<PathBuf> {
    let mut candidate = start
        .canonicalize()
        .context("failed to canonicalize path when locating project root")?;
    let mut first_manifest_root = None;

    loop {
        let manifest = candidate.join("Cargo.toml");
        if manifest.is_file() {
            if fs::read_to_string(&manifest)
                .with_context(|| format!("failed to read {}", manifest.display()))?
                .contains("[workspace]")
            {
                return Ok(candidate);
            }

            if first_manifest_root.is_none() {
                first_manifest_root = Some(candidate.clone());
            }
        }

        if !candidate.pop() {
            break;
        }
    }

    if let Some(root) = first_manifest_root {
        return Ok(root);
    }

    bail!("failed to locate project root from {}", start.display());
}

fn copy_dir_filtered(src: &Path, dst: &Path, skip_names: &[&str]) -> Result<()> {
    fs::create_dir_all(dst)
        .with_context(|| format!("failed to create directory {}", dst.display()))?;

    for entry in
        fs::read_dir(src).with_context(|| format!("failed to read directory {}", src.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if skip_names.iter().any(|skip| *skip == name) {
            continue;
        }

        let target = dst.join(name.as_ref());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_filtered(&path, &target, skip_names)?;
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &target).with_context(|| {
                format!("failed to copy {} to {}", path.display(), target.display())
            })?;
        } else if file_type.is_symlink() {
            let link_target = fs::read_link(&path)
                .with_context(|| format!("failed to read symlink {}", path.display()))?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link_target, &target).with_context(|| {
                format!(
                    "failed to recreate symlink {} -> {}",
                    target.display(),
                    link_target.display()
                )
            })?;
        }
    }

    fs::set_permissions(dst, fs::metadata(src)?.permissions()).with_context(|| {
        format!(
            "failed to preserve directory permissions from {} on {}",
            src.display(),
            dst.display()
        )
    })?;

    Ok(())
}

const STATIC_TEST_SHELL_ENV: &str = "CONARY_TEST_STATIC_SHELL";

/// Return true when `path` is a regular file with an execute bit set.
fn is_executable_regular_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

/// Locate the static shell the `conary-test-shell` fixture stages as `/bin/sh`.
///
/// An explicit `CONARY_TEST_STATIC_SHELL` wins. Otherwise the first executable
/// `busybox` on `PATH` is used. The binary contents are deliberately not
/// inspected: the fixture hook that runs through this interpreter is the
/// functional proof that it can serve as `/bin/sh`.
fn resolve_static_test_shell() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os(STATIC_TEST_SHELL_ENV) {
        let path = PathBuf::from(explicit);
        if !is_executable_regular_file(&path) {
            bail!(
                "{STATIC_TEST_SHELL_ENV} must name an existing executable regular file, got {}",
                path.display()
            );
        }
        return Ok(path);
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path_var) {
            let candidate = directory.join("busybox");
            if is_executable_regular_file(&candidate) {
                return Ok(candidate);
            }
        }
    }

    bail!(
        "integration fixture conary-test-shell needs a statically linked shell: install busybox (static) or set CONARY_TEST_STATIC_SHELL=<path>"
    )
}

/// Build one signed fixture package and verify it under the fixture trust
/// policy, returning the single artifact it emitted.
fn build_signed_fixture(
    conary_bin: &Path,
    manifest: &Path,
    source: &Path,
    output_dir: &Path,
    signing_key: &Path,
    trust_policy: &Path,
) -> Result<PathBuf> {
    if output_dir.exists() {
        fs::remove_dir_all(output_dir)
            .with_context(|| format!("failed to reset {}", output_dir.display()))?;
    }
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let output = std::process::Command::new(conary_bin)
        .args(["ccs", "build"])
        .arg(manifest)
        .arg("--source")
        .arg(source)
        .arg("--output")
        .arg(output_dir)
        .arg("--key")
        .arg(signing_key)
        .output()
        .with_context(|| {
            format!(
                "failed to build fixture {} with {}",
                manifest.display(),
                conary_bin.display()
            )
        })?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to build fixture {}\nstdout:\n{}\nstderr:\n{}",
            manifest.display(),
            stdout.trim_end(),
            stderr.trim_end()
        );
    }

    let packages = fs::read_dir(output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "ccs"))
        .collect::<Vec<_>>();
    if packages.len() != 1 {
        bail!(
            "expected one signed fixture in {}, found {}",
            output_dir.display(),
            packages.len()
        );
    }
    let package = packages.into_iter().next().expect("one package");

    let verify = std::process::Command::new(conary_bin)
        .args(["ccs", "verify"])
        .arg(&package)
        .arg("--policy")
        .arg(trust_policy)
        .output()
        .with_context(|| {
            format!(
                "failed to verify fixture {} with {}",
                package.display(),
                conary_bin.display()
            )
        })?;
    if !verify.status.success() {
        let stdout = String::from_utf8_lossy(&verify.stdout);
        let stderr = String::from_utf8_lossy(&verify.stderr);
        bail!(
            "fixture {} did not verify under {}\nstdout:\n{}\nstderr:\n{}",
            package.display(),
            trust_policy.display(),
            stdout.trim_end(),
            stderr.trim_end()
        );
    }

    Ok(package)
}

/// Stage the static shell source into the `conary-test-shell` fixture and build
/// the signed provider. A missing fixture directory means this workspace has no
/// shell provider to build, matching how the v1/v2 fixtures are skipped.
fn build_test_shell_fixture(
    fixtures_root: &Path,
    conary_bin: &Path,
    signing_key: &Path,
    trust_policy: &Path,
) -> Result<()> {
    let fixture_root = fixtures_root.join("conary-test-shell");
    if !fixture_root.is_dir() {
        return Ok(());
    }
    let manifest = fixture_root.join("ccs.toml");
    if !manifest.is_file() {
        bail!(
            "integration fixture conary-test-shell is missing {}",
            manifest.display()
        );
    }

    let source = resolve_static_test_shell()?;
    let stage = fixture_root.join("stage");
    if stage.exists() {
        fs::remove_dir_all(&stage)
            .with_context(|| format!("failed to reset {}", stage.display()))?;
    }
    let staged_shell = stage.join("bin/sh");
    let staged_parent = staged_shell
        .parent()
        .expect("staged shell path always has a parent");
    fs::create_dir_all(staged_parent)
        .with_context(|| format!("failed to create {}", staged_parent.display()))?;
    fs::copy(&source, &staged_shell).with_context(|| {
        format!(
            "failed to stage static shell {} as {}",
            source.display(),
            staged_shell.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged_shell, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to make {} executable", staged_shell.display()))?;
    }

    build_signed_fixture(
        conary_bin,
        &manifest,
        &stage,
        &fixture_root.join("output"),
        signing_key,
        trust_policy,
    )?;
    Ok(())
}

fn ensure_phase2_fixture_outputs(fixtures_root: &Path, conary_bin: &Path) -> Result<()> {
    let fixture_root = fixtures_root.join("conary-test-fixture");
    let shell_root = fixtures_root.join("conary-test-shell");
    if !fixture_root.is_dir() && !shell_root.is_dir() {
        return Ok(());
    }

    let signing_key = crate::paths::fixture_ccs_key_path_for(fixtures_root);
    let trust_policy = crate::paths::fixture_ccs_policy_path_for(fixtures_root);
    for authority_path in [&signing_key, &trust_policy] {
        if !authority_path.is_file() {
            bail!(
                "fixture authority is missing {}; regenerate apps/conary/tests/fixtures/ccs-test-authority",
                authority_path.display()
            );
        }
    }

    if fixture_root.is_dir() {
        for version in ["v1", "v2"] {
            let version_root = fixture_root.join(version);
            let manifest = version_root.join("ccs.toml");
            let source = version_root.join("stage");
            if !manifest.is_file() || !source.is_dir() {
                continue;
            }

            build_signed_fixture(
                conary_bin,
                &manifest,
                &source,
                &version_root.join("output"),
                &signing_key,
                &trust_policy,
            )?;
        }
    }

    build_test_shell_fixture(fixtures_root, conary_bin, &signing_key, &trust_policy)?;

    Ok(())
}

fn canonical_native_package_name(format: ProfilePackageFormat) -> &'static str {
    match format {
        ProfilePackageFormat::Rpm => "conary-release.rpm",
        ProfilePackageFormat::Deb => "conary-release.deb",
        ProfilePackageFormat::Arch => "conary-release.pkg.tar.zst",
        ProfilePackageFormat::Eopkg => "conary-release.eopkg",
    }
}

fn stage_native_package(root: &Path, artifact: NativePackageArtifact<'_>) -> Result<()> {
    let metadata = fs::symlink_metadata(artifact.path).with_context(|| {
        format!(
            "failed to inspect native package artifact {}",
            artifact.path.display()
        )
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!(
            "native package artifact must be a real regular file: {}",
            artifact.path.display()
        );
    }
    if metadata.len() == 0 {
        bail!(
            "native package artifact must not be empty: {}",
            artifact.path.display()
        );
    }

    let destination = root.join(canonical_native_package_name(artifact.format));
    fs::copy(artifact.path, &destination).with_context(|| {
        format!(
            "failed to stage native package artifact {} as {}",
            artifact.path.display(),
            destination.display()
        )
    })?;
    Ok(())
}

/// Pick the Conary binary an image receives.
///
/// The static choice fails closed rather than falling back to the host build:
/// a host binary that happens to run in one image and dies at the dynamic
/// linker in another is the exact failure this capability removes.
fn resolve_stage_source(
    build_context: DistroBuildContext,
    host_binary: &Path,
    target_dir: &Path,
) -> Result<PathBuf> {
    match build_context {
        DistroBuildContext::Binary => Ok(host_binary.to_path_buf()),
        DistroBuildContext::StaticBinary => {
            crate::static_binary::static_conary_binary_in(target_dir)
        }
    }
}

fn stage_build_context(
    containerfile: &Path,
    distro: &str,
    build_context: DistroBuildContext,
    native_package: Option<NativePackageArtifact<'_>>,
) -> Result<StagedBuildContext> {
    let integration_root = containerfile
        .parent()
        .and_then(Path::parent)
        .context("containerfile is missing expected remi directory structure")?;
    let project_root = find_project_root(integration_root)?;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before unix epoch")?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("conary-test-image-{distro}-{unique}"));

    fs::create_dir_all(root.join("containers"))?;
    let dockerfile_name = containerfile
        .file_name()
        .context("containerfile has no filename")?;
    fs::copy(containerfile, root.join("containers").join(dockerfile_name))
        .with_context(|| format!("failed to copy {}", containerfile.display()))?;
    fs::copy(
        integration_root.join("config.toml"),
        root.join("config.toml"),
    )
    .context("failed to copy integration config.toml")?;

    let fixtures_src = crate::paths::resolve_fixtures_root_for(&project_root);
    if fixtures_src.is_dir() {
        copy_dir_filtered(&fixtures_src, &root.join("fixtures"), &[])?;
    } else {
        fs::create_dir_all(root.join("fixtures"))?;
    }

    let arch_pkgbuild = project_root.join("packaging/arch/PKGBUILD");
    if arch_pkgbuild.is_file() {
        let pkgbuild_dir = root.join("fixtures/pkgbuild");
        fs::create_dir_all(&pkgbuild_dir)?;
        fs::copy(&arch_pkgbuild, pkgbuild_dir.join("PKGBUILD"))
            .with_context(|| format!("failed to copy {}", arch_pkgbuild.display()))?;
    }

    // Fixture packages are built by running Conary on this host, so that step
    // always uses the host binary. What gets staged into the image is a
    // separate, typed choice: the image's userland is not the host's.
    let host_binary = crate::paths::find_host_conary_binary(&project_root)?;
    let stage_source = resolve_stage_source(
        build_context,
        &host_binary,
        &crate::static_binary::static_target_dir(&project_root),
    )?;
    let staged_binary = root.join("conary");
    fs::copy(&stage_source, &staged_binary)
        .with_context(|| format!("failed to stage conary binary {}", stage_source.display()))?;

    // Strip debug symbols to shrink the tar context sent over the container
    // socket. A debug build can be 300MB+; stripped it drops to ~70MB, which
    // avoids Podman compat-API stream errors on large payloads.
    let _ = std::process::Command::new("strip")
        .arg(&staged_binary)
        .status();

    if let Some(artifact) = native_package {
        stage_native_package(&root, artifact)?;
    }

    ensure_phase2_fixture_outputs(&root.join("fixtures"), &host_binary)?;

    Ok(StagedBuildContext {
        dockerfile: root.join("containers").join(dockerfile_name),
        root,
    })
}

/// Build a distro-specific test image from a Containerfile.
///
/// Tags the image as `conary-test-{distro}:latest`.
pub async fn build_distro_image(
    backend: &dyn ContainerBackend,
    containerfile: &Path,
    distro: &str,
    distro_config: &DistroConfig,
) -> Result<String> {
    build_distro_image_inner(backend, containerfile, distro, distro_config, None).await
}

/// Build a distro-specific test image by installing one exact native package.
///
/// The package format comes from Conary's typed supported-profile catalog.
/// Containerfiles install the staged canonical filename through the distro's
/// native package manager before any lifecycle proof runs.
pub async fn build_distro_image_from_native_package(
    backend: &dyn ContainerBackend,
    containerfile: &Path,
    distro: &str,
    distro_config: &DistroConfig,
    package: &Path,
    package_format: ProfilePackageFormat,
) -> Result<String> {
    build_distro_image_inner(
        backend,
        containerfile,
        distro,
        distro_config,
        Some(NativePackageArtifact {
            path: package,
            format: package_format,
        }),
    )
    .await
}

async fn build_distro_image_inner(
    backend: &dyn ContainerBackend,
    containerfile: &Path,
    distro: &str,
    distro_config: &DistroConfig,
    native_package: Option<NativePackageArtifact<'_>>,
) -> Result<String> {
    let tag = format!("conary-test-{distro}:latest");
    let force_rebuild = std::env::var("CONARY_TEST_REBUILD_IMAGE")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);
    let reuse_existing = native_package.is_none()
        && std::env::var("CONARY_TEST_REUSE_IMAGE")
            .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false);

    if reuse_existing && !force_rebuild {
        let images = backend
            .list_images()
            .await
            .context("failed to inspect existing distro test images")?;
        if images
            .iter()
            .any(|image| image.tags.iter().any(|candidate| candidate == &tag))
        {
            tracing::info!(image = %tag, "reusing existing distro test image");
            return Ok(tag);
        }
    }

    let staged = stage_build_context(
        containerfile,
        distro,
        distro_config.build_context,
        native_package,
    )?;
    let mut build_args = match (&distro_config.release_root, &distro_config.target_root) {
        (Some(_), Some(_)) => {
            anyhow::bail!("distro {distro} cannot declare both release_root and target_root")
        }
        (Some(release_root), None) => release_root.docker_build_args()?,
        (None, Some(target_root)) => target_root.docker_build_args()?,
        (None, None) => HashMap::new(),
    };
    if native_package.is_some() {
        build_args.insert("INSTALL_MODE".to_string(), "package".to_string());
    }
    backend
        .build_image(&staged.dockerfile, &tag, build_args)
        .await
}

#[cfg(test)]
#[path = "image/tests.rs"]
mod tests;
