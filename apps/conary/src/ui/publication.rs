// apps/conary/src/ui/publication.rs
//! Guidance for generation publication that is pending because the selected
//! root has no base system yet.
//!
//! The command boundary selects this text from the typed publication failure.
//! It is never inferred from a rendered error message.

/// Why publication cannot proceed: the selected root has no executable init.
pub(crate) const NO_BASE_SYSTEM_REASON: &str = "selected root has no base system yet: no executable /sbin/init, so no generation can be published or booted";

/// Committed-change reassurance plus the two supported ways to provide a base.
pub(crate) const NO_BASE_SYSTEM_GUIDANCE: [&str; 3] = [
    "The package change is committed and will publish once a base system is present.",
    "Adopt this machine's native system: conary system adopt --system",
    "Or install a base system that provides /sbin/init from a repository.",
];
