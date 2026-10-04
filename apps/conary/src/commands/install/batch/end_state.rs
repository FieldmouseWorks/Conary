// apps/conary/src/commands/install/batch/end_state.rs

//! Locked-batch certification of the fully determined end state.
//!
//! The caller solved the batch's dependencies against installed state read
//! before the mutation lock. Under the lock nothing is left to choose, so the
//! complete end state is `(installed - outgoing) + every prepared package`.
//! Re-certifying every hard requirement group the transaction can observe
//! against that fixed set avoids committing a batch that another transaction
//! broke after the pre-lock solve.

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::commands::install::dependencies::RequirementsChanged;

use super::PreparedPackage;

/// Certify the complete fixed end state of a prepared batch.
///
/// An empty result certifies the end state; any unsatisfied group refuses with
/// the typed [`RequirementsChanged`] error.
pub(super) fn certify_batch_end_state(
    conn: &Connection,
    packages: &[PreparedPackage],
) -> Result<()> {
    if packages.is_empty() {
        anyhow::bail!("cannot certify an empty batch end state");
    }
    let incoming = packages
        .iter()
        .map(fixed_incoming_from_prepared)
        .collect::<Result<Vec<_>>>()?;
    let outgoing = super::resolved_batch_outgoing(packages)?.sorted_ids();
    let unsatisfied = conary_core::resolver::certify_fixed_end_state(conn, &incoming, &outgoing)?;
    if unsatisfied.is_empty() {
        return Ok(());
    }
    let conflict = unsatisfied
        .iter()
        .map(|group| group.description())
        .collect::<Vec<_>>()
        .join("; ");
    Err(RequirementsChanged {
        package: packages
            .last()
            .expect("non-empty batch checked above")
            .name
            .clone(),
        conflict: Some(conflict),
        missing: Vec::new(),
        unsatisfied,
    }
    .into())
}

/// Build one fixed incoming identity from a prepared package's exact fields.
///
/// End-state certification evaluates the prepared set directly, so it takes the
/// same name, version, architecture, version scheme, provides, and requirements
/// the batch commits.
fn fixed_incoming_from_prepared(
    package: &PreparedPackage,
) -> Result<conary_core::resolver::FixedIncomingPackage> {
    conary_core::resolver::FixedIncomingPackage::new(
        package.name.clone(),
        package.version.clone(),
        package.package_release.clone(),
        package.architecture.clone(),
        package.debian_multi_arch,
        package.semantics.version_scheme,
        package.provides.clone(),
        package.requirements.clone(),
    )
    .with_context(|| {
        format!(
            "failed to build fixed incoming identity for '{}'",
            package.name
        )
    })
}
