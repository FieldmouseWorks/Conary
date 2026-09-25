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

/// Committed-change reassurance plus the two supported ways to provide a base.
pub(crate) const NO_BASE_SYSTEM_GUIDANCE: [&str; 3] = [
    "The package change is committed and will publish once a base system is present.",
    "Adopt this machine's native system: conary system adopt --system",
    "Or install a base system that provides /sbin/init from a repository.",
];

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
    for guidance in NO_BASE_SYSTEM_GUIDANCE {
        lines.push(guidance.to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests;
