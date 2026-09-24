// apps/conary/src/commands/generation/selected_root/overlay_session.rs

//! Transaction-owned OverlayFS lifetime for a selected-root session.
//!
//! The merged mount and every immutable lower stay under the session directory.
//! The writable `upper` and `work` directories live wherever the functional
//! scratch probe proved an OverlayFS upper can live for this runtime root.

use crate::commands::DeferredOverlayDurability;
use anyhow::{Context, Result};
use conary_core::filesystem::CasStore;
use conary_core::generation::artifact::GenerationArtifact;
use conary_core::generation::mount::{MountOptions, mount_generation};
use conary_core::generation::root_manifest::{
    CapturedSelectedRoot, MountedScratchTmpfs, MountedSelectedRootOverlay, OverlayScratchPlacement,
    SelectedRootManifestDelta, SelectedRootOverlayCapabilities, SelectedRootOverlayProfile,
    SelectedRootOverlayScratch, SelectedRootSnapshot, apply_resolved_payload_metadata,
    decode_selected_root_overlay_upper_indexed, encode_selected_root_overlay_upper_node,
    materialize_state_root, select_selected_root_overlay_scratch,
};
use nix::mount::{MntFlags, umount2};
use std::fs;
use std::path::{Path, PathBuf};

/// A mounted merged root whose only writable authority is `upper`.
pub(super) struct SelectedRootOverlaySession {
    selected_root: PathBuf,
    upper: PathBuf,
    mounted: Option<MountedSelectedRootOverlay>,
    generation_lower: Option<MountedGenerationLower>,
    capabilities: SelectedRootOverlayCapabilities,
    scratch_tmpfs: Option<MountedScratchTmpfs>,
}

/// One composefs generation mounted only for the lifetime of a transaction.
struct MountedGenerationLower {
    mount_point: PathBuf,
    mounted: bool,
}

impl SelectedRootOverlaySession {
    /// Functionally prove the exact transaction OverlayFS profile before any
    /// selected-root snapshot or package authority can be mutated.
    pub(super) fn preflight(session_dir: &Path) -> Result<SelectedRootOverlayScratch> {
        let profile = SelectedRootOverlayProfile::trusted();
        select_selected_root_overlay_scratch(session_dir, &profile).with_context(|| {
            format!(
                "selected-root OverlayFS capability preflight failed on {}; recovery: boot a kernel with OverlayFS support and run Conary with mount and trusted-xattr privileges on a compatible workspace filesystem or with permission to mount a private tmpfs",
                session_dir.display()
            )
        })
    }

    /// Probe the actual workspace, admit the materialized immutable lower, and
    /// mount the exact profile before lifecycle mutation can begin.
    pub(super) fn begin_materialized(
        session_dir: &Path,
        prior: &CapturedSelectedRoot,
        scratch: SelectedRootOverlayScratch,
    ) -> Result<Self> {
        Self::begin_with_lowers(
            session_dir,
            prior,
            &[session_dir.join("lower")],
            None,
            scratch,
        )
    }

    /// Mount the current generation image directly beneath its typed mutable
    /// state instead of reconstructing immutable CAS payload bytes.
    pub(super) fn begin_current_generation(
        session_dir: &Path,
        prior: &CapturedSelectedRoot,
        artifact: &GenerationArtifact,
        cas: &CasStore,
        scratch: SelectedRootOverlayScratch,
    ) -> Result<Self> {
        fs::create_dir_all(session_dir)?;
        let state_lower = session_dir.join("lower-state");
        fs::create_dir(&state_lower).with_context(|| {
            format!(
                "failed to create selected-root mutable-state lower {}",
                state_lower.display()
            )
        })?;
        materialize_state_root(&prior.state, cas, &state_lower)
            .context("failed to materialize selected-root mutable-state lower")?;
        apply_resolved_payload_metadata(&state_lower, &prior.generation.root)
            .context("failed to restore selected-root lower root metadata")?;
        fs::File::open(&state_lower)?.sync_all()?;

        let generation_mount = session_dir.join("lower-generation");
        fs::create_dir(&generation_mount).with_context(|| {
            format!(
                "failed to create selected-root generation lower {}",
                generation_mount.display()
            )
        })?;
        let generation_lower = MountedGenerationLower::mount(artifact, generation_mount)
            .context("failed to mount current generation as selected-root lower")?;
        tracing::info!(
            generation = artifact.generation,
            immutable_entries = prior.generation.entries.len(),
            mutable_state_entries = prior.state.entries.len(),
            "mounted current generation directly as selected-root immutable lower"
        );
        Self::begin_with_lowers(
            session_dir,
            prior,
            &[state_lower, generation_lower.mount_point.clone()],
            Some(generation_lower),
            scratch,
        )
    }

    fn begin_with_lowers(
        session_dir: &Path,
        prior: &CapturedSelectedRoot,
        lowers: &[PathBuf],
        generation_lower: Option<MountedGenerationLower>,
        scratch: SelectedRootOverlayScratch,
    ) -> Result<Self> {
        let (capabilities, scratch_directory, scratch_tmpfs) = scratch.into_parts();
        fs::create_dir_all(session_dir).with_context(|| {
            format!(
                "failed to create selected-root overlay workspace {}",
                session_dir.display()
            )
        })?;
        let profile = SelectedRootOverlayProfile::trusted();

        let upper = scratch_directory.join("upper");
        let work = scratch_directory.join("work");
        let selected_root = session_dir.join("root");
        for lower in lowers {
            if !lower.is_dir() {
                anyhow::bail!(
                    "selected-root immutable lower authority is missing at {}",
                    lower.display()
                );
            }
        }
        for directory in [&upper, &work, &selected_root] {
            fs::create_dir(directory).with_context(|| {
                format!(
                    "failed to create selected-root overlay directory {}",
                    directory.display()
                )
            })?;
        }
        // The upper directory itself represents the merged root node. Seeding
        // it prevents an unchanged root from becoming a synthetic delta.
        let upper_root = encode_selected_root_overlay_upper_node(&prior.generation.root, &profile)
            .context("failed to encode selected-root overlay root metadata")?;
        apply_resolved_payload_metadata(&upper, &upper_root)
            .context("failed to seed selected-root overlay root metadata")?;
        let lower_refs = lowers.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let mounted = MountedSelectedRootOverlay::mount_lowers(
            &lower_refs,
            &upper,
            &work,
            &selected_root,
            &profile,
        )
        .context("failed to mount selected-root OverlayFS session")?;

        let session = Self {
            selected_root,
            upper,
            mounted: Some(mounted),
            generation_lower,
            capabilities,
            scratch_tmpfs,
        };
        tracing::info!(
            scratch_placement = ?session.scratch_placement(),
            "mounted selected-root OverlayFS session"
        );
        Ok(session)
    }

    pub(super) fn selected_root(&self) -> &Path {
        &self.selected_root
    }

    pub(super) fn scratch_placement(&self) -> OverlayScratchPlacement {
        self.capabilities.scratch_placement
    }

    /// Freeze the upper with a strict unmount, then decode changed paths only.
    pub(super) fn freeze_and_decode(
        &mut self,
        conn: &rusqlite::Connection,
        prior: SelectedRootSnapshot,
        cas: &CasStore,
        durability: DeferredOverlayDurability,
    ) -> Result<SelectedRootManifestDelta> {
        let metrics = durability.metrics();
        self.mounted
            .take()
            .context("selected-root OverlayFS session is already frozen")?
            .freeze(&self.upper)
            .context("failed to freeze selected-root OverlayFS session")?;
        tracing::info!(
            deferred_file_syncs = metrics.deferred_file_syncs,
            deferred_directory_syncs = metrics.deferred_directory_syncs,
            filesystem_freezes = 1_u64,
            "froze deferred selected-root mutation authority"
        );
        self.unmount_generation_lower()?;
        let delta = decode_selected_root_overlay_upper_indexed(
            &self.upper,
            conn,
            prior,
            cas,
            &self.capabilities.profile,
        )
        .context("failed to decode selected-root OverlayFS upper")?;
        if let Some(tmpfs) = self.scratch_tmpfs.take() {
            tmpfs
                .unmount()
                .context("failed to unmount selected-root scratch tmpfs")?;
        }
        Ok(delta)
    }

    /// Strictly unmount without decoding; the caller then discards the
    /// transaction directory and the prior manifest remains authoritative.
    pub(super) fn unmount_for_discard(&mut self) -> Result<()> {
        // Attempt every unmount in stacking order, then report the first failure.
        let overlay = self.mounted.take().map_or(Ok(()), |mounted| {
            mounted
                .unmount()
                .context("failed to unmount discarded selected-root OverlayFS session")
        });
        let generation_lower = self.unmount_generation_lower();
        let scratch = self.scratch_tmpfs.take().map_or(Ok(()), |tmpfs| {
            tmpfs
                .unmount()
                .context("failed to unmount discarded selected-root scratch tmpfs")
        });
        overlay.and(generation_lower).and(scratch)
    }

    fn unmount_generation_lower(&mut self) -> Result<()> {
        if let Some(lower) = &mut self.generation_lower {
            lower.unmount()?;
        }
        Ok(())
    }
}

impl MountedGenerationLower {
    fn mount(artifact: &GenerationArtifact, mount_point: PathBuf) -> Result<Self> {
        let requested_verity = super::super::builder::requested_generation_verity(
            artifact.metadata.erofs_verity_digest.as_deref(),
            artifact.metadata.fsverity_enabled,
        );
        mount_generation(&MountOptions {
            image_path: artifact.erofs_path.clone(),
            basedir: artifact.cas_dir.clone(),
            mount_point: mount_point.clone(),
            verity: requested_verity,
            digest: requested_verity
                .then(|| artifact.metadata.erofs_verity_digest.clone())
                .flatten(),
            upperdir: None,
            workdir: None,
        })?;
        Ok(Self {
            mount_point,
            mounted: true,
        })
    }

    fn unmount(&mut self) -> Result<()> {
        if !self.mounted {
            return Ok(());
        }
        umount2(&self.mount_point, MntFlags::empty()).with_context(|| {
            format!(
                "failed to strictly unmount selected-root generation lower {}",
                self.mount_point.display()
            )
        })?;
        self.mounted = false;
        Ok(())
    }
}

impl Drop for MountedGenerationLower {
    fn drop(&mut self) {
        if self.mounted {
            let _ = umount2(&self.mount_point, MntFlags::MNT_DETACH);
        }
    }
}
