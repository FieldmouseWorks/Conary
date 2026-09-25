// apps/conary/src/ui/publication.rs
//! Guidance for generation publication that is pending because the selected
//! root has no base system yet.
//!
//! The command boundary selects this text from the typed publication failure.
//! It is never inferred from a rendered error message.

use conary_core::MissingBaseSystemPart;

/// Reason line when the selected root has no executable `/sbin/init`.
pub(crate) const NO_BASE_SYSTEM_MISSING_INIT_REASON: &str = "selected root has no base system yet: no executable /sbin/init, so no generation can be published or booted";

/// Reason line when the selected root has no kernel or boot assets.
pub(crate) const NO_BASE_SYSTEM_MISSING_BOOT_ASSETS_REASON: &str = "selected root has no base system yet: no kernel or boot assets, so no generation can be published or booted";

/// The exact reason line naming the missing base-system part.
pub(crate) fn no_base_system_reason(missing: MissingBaseSystemPart) -> &'static str {
    match missing {
        MissingBaseSystemPart::MissingInit => NO_BASE_SYSTEM_MISSING_INIT_REASON,
        MissingBaseSystemPart::MissingBootAssets => NO_BASE_SYSTEM_MISSING_BOOT_ASSETS_REASON,
    }
}

/// Committed-change reassurance plus the two supported ways to provide the
/// exact missing base-system part.
///
/// The install note names the builder's own boot paths, so a root that already
/// has `/sbin/init` is never told to install init again. `boot_assets.rs`
/// stages its loader from `EFI/BOOT/BOOTX64.EFI` under the generation boot root
/// (shown here as `/boot`) or, when no ESP is staged, from systemd-boot's
/// installed `/usr/lib/systemd/boot/efi/systemd-bootx64.efi`.
pub(crate) fn no_base_system_guidance(missing: MissingBaseSystemPart) -> [&'static str; 3] {
    match missing {
        MissingBaseSystemPart::MissingInit => [
            "The package change is committed and will publish once a base system with an executable /sbin/init is present.",
            "Adopt this machine's native system: conary system adopt --system",
            "Or install a base system that provides /sbin/init from a repository.",
        ],
        MissingBaseSystemPart::MissingBootAssets => [
            "The package change is committed and will publish once a /boot/vmlinuz-<release> kernel and an EFI loader are present.",
            "Adopt this machine's native system: conary system adopt --system",
            "Or install a kernel package that provides /boot/vmlinuz-<release> and an EFI loader at /boot/EFI/BOOT/BOOTX64.EFI or systemd-boot's /usr/lib/systemd/boot/efi/systemd-bootx64.efi.",
        ],
    }
}

/// Render the explicit publish/recover failure body for a no-base debt.
///
/// The retry command is deliberately absent: replaying the same command cannot
/// create a base system, so the operator must adopt or install one first.
pub(crate) fn no_base_system_command_failure(
    context: &str,
    missing: MissingBaseSystemPart,
) -> String {
    let mut lines = vec![
        format!("{context} is still pending."),
        format!("Reason: {}", no_base_system_reason(missing)),
    ];
    for guidance in no_base_system_guidance(missing) {
        lines.push(guidance.to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests;
