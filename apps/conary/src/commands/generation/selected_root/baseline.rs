// apps/conary/src/commands/generation/selected_root/baseline.rs

//! Read-only selected-root baseline selection and capture.
//!
//! This is the read-only half of selection, shared by `system root inspect` and
//! the CCS dry-run baseline. It takes the same artifact-versus-database
//! authority decision as writable preparation but never acquires the runtime
//! mutation lock, creates a session directory, or writes under the runtime root.

use anyhow::{Context, Result};
use conary_core::db::models::{GenerationPublication, SystemState, TrySession, TrySessionStatus};
use conary_core::generation::root_manifest::CapturedSelectedRoot;
use conary_core::runtime_root::ConaryRuntimeRoot;

use super::{
    InstalledDatabaseAuthority, SelectedRootSelection, create_selected_root_stand_in,
    select_selected_root, use_materialized_selected_root_backing,
};

/// The typed authority that supplied a read-only selected-root baseline.
///
/// This is the one derivation of which baseline source the read-only preview
/// and `system root inspect` report; callers never recompute it from package
/// rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectedRootSource {
    /// A pending publication debt owns an already-captured typed baseline.
    PendingSnapshot {
        snapshot_id: i64,
        changeset_id: Option<i64>,
    },
    /// The current generation artifact carries the baseline as typed manifests.
    CurrentGeneration {
        snapshot_id: Option<i64>,
        changeset_id: Option<i64>,
        /// True when a stable `/current` link named a generation the pinned
        /// snapshot never recorded, so the IDs are unknown rather than absent.
        recovered_without_state: bool,
    },
    /// No generation exists yet, so installed database rows are the baseline.
    DatabaseProjection { changeset_id: Option<i64> },
}

/// The typed read-only selected-root baseline a real install would prepare.
///
/// With no committed authority there is no capture, so a caller cannot mistake
/// a fabricated empty root for committed state. Only [`Self::Captured`] carries
/// a [`CapturedSelectedRoot`].
#[derive(Debug)]
pub(crate) enum SelectedRootBaseline {
    /// A committed or installed authority supplied this exact capture.
    Captured {
        source: SelectedRootSource,
        captured: Box<CapturedSelectedRoot>,
    },
    /// No generation, snapshot, or installed trove exists, so there is no
    /// committed root and no capture to report.
    NoCommittedRoot,
}

/// How many times a selection may be redone after `/current` moves past the
/// pinned SQLite snapshot before the read is refused.
const MAX_CURRENT_GENERATION_ATTEMPTS: usize = 3;

/// Typed refusal for a `/current` link that outran its pinned snapshot.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SelectedRootBaselineError {
    /// Every attempt read a `/current` generation the pinned snapshot did not
    /// yet record, so no attempt could pair the artifact with its own database
    /// authority.
    #[error("the current generation changed during inspection; retry")]
    CurrentGenerationChanged,
    /// An active or orphaned try session owns the state-less `/current`
    /// generation; the link is an uncommitted trial, not recovered.
    #[error("a try session owns uncommitted /current; run conary try keep or conary try rollback")]
    TrySessionOwnsCurrent,
    /// A rolled-back try session still owns the state-less `/current`
    /// generation. `rollback_active_try_session` leaves the link on the try
    /// generation when the session has no previous generation to restore, so
    /// the link outlived the discarded trial and is not a committed baseline.
    #[error("the current generation is a rolled-back try session, not a committed generation")]
    TrySessionRolledBack,
}

/// One attempt at the baseline, or the typed signal to re-pin and retry.
enum BaselineAttempt {
    Complete(SelectedRootBaseline),
    /// `/current` named a generation the pinned snapshot did not record.
    StaleCurrentGeneration,
}

/// Read the exact typed selected-root baseline a real install would prepare.
///
/// This is the read-only half of `prepare_current_root`: it takes the same
/// artifact-versus-database decision but never acquires the runtime mutation
/// lock, creates a session directory, or writes under the runtime root.
///
/// The source is the typed selection result, not a recomputation from package
/// rows, so reporting and preparation cannot disagree about the authority.
///
/// Artifact- and pending-snapshot-backed reads need no temporary write access.
/// A present database projection creates a private [`tempfile::TempDir`] and
/// makes the empty stand-in inside it with [`create_selected_root_stand_in`].
/// An absent one has no committed root at all, so it is
/// [`SelectedRootBaseline::NoCommittedRoot`] with no capture, temp directory, or
/// collection: nothing fabricated stands in for authority that was never
/// recorded. The stand-in is read only for root metadata and any
/// package-unclaimed parent closure, and the returned capture holds manifest
/// values only, never a path into that directory, so the temp directory is
/// dropped before returning. The active generation config-state upper is
/// deliberately not projected. Its capture requires content writes into the
/// runtime CAS and selected-root snapshot writes, which a preview must not
/// perform.
///
/// Selection and collection run inside one deferred read transaction; without
/// it a concurrent install that commits between the selecting query and the
/// collecting reads can produce a source that disagrees with its capture
/// (`DatabaseProjection` with an empty one). WAL mode pins one snapshot at the
/// transaction's first read.
///
/// `/current` is a filesystem link the database snapshot cannot pin. A
/// publication commits its state and terminal publication rows before swapping
/// the link, so the current-generation branch verifies both are recorded for
/// the selected generation and retries the whole selection in a fresh snapshot
/// (at most [`MAX_CURRENT_GENERATION_ATTEMPTS`] times) when they are not.
/// Exhausting the attempts is a typed
/// [`SelectedRootBaselineError::CurrentGenerationChanged`].
///
/// A stable `/current` link to a generation the snapshot never recorded is a
/// recovered state-less target only when no try session claims it. An active or
/// orphaned session refuses with
/// [`SelectedRootBaselineError::TrySessionOwnsCurrent`], and a rolled-back
/// session refuses with [`SelectedRootBaselineError::TrySessionRolledBack`].
/// A kept session is the operator's promotion decision and does not refuse:
/// `try keep` records the generation's commitment, so recovery may proceed.
pub(crate) fn read_selected_root_baseline(
    conn: &rusqlite::Connection,
    runtime_root: &ConaryRuntimeRoot,
) -> Result<SelectedRootBaseline> {
    // A caller may already hold a transaction or savepoint. Reuse that
    // snapshot rather than attempting a nested BEGIN, which SQLite rejects.
    // Its pin cannot be bracketed, so an unrecorded generation stays refused.
    if !conn.is_autocommit() {
        return match read_baseline_attempt(conn, runtime_root, None)? {
            BaselineAttempt::Complete(baseline) => Ok(baseline),
            BaselineAttempt::StaleCurrentGeneration => {
                Err(SelectedRootBaselineError::CurrentGenerationChanged.into())
            }
        };
    }

    let mut attempt = 0;
    loop {
        attempt += 1;
        // Bracket the WAL snapshot: sample `/current` before the pin and after
        // selection; a state-less target needs every read to agree. The sample
        // never fails the read: a pending snapshot ignores `/current` entirely.
        let current_before = Some(sample_current_generation_link(runtime_root));
        let transaction = conn.unchecked_transaction()?;
        match read_baseline_attempt(&transaction, runtime_root, current_before) {
            Ok(BaselineAttempt::Complete(baseline)) => {
                transaction.commit()?;
                return Ok(baseline);
            }
            Ok(BaselineAttempt::StaleCurrentGeneration)
                if attempt < MAX_CURRENT_GENERATION_ATTEMPTS =>
            {
                // Dropping the transaction rolls the pinned snapshot back so
                // the next attempt can observe the publication that moved
                // `/current`.
                drop(transaction);
            }
            Ok(BaselineAttempt::StaleCurrentGeneration) => {
                return Err(SelectedRootBaselineError::CurrentGenerationChanged.into());
            }
            Err(error) => return Err(error),
        }
    }
}

/// Read `/current` as a generation number, if any.
fn current_generation_link(runtime_root: &ConaryRuntimeRoot) -> conary_core::Result<Option<i64>> {
    conary_core::generation::mount::current_generation(runtime_root.root())
}

/// One non-failing sample of the `/current` link.
///
/// `Unreadable` stays distinct from `Absent`: a state-less generation is never
/// stable against it, so it retries and can still refuse instead of accepting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurrentLinkSample {
    Generation(i64),
    Absent,
    Unreadable,
}

/// Sample `/current` without ever failing the baseline read.
fn sample_current_generation_link(runtime_root: &ConaryRuntimeRoot) -> CurrentLinkSample {
    match current_generation_link(runtime_root) {
        Ok(Some(generation)) => CurrentLinkSample::Generation(generation),
        Ok(None) => CurrentLinkSample::Absent,
        Err(_) => CurrentLinkSample::Unreadable,
    }
}

/// One attempt at the baseline against a snapshot pinned by the caller.
///
/// `current_before` is the typed `/current` sample taken before the snapshot
/// was pinned, or `None` when the caller already owned it. A state-less
/// generation is stable only when both samples name it, never when unreadable.
/// Boot recovery (`mark_generation_state_active_if_present` in
/// `crates/conary-core/src/transaction/recovery.rs`) writes no terminal
/// `GenerationPublication` row; a concurrent publication commits its state and
/// publication rows before moving the link.
fn read_baseline_attempt(
    conn: &rusqlite::Connection,
    runtime_root: &ConaryRuntimeRoot,
    current_before: Option<CurrentLinkSample>,
) -> Result<BaselineAttempt> {
    let selection = select_selected_root(
        conn,
        runtime_root,
        use_materialized_selected_root_backing(),
        conary_core::generation::composefs::probe_composefs_mount_runtime,
    )?;
    run_between_selection_and_collection_hook();
    Ok(match selection {
        SelectedRootSelection::PendingPublication {
            snapshot,
            captured,
            changeset_id,
        } => BaselineAttempt::Complete(SelectedRootBaseline::Captured {
            source: SelectedRootSource::PendingSnapshot {
                snapshot_id: snapshot.id(),
                changeset_id,
            },
            captured,
        }),
        SelectedRootSelection::CurrentGeneration {
            artifact,
            generation,
            ..
        } => {
            // The generation's state snapshot and terminal publication row are
            // committed with the artifact and before `/current` moves, so a
            // snapshot that records neither normally proves the link advanced
            // past it. The state snapshot also covers generations selected
            // through `generation switch` without a publication row.
            let publication = GenerationPublication::completed_for_generation(conn, generation)?;
            if publication.is_none() && SystemState::find_by_number(conn, generation)?.is_none() {
                let stable = current_before == Some(CurrentLinkSample::Generation(generation))
                    && sample_current_generation_link(runtime_root)
                        == CurrentLinkSample::Generation(generation);
                if !stable {
                    return Ok(BaselineAttempt::StaleCurrentGeneration);
                }
                // A try session records the generation it built, in every
                // status. An active or orphaned session has not decided yet,
                // and a rolled-back session discarded the trial; neither is
                // recovery. A kept session is the operator's promotion
                // decision, so it does not refuse.
                if let Some(session) = TrySession::find_by_try_generation(conn, generation)? {
                    match session.status {
                        TrySessionStatus::Active | TrySessionStatus::Orphaned => {
                            return Err(SelectedRootBaselineError::TrySessionOwnsCurrent.into());
                        }
                        TrySessionStatus::RolledBack => {
                            return Err(SelectedRootBaselineError::TrySessionRolledBack.into());
                        }
                        // A namespace keep promotes the copied database into
                        // the live one and marks the generation's `SystemState`
                        // active (`keep_active_try_session`), so a kept
                        // generation normally has committed state and never
                        // reaches this branch. An activated keep records only
                        // the resolved `Kept` decision; that typed record is
                        // the commit authority here.
                        TrySessionStatus::Kept => {}
                    }
                }
                // The IDs are unknown, not absent; the artifact is the baseline.
                return Ok(BaselineAttempt::Complete(SelectedRootBaseline::Captured {
                    source: SelectedRootSource::CurrentGeneration {
                        snapshot_id: None,
                        changeset_id: None,
                        recovered_without_state: true,
                    },
                    captured: Box::new(CapturedSelectedRoot {
                        generation: artifact.generation_root.clone(),
                        state: artifact.mutable_state.clone(),
                    }),
                }));
            }
            let snapshot_id =
                GenerationPublication::selected_root_snapshot_for_generation(conn, generation)?;
            let changeset_id =
                publication.and_then(|publication| publication.published_through_changeset_id);
            BaselineAttempt::Complete(SelectedRootBaseline::Captured {
                source: SelectedRootSource::CurrentGeneration {
                    snapshot_id,
                    changeset_id,
                    recovered_without_state: false,
                },
                captured: Box::new(CapturedSelectedRoot {
                    generation: artifact.generation_root.clone(),
                    state: artifact.mutable_state.clone(),
                }),
            })
        }
        SelectedRootSelection::DatabaseProjection { installed } => {
            if installed == InstalledDatabaseAuthority::Absent {
                return Ok(BaselineAttempt::Complete(
                    SelectedRootBaseline::NoCommittedRoot,
                ));
            }
            // Only the present projection needs a private temp parent.
            let stand_in_parent = tempfile::TempDir::new()
                .context("failed to create the private selected-root projection directory")?;
            let empty_root = create_selected_root_stand_in(stand_in_parent.path())?;
            let captured =
                conary_core::generation::builder::collect_selected_root_from_db_with_authority(
                    conn,
                    &empty_root,
                )
                .map_err(anyhow::Error::from)?;
            drop(stand_in_parent);
            BaselineAttempt::Complete(SelectedRootBaseline::Captured {
                source: SelectedRootSource::DatabaseProjection {
                    changeset_id: GenerationPublication::applied_high_water_changeset_id(conn)?,
                },
                captured: Box::new(captured),
            })
        }
    })
}

#[cfg(test)]
// Test-only seam between the selecting query and the collecting reads.
//
// A test arms this to commit on another connection exactly where an
// autocommit implementation would open a second snapshot, then proves the
// capture still reflects the selection's snapshot.
thread_local! {
    static BETWEEN_SELECTION_AND_COLLECTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn set_between_selection_and_collection_hook(hook: impl FnOnce() + 'static) {
    BETWEEN_SELECTION_AND_COLLECTION.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn run_between_selection_and_collection_hook() {
    let hook = BETWEEN_SELECTION_AND_COLLECTION.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(not(test))]
fn run_between_selection_and_collection_hook() {}

#[cfg(test)]
// Test-only seam between the pinned SQLite snapshot and the `/current` read.
//
// A test arms this to publish a newer generation on another connection exactly
// where the filesystem link can advance past the snapshot, then proves the
// selection retries against a fresh snapshot. Unlike the selection-to-collection
// seam this hook is a plain `Fn` so one attempt's hook can observe every retry.
thread_local! {
    static BEFORE_CURRENT_SELECTION: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn set_before_current_selection_hook(hook: impl Fn() + 'static) {
    BEFORE_CURRENT_SELECTION.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(crate) fn clear_before_current_selection_hook() {
    BEFORE_CURRENT_SELECTION.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(test)]
pub(super) fn run_before_current_selection_hook() {
    BEFORE_CURRENT_SELECTION.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook();
        }
    });
}

#[cfg(not(test))]
pub(super) fn run_before_current_selection_hook() {}
