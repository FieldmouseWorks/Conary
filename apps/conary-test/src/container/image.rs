// apps/conary-test/src/container/image.rs

use anyhow::{Context, Result, bail};
use conary_core::repository::supported_profiles::ProfilePackageFormat;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::backend::ContainerBackend;
use crate::config::manifest::StaticFixture;
use crate::config::{DistroBuildContext, DistroConfig};

#[path = "image/static_shell.rs"]
mod static_shell;

pub use static_shell::ShellProviderRequirement;

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

/// The artifact path image staging writes for one static fixture.
///
/// The harness variable map derives its fixture install variables from this
/// same function, so the path the image builder writes and the path a suite
/// installs cannot diverge. `fixture_dir` is `paths.fixture_dir`.
pub fn static_fixture_artifact_path(fixture: StaticFixture, fixture_dir: &Path) -> PathBuf {
    let (directory, artifact) = match fixture {
        StaticFixture::Shell => ("conary-test-shell", "conary-test-shell-1.0.0-1.ccs"),
        StaticFixture::Base => ("conary-test-base", "conary-test-base-1.0.0-1.ccs"),
    };
    fixture_dir.join(directory).join("output").join(artifact)
}

/// The fixture root that holds a static fixture's `ccs.toml` and `stage/` tree.
///
/// Derived from [`static_fixture_artifact_path`] so the directory image staging
/// reads and the artifact path the harness installs can never point at
/// different fixture roots.
fn static_fixture_root(fixture: StaticFixture, fixture_dir: &Path) -> PathBuf {
    static_fixture_artifact_path(fixture, fixture_dir)
        .parent()
        .and_then(Path::parent)
        .expect("a static fixture artifact lives under its fixture root")
        .to_path_buf()
}

impl StaticFixture {
    /// The harness variable name that holds this fixture's artifact path.
    ///
    /// Derived from [`StaticFixture::install_variable`] so the install command
    /// and the variable map can never name two different variables.
    pub fn artifact_variable(self) -> &'static str {
        self.install_variable()
            .strip_prefix("${")
            .and_then(|name| name.strip_suffix('}'))
            .expect("a fixture install variable is always a ${...} placeholder")
    }
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

/// One file the image builder writes into a provider fixture's `stage/` tree.
#[derive(Debug, Clone, Copy)]
enum FixturePayload {
    /// The host static binary the shell resolver selects and validates.
    StaticBinary(&'static str),
    /// Clearly fake bytes for a payload that is never booted or executed.
    Fake {
        path: &'static str,
        contents: &'static [u8],
    },
}

/// A hermetic provider fixture staged from the host before the image build.
#[derive(Debug, Clone, Copy)]
struct ProviderFixture {
    /// The typed fixture this staging recipe builds and the harness installs.
    ///
    /// The fixture root and artifact path derive from it through
    /// [`static_fixture_artifact_path`] rather than a second name spelling.
    typed: StaticFixture,
    /// Files written into `stage/` before `ccs build`.
    payloads: &'static [FixturePayload],
}

/// Fake boot bytes for `conary-test-base`. Publication only copies and hashes
/// them, so they need no kernel image structure; marking them clearly fake
/// keeps the repository from looking like it ships a bootable base.
const FAKE_KERNEL: &[u8] = b"conary-test-base fake kernel image; not bootable\n";
const FAKE_INITRAMFS: &[u8] = b"conary-test-base fake initramfs; not bootable\n";
const FAKE_EFI_LOADER: &[u8] = b"conary-test-base fake EFI loader; not bootable\n";

/// The `/bin/sh` provider that lets fixture hooks run (#1080).
const SHELL_PROVIDER_FIXTURE: ProviderFixture = ProviderFixture {
    typed: StaticFixture::Shell,
    payloads: &[FixturePayload::StaticBinary("bin/sh")],
};

/// The fake `/sbin/init` + boot-asset base that lets installs publish a
/// generation in an unbooted container (#1102).
///
/// The boot assets are the minimal set the generation builder resolves for a
/// staged boot root: `boot/vmlinuz-<release>` and
/// `boot/initramfs-<release>.img` matched by exact filename, with
/// `validate_kernel_release` accepting `conary-test`, plus
/// `boot/EFI/BOOT/BOOTX64.EFI`. No `lib/modules/<release>` tree is needed
/// because a staged boot root reads its versioned kernel from `/boot`.
const BASE_PROVIDER_FIXTURE: ProviderFixture = ProviderFixture {
    typed: StaticFixture::Base,
    payloads: &[
        FixturePayload::StaticBinary("sbin/init"),
        FixturePayload::Fake {
            path: "boot/vmlinuz-conary-test",
            contents: FAKE_KERNEL,
        },
        FixturePayload::Fake {
            path: "boot/initramfs-conary-test.img",
            contents: FAKE_INITRAMFS,
        },
        FixturePayload::Fake {
            path: "boot/EFI/BOOT/BOOTX64.EFI",
            contents: FAKE_EFI_LOADER,
        },
    ],
};

/// Set the execute bits on a staged provider payload.
fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to make {} executable", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Stage every payload of `fixture` and build its signed package.
///
/// A missing fixture directory means this workspace has no such provider to
/// build, matching how the v1/v2 fixtures are skipped.
fn build_provider_fixture(
    fixtures_root: &Path,
    fixture: &ProviderFixture,
    static_binary: &Path,
    conary_bin: &Path,
    signing_key: &Path,
    trust_policy: &Path,
) -> Result<()> {
    let artifact = static_fixture_artifact_path(fixture.typed, fixtures_root);
    let output_dir = artifact
        .parent()
        .expect("a static fixture artifact path has an output directory");
    let fixture_root = static_fixture_root(fixture.typed, fixtures_root);
    if !fixture_root.is_dir() {
        return Ok(());
    }
    let manifest = fixture_root.join("ccs.toml");
    if !manifest.is_file() {
        bail!(
            "integration fixture {} is missing {}",
            fixture.typed.declaration(),
            manifest.display()
        );
    }

    let stage = fixture_root.join("stage");
    if stage.exists() {
        fs::remove_dir_all(&stage)
            .with_context(|| format!("failed to reset {}", stage.display()))?;
    }
    for payload in fixture.payloads {
        match *payload {
            FixturePayload::StaticBinary(path) => {
                let destination = stage.join(path);
                fs::create_dir_all(
                    destination
                        .parent()
                        .expect("staged fixture payload always has a parent"),
                )
                .with_context(|| format!("failed to create {}", destination.display()))?;
                fs::copy(static_binary, &destination).with_context(|| {
                    format!(
                        "failed to stage static binary {} as {}",
                        static_binary.display(),
                        destination.display()
                    )
                })?;
                set_executable(&destination)?;
            }
            FixturePayload::Fake { path, contents } => {
                let destination = stage.join(path);
                fs::create_dir_all(
                    destination
                        .parent()
                        .expect("staged fixture payload always has a parent"),
                )
                .with_context(|| format!("failed to create {}", destination.display()))?;
                fs::write(&destination, contents).with_context(|| {
                    format!("failed to stage fixture payload {}", destination.display())
                })?;
            }
        }
    }

    let package = build_signed_fixture(
        conary_bin,
        &manifest,
        &stage,
        output_dir,
        signing_key,
        trust_policy,
    )?;
    if package != artifact {
        bail!(
            "fixture build for {} produced {} but the harness installs {}; update static_fixture_artifact_path",
            fixture.typed.declaration(),
            package.display(),
            artifact.display()
        );
    }
    Ok(())
}

fn ensure_phase2_fixture_outputs(
    fixtures_root: &Path,
    conary_bin: &Path,
    providers: ShellProviderRequirement,
) -> Result<()> {
    let fixture_root = fixtures_root.join("conary-test-fixture");
    let build_shell = providers.shell_required()
        && static_fixture_root(StaticFixture::Shell, fixtures_root).is_dir();
    let build_base = providers.base_required()
        && static_fixture_root(StaticFixture::Base, fixtures_root).is_dir();
    if !fixture_root.is_dir() && !build_shell && !build_base {
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

    if build_shell || build_base {
        // The `/bin/sh` provider must run the fixture hooks, so its binary is
        // functionally probed. The base provider stages the same binary as
        // `/sbin/init`, where no hooks run; a base-only image skips the probe.
        let static_binary = if build_shell {
            static_shell::resolve_static_test_shell()?
        } else {
            static_shell::resolve_static_test_binary()?
        };
        for (build, fixture) in [
            (build_shell, &SHELL_PROVIDER_FIXTURE),
            (build_base, &BASE_PROVIDER_FIXTURE),
        ] {
            if build {
                build_provider_fixture(
                    fixtures_root,
                    fixture,
                    &static_binary,
                    conary_bin,
                    &signing_key,
                    &trust_policy,
                )?;
            }
        }
    }

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
    providers: ShellProviderRequirement,
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

    ensure_phase2_fixture_outputs(&root.join("fixtures"), &host_binary, providers)?;

    Ok(StagedBuildContext {
        dockerfile: root.join("containers").join(dockerfile_name),
        root,
    })
}

/// Build a distro-specific test image from a Containerfile.
///
/// Tags the image as `conary-test-{distro}:latest`. `providers` reports which
/// hermetic provider fixtures the selected suites install; only then does
/// staging resolve a host static binary.
pub async fn build_distro_image(
    backend: &dyn ContainerBackend,
    containerfile: &Path,
    distro: &str,
    distro_config: &DistroConfig,
    providers: ShellProviderRequirement,
) -> Result<String> {
    build_distro_image_inner(
        backend,
        containerfile,
        distro,
        distro_config,
        None,
        providers,
    )
    .await
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
    providers: ShellProviderRequirement,
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
        providers,
    )
    .await
}

async fn build_distro_image_inner(
    backend: &dyn ContainerBackend,
    containerfile: &Path,
    distro: &str,
    distro_config: &DistroConfig,
    native_package: Option<NativePackageArtifact<'_>>,
    providers: ShellProviderRequirement,
) -> Result<String> {
    let tag = format!("conary-test-{distro}:latest");
    let force_rebuild = std::env::var("CONARY_TEST_REBUILD_IMAGE")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);
    // An image built by `images build` has no selected suites and therefore
    // stages no provider fixtures. A run whose suites install one must rebuild
    // so the provider payloads are present rather than silently reusing an
    // image built without them.
    let reuse_existing = native_package.is_none()
        && providers == ShellProviderRequirement::NotInstalled
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
        providers,
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
