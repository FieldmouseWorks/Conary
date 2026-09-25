// crates/conary-core/src/generation/builder/root_validation.rs

use std::collections::{HashMap, HashSet};

use crate::error::MissingBaseSystemPart;
use crate::generation::root_manifest::{GenerationRootEntry, GenerationRootManifest};
use crate::payload::PayloadNodeKind;

use super::boot_assets::{ESP_BOOTLOADER_REL, SYSTEMD_BOOT_EFI_REL};
use super::kernel::{self, solus_kernel_artifact_name};

pub(super) fn validate_runtime_generation_root_is_self_contained(
    manifest: &GenerationRootManifest,
) -> crate::Result<()> {
    manifest.validate()?;
    if generation_root_has_init_entrypoint(manifest) {
        return Ok(());
    }

    Err(crate::Error::GenerationRootMissingBaseSystem {
        missing: MissingBaseSystemPart::MissingInit,
    })
}

/// Require a host build's exact manifest to carry kernel and EFI boot assets.
///
/// The builder generates the initramfs for `BootRoot::Host` from the
/// materialized sysroot, so only the assets that must come from the manifest
/// are required. Candidate releases, release validation, and the Solus pairing
/// all use the same rules as the filesystem builder. This runs only when no
/// verified prior generation can supply the boot assets.
pub(super) fn validate_generation_root_host_boot_assets(
    manifest: &GenerationRootManifest,
) -> crate::Result<()> {
    let view = GenerationRootView::new(manifest);
    let mut releases = kernel::boot_kernel_releases_from_names(view.child_names("/boot"))?;
    for modules_root in ["/lib/modules", "/usr/lib/modules"] {
        for release in
            kernel::module_kernel_releases_from_names(view.child_names(modules_root), |release| {
                view.is_regular_file(&format!("{modules_root}/{release}/vmlinuz"))
                    || view.solus_kernel_file(release)
            })?
        {
            releases.push(release);
        }
    }
    releases.sort();
    releases.dedup();

    if releases
        .iter()
        .any(|release| view.kernel_file_present(release) && view.efi_bootloader_present())
    {
        return Ok(());
    }

    Err(crate::Error::GenerationRootMissingBaseSystem {
        missing: MissingBaseSystemPart::MissingBootAssets,
    })
}

fn generation_root_has_init_entrypoint(manifest: &GenerationRootManifest) -> bool {
    GenerationRootView::new(manifest).has_executable_init()
}

/// Manifest-local view that applies virtual-path and boot-asset rules.
///
/// Manifest-owned symlinks are resolved exactly as the builder resolves them
/// after materialization, so a `/lib -> usr/lib` alias does not hide a kernel.
struct GenerationRootView<'a> {
    entries: &'a [GenerationRootEntry],
    symlinks: HashMap<String, String>,
    regular_paths: HashSet<String>,
    executable_paths: HashSet<String>,
}

impl<'a> GenerationRootView<'a> {
    fn new(manifest: &'a GenerationRootManifest) -> Self {
        let symlinks = manifest
            .entries
            .iter()
            .filter_map(|entry| match &entry.node.source.kind {
                PayloadNodeKind::Symlink { target } => Some((entry.path.clone(), target.clone())),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let regular_paths = manifest
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.node.source.kind,
                    PayloadNodeKind::Regular { .. } | PayloadNodeKind::Hardlink { .. }
                )
            })
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();
        let executable_paths = manifest
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.node.source.kind,
                    PayloadNodeKind::Regular { .. } | PayloadNodeKind::Hardlink { .. }
                ) && entry.node.source.mode & 0o111 != 0
            })
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();
        Self {
            entries: &manifest.entries,
            symlinks,
            regular_paths,
            executable_paths,
        }
    }

    fn resolve(&self, path: &str) -> Option<String> {
        resolve_virtual_path(path, &self.symlinks).ok()
    }

    fn is_regular_file(&self, path: &str) -> bool {
        self.resolve(path)
            .is_some_and(|resolved| self.regular_paths.contains(&resolved))
    }

    fn has_executable_init(&self) -> bool {
        self.resolve("/sbin/init")
            .is_some_and(|resolved| self.executable_paths.contains(&resolved))
    }

    /// Direct child names under `directory`, resolved through manifest symlinks.
    fn child_names(&self, directory: &str) -> Vec<String> {
        let Some(resolved) = self.resolve(directory) else {
            return Vec::new();
        };
        let prefix = format!("{}/", resolved.trim_end_matches('/'));
        self.entries
            .iter()
            .filter_map(|entry| entry.path.strip_prefix(prefix.as_str()))
            .filter(|name| !name.is_empty() && !name.contains('/'))
            .map(str::to_string)
            .collect()
    }

    fn solus_kernel_file(&self, release: &str) -> bool {
        solus_kernel_artifact_name(release)
            .is_some_and(|name| self.is_regular_file(&format!("/boot/{name}")))
    }

    fn kernel_file_present(&self, release: &str) -> bool {
        self.is_regular_file(&format!("/boot/vmlinuz-{release}"))
            || self.is_regular_file(&format!("/lib/modules/{release}/vmlinuz"))
            || self.is_regular_file(&format!("/usr/lib/modules/{release}/vmlinuz"))
            || self.solus_kernel_file(release)
    }

    fn efi_bootloader_present(&self) -> bool {
        self.is_regular_file(&format!("/boot/{ESP_BOOTLOADER_REL}"))
            || self.is_regular_file(&format!("/{SYSTEMD_BOOT_EFI_REL}"))
    }
}

pub(super) fn resolve_virtual_path(
    path: &str,
    symlinks: &HashMap<String, String>,
) -> crate::Result<String> {
    let mut current = normalize_virtual_path(path, "/").ok_or_else(|| {
        crate::Error::InvalidPath(format!("virtual path escapes the generation root: {path}"))
    })?;
    for _ in 0..40 {
        let Some(next) = rewrite_first_symlink_component(&current, symlinks)? else {
            return Ok(current);
        };
        current = next;
    }
    Err(crate::Error::InvalidPath(format!(
        "virtual path exceeds 40 manifest symlink edges: {path}"
    )))
}

fn rewrite_first_symlink_component(
    path: &str,
    symlinks: &HashMap<String, String>,
) -> crate::Result<Option<String>> {
    let components = path
        .trim_start_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    for index in 0..components.len() {
        let prefix = format!("/{}", components[..=index].join("/"));
        let Some(target) = symlinks.get(&prefix) else {
            continue;
        };
        let base = parent_virtual_path(&prefix);
        let mut rewritten = normalize_virtual_path(target, &base).ok_or_else(|| {
            crate::Error::InvalidPath(format!(
                "manifest symlink {prefix} target {target:?} escapes the generation root"
            ))
        })?;
        for component in &components[index + 1..] {
            if rewritten != "/" {
                rewritten.push('/');
            }
            rewritten.push_str(component);
        }
        return normalize_virtual_path(&rewritten, "/")
            .map(Some)
            .ok_or_else(|| {
                crate::Error::InvalidPath(format!(
                    "manifest symlink {prefix} rewrites {path} outside the generation root"
                ))
            });
    }
    Ok(None)
}

fn normalize_virtual_path(path: &str, base: &str) -> Option<String> {
    let combined = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), path)
    };
    let mut components = Vec::new();
    for component in combined.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            component => components.push(component),
        }
    }
    Some(format!("/{}", components.join("/")))
}

fn parent_virtual_path(path: &str) -> String {
    let path = path.trim_end_matches('/');
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::root_manifest::{GENERATION_ROOT_MANIFEST_VERSION, GenerationRootEntry};
    use crate::payload::{
        PayloadContentAuthority, PayloadNode, PayloadNodeKind, ResolvedPayloadNode,
    };

    fn resolved(node: PayloadNode) -> ResolvedPayloadNode {
        ResolvedPayloadNode::from_numeric_source(node).unwrap()
    }

    #[test]
    fn init_detection_resolves_only_manifest_owned_symlinks() {
        let manifest = manifest(vec![
            directory("/usr"),
            directory("/usr/lib"),
            directory("/usr/lib/systemd"),
            regular("/usr/lib/systemd/systemd", 0o755),
            directory("/usr/sbin"),
            symlink("/usr/sbin/init", "../lib/systemd/systemd"),
            symlink("/sbin", "usr/sbin"),
        ]);
        assert!(generation_root_has_init_entrypoint(&manifest));
    }

    #[test]
    fn init_detection_does_not_synthesize_usr_merge_links() {
        let manifest = manifest(vec![
            directory("/usr"),
            directory("/usr/sbin"),
            regular("/usr/sbin/init", 0o755),
        ]);
        assert!(!generation_root_has_init_entrypoint(&manifest));
    }

    #[test]
    fn self_contained_validation_types_a_missing_init_as_no_base_system() {
        let without_init = manifest(vec![
            directory("/usr"),
            directory("/usr/bin"),
            regular("/usr/bin/hello", 0o755),
        ]);
        let error = validate_runtime_generation_root_is_self_contained(&without_init)
            .expect_err("a root without an executable /sbin/init must be refused");
        assert!(matches!(
            error,
            crate::Error::GenerationRootMissingBaseSystem {
                missing: crate::error::MissingBaseSystemPart::MissingInit
            }
        ));
        let with_init = manifest(vec![directory("/sbin"), regular("/sbin/init", 0o755)]);
        validate_runtime_generation_root_is_self_contained(&with_init)
            .expect("a root with an executable /sbin/init must validate");
    }

    #[test]
    fn manifest_with_init_but_no_kernel_types_as_missing_boot_assets() {
        let with_init_only = manifest(vec![directory("/sbin"), regular("/sbin/init", 0o755)]);

        let error = validate_generation_root_host_boot_assets(&with_init_only)
            .expect_err("an executable init without boot assets must be refused");
        assert!(matches!(
            error,
            crate::Error::GenerationRootMissingBaseSystem {
                missing: crate::error::MissingBaseSystemPart::MissingBootAssets
            }
        ));

        validate_generation_root_host_boot_assets(&boot_asset_manifest())
            .expect("the same fixture with the minimal boot assets must pass");
    }

    #[test]
    fn manifest_with_init_and_minimal_boot_assets_passes() {
        let manifest = boot_asset_manifest();

        validate_runtime_generation_root_is_self_contained(&manifest)
            .expect("the minimal boot asset root must be self-contained");
        validate_generation_root_host_boot_assets(&manifest)
            .expect("the stage_test_boot_assets fixture must satisfy host boot assets");
    }

    #[test]
    fn boot_asset_validation_accepts_the_systemd_boot_fallback() {
        let manifest = manifest(vec![
            directory("/boot"),
            regular("/boot/vmlinuz-test-kernel", 0o644),
            directory("/sbin"),
            regular("/sbin/init", 0o755),
            directory("/usr"),
            directory("/usr/lib"),
            directory("/usr/lib/systemd"),
            directory("/usr/lib/systemd/boot"),
            directory("/usr/lib/systemd/boot/efi"),
            regular("/usr/lib/systemd/boot/efi/systemd-bootx64.efi", 0o644),
        ]);

        validate_generation_root_host_boot_assets(&manifest)
            .expect("systemd-boot's installed binary is a valid EFI loader source");
    }

    #[test]
    fn manifest_with_kernel_but_no_efi_loader_types_as_missing_boot_assets() {
        let kernel_only = manifest(vec![
            directory("/boot"),
            regular("/boot/vmlinuz-test-kernel", 0o644),
            directory("/sbin"),
            regular("/sbin/init", 0o755),
        ]);

        let error = validate_generation_root_host_boot_assets(&kernel_only)
            .expect_err("a kernel without any EFI loader must be refused");
        assert!(matches!(
            error,
            crate::Error::GenerationRootMissingBaseSystem {
                missing: crate::error::MissingBaseSystemPart::MissingBootAssets
            }
        ));

        validate_generation_root_host_boot_assets(&boot_asset_manifest())
            .expect("the same fixture with an EFI loader must pass");
    }

    /// Minimal asset set `stage_test_boot_assets` writes for a staged root.
    fn boot_asset_manifest() -> GenerationRootManifest {
        manifest(vec![
            directory("/boot"),
            directory("/boot/EFI"),
            directory("/boot/EFI/BOOT"),
            regular("/boot/EFI/BOOT/BOOTX64.EFI", 0o644),
            regular("/boot/initramfs-test-kernel.img", 0o644),
            regular("/boot/vmlinuz-test-kernel", 0o644),
            directory("/sbin"),
            regular("/sbin/init", 0o755),
        ])
    }

    #[test]
    fn virtual_path_resolution_rejects_escape_and_loops() {
        let escape = HashMap::from([("/lib".to_string(), "../../outside".to_string())]);
        let error = resolve_virtual_path("/lib/modules", &escape).unwrap_err();
        assert!(
            error.to_string().contains("escapes the generation root"),
            "{error}"
        );

        let cycle = HashMap::from([
            ("/first".to_string(), "/second".to_string()),
            ("/second".to_string(), "/first".to_string()),
        ]);
        let error = resolve_virtual_path("/first/tool", &cycle).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("exceeds 40 manifest symlink edges"),
            "{error}"
        );
    }

    fn manifest(entries: Vec<GenerationRootEntry>) -> GenerationRootManifest {
        let mut root = PayloadNode::regular(0o755);
        root.kind = PayloadNodeKind::Directory;
        root.mode = libc::S_IFDIR | 0o755;
        GenerationRootManifest {
            version: GENERATION_ROOT_MANIFEST_VERSION,
            root: resolved(root),
            entries,
        }
    }

    fn directory(path: &str) -> GenerationRootEntry {
        let mut node = PayloadNode::regular(0o755);
        node.kind = PayloadNodeKind::Directory;
        node.mode = libc::S_IFDIR | 0o755;
        GenerationRootEntry {
            path: path.to_string(),
            node: resolved(node),
            content: None,
        }
    }

    fn regular(path: &str, permissions: u32) -> GenerationRootEntry {
        GenerationRootEntry {
            path: path.to_string(),
            node: resolved(PayloadNode::regular(permissions)),
            content: Some(PayloadContentAuthority {
                sha256: "a".repeat(64),
                size: 1,
            }),
        }
    }

    fn symlink(path: &str, target: &str) -> GenerationRootEntry {
        let mut node = PayloadNode::regular(0o777);
        node.kind = PayloadNodeKind::Symlink {
            target: target.to_string(),
        };
        node.mode = libc::S_IFLNK | 0o777;
        GenerationRootEntry {
            path: path.to_string(),
            node: resolved(node),
            content: None,
        }
    }
}
