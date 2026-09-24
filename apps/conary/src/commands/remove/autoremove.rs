// apps/conary/src/commands/remove/autoremove.rs

use anyhow::{Context, Result};
use conary_core::db::models::{PackagePayloadOwnership, Trove};
use conary_core::scriptlet::ExecutionMode;
use rusqlite::Connection;
use std::collections::{BTreeSet, HashSet};
use tracing::info;

use self::plan_output::{AutoremovePlanData, AutoremoveSkipReason, plan_result};
use super::types::RemoveLifecycleOptions;
use crate::commands::{SandboxMode, open_db};

/// Maximum fixed-point rounds apply mode and the preview planner run.
const MAX_AUTOREMOVE_ITERATIONS: usize = 100;

#[derive(Debug, Clone)]
pub(super) struct AutoremovePlan {
    pub(super) removable: Vec<Trove>,
    pub(super) skipped: Vec<(Trove, AutoremoveSkipReason)>,
}

/// One round of the fixed-point preview plan.
#[derive(Debug, Clone)]
struct AutoremoveFixedPointRound {
    /// 1-based round number.
    round: u32,
    removable: Vec<Trove>,
}

/// The fixed point apply mode would reach, computed without mutating anything.
///
/// Every recorded round has at least one removable trove, so the rounds are
/// numbered `1..=rounds.len()` without gaps.
#[derive(Debug, Clone)]
pub(super) struct AutoremoveFixedPointPlan {
    rounds: Vec<AutoremoveFixedPointRound>,
    skipped: Vec<(Trove, AutoremoveSkipReason)>,
}

/// What `cmd_autoremove` does with the orphan plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoremoveMode {
    /// Remove the planned orphans.
    Apply,
    /// Print the plan as human-readable text without removing anything.
    PreviewText,
    /// Print the plan as a typed JSON document without removing anything.
    PreviewJson,
}

/// Remove orphaned packages (installed as dependencies but no longer needed)
///
/// Finds packages that were installed as dependencies of other packages,
/// but are no longer required by any installed package.
pub fn cmd_autoremove(
    db_path: &str,
    mode: AutoremoveMode,
    sandbox_mode: SandboxMode,
) -> Result<()> {
    info!("Finding orphaned packages...");

    let conn = open_db(db_path)?;

    if mode != AutoremoveMode::Apply {
        let plan = plan_autoremove_fixed_point(&conn)?;
        return preview_autoremove(&plan, mode);
    }

    // Apply mode reflects real removal outcomes, so it re-queries after every
    // round instead of using the simulated fixed point.
    let orphans = Trove::find_orphans(&conn)?;
    let orphans_empty = orphans.is_empty();
    let plan = plan_autoremove(orphans);

    if orphans_empty {
        println!("No orphaned packages found.");
        return Ok(());
    }

    if plan.removable.is_empty() {
        println!("No Conary-owned orphaned packages can be autoremoved.");
        print_autoremove_skips(&plan.skipped);
        return Ok(());
    }
    print_autoremove_candidates("Found", &plan.removable);
    print_autoremove_skips(&plan.skipped);

    // Fixed-point iteration: removing orphans may expose new orphans (transitive chains).
    // Re-query after each round until no more orphans are found.
    let mut total_removed = 0;
    let mut total_failed = 0;
    let mut current_plan = plan;
    let mut failed_orphans = HashSet::new();

    for iteration in 0..MAX_AUTOREMOVE_ITERATIONS {
        if iteration > 0 {
            // Re-query orphans after previous round of removals
            let conn = open_db(db_path)?;
            let current_orphans = Trove::find_orphans(&conn)?;
            if current_orphans.is_empty() {
                break;
            }
            current_plan = plan_autoremove(current_orphans);
            current_plan
                .removable
                .retain(|trove| !failed_orphans.contains(&autoremove_identity(trove)));
            if current_plan.removable.is_empty() {
                println!("\nNo additional Conary-owned orphaned packages can be autoremoved.");
                print_autoremove_skips(&current_plan.skipped);
                break;
            }
            print_autoremove_candidates("Found additional", &current_plan.removable);
            print_autoremove_skips(&current_plan.skipped);
        } else {
            println!(
                "\nRemoving {} orphaned package(s)...",
                current_plan.removable.len()
            );
        }

        let mut conn = open_db(db_path)?;
        preflight_autoremove_round(
            &mut conn,
            &current_plan.removable,
            db_path,
            RemoveLifecycleOptions::new(sandbox_mode),
        )?;

        let mut round_removed = 0;
        for trove in &current_plan.removable {
            println!("\nRemoving {} {}...", trove.name, trove.version);
            match super::cmd_remove(
                &trove.name,
                db_path,
                Some(trove.version.clone()),
                trove.architecture.clone(),
                sandbox_mode,
                false,
            ) {
                Ok(()) => {
                    round_removed += 1;
                }
                Err(e) => {
                    eprintln!("  Failed to remove {}: {}", trove.name, e);
                    failed_orphans.insert(autoremove_identity(trove));
                    total_failed += 1;
                }
            }
        }

        total_removed += round_removed;

        // If nothing was removed this round, no point continuing
        if round_removed == 0 {
            break;
        }
    }

    println!("\nAutoremove complete:");
    println!("  Removed: {} package(s)", total_removed);
    if total_failed > 0 {
        println!("  Failed: {} package(s)", total_failed);
        anyhow::bail!(
            "Autoremove failed for {} package(s); see summary above",
            total_failed
        );
    }

    Ok(())
}

/// Print a preview plan without mutating anything.
fn preview_autoremove(plan: &AutoremoveFixedPointPlan, mode: AutoremoveMode) -> Result<()> {
    if mode == AutoremoveMode::PreviewJson {
        let data = AutoremovePlanData::from_fixed_point(plan);
        let result = plan_result(&data)?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    if plan.rounds.is_empty() {
        if plan.skipped.is_empty() {
            println!("No orphaned packages found.");
        } else {
            println!("No Conary-owned orphaned packages can be autoremoved.");
            print_autoremove_skips(&plan.skipped);
        }
        return Ok(());
    }

    for (index, round) in plan.rounds.iter().enumerate() {
        let prefix = if index == 0 {
            "Found"
        } else {
            "Found additional"
        };
        print_autoremove_candidates(prefix, &round.removable);
    }
    print_autoremove_skips(&plan.skipped);
    println!("\nDry run - no packages will be removed.");
    println!("Run without --dry-run to remove these packages.");

    Ok(())
}

/// Simulate apply's fixed point without mutating anything.
fn plan_autoremove_fixed_point(conn: &Connection) -> Result<AutoremoveFixedPointPlan> {
    let mut removed: BTreeSet<i64> = BTreeSet::new();
    let mut skipped: Vec<(Trove, AutoremoveSkipReason)> = Vec::new();
    let mut skipped_ids: BTreeSet<i64> = BTreeSet::new();
    let mut rounds: Vec<AutoremoveFixedPointRound> = Vec::new();

    for iteration in 0..MAX_AUTOREMOVE_ITERATIONS {
        let orphans = Trove::find_orphans_after_removing(conn, &removed)?
            .into_iter()
            .filter(|trove| {
                trove
                    .id
                    .is_none_or(|trove_id| !skipped_ids.contains(&trove_id))
            })
            .collect::<Vec<_>>();
        let AutoremovePlan {
            removable,
            skipped: round_skipped,
        } = plan_autoremove(orphans);

        for (trove, reason) in round_skipped {
            if let Some(trove_id) = trove.id {
                skipped_ids.insert(trove_id);
            }
            skipped.push((trove, reason));
        }

        if removable.is_empty() {
            return Ok(AutoremoveFixedPointPlan { rounds, skipped });
        }

        let round = u32::try_from(iteration + 1)
            .map_err(|_| anyhow::anyhow!("autoremove round number exceeded u32"))?;
        removed.extend(removable.iter().filter_map(|trove| trove.id));
        rounds.push(AutoremoveFixedPointRound { round, removable });
    }

    anyhow::bail!(
        "autoremove plan did not reach a fixed point after {MAX_AUTOREMOVE_ITERATIONS} iterations"
    )
}

fn preflight_autoremove_round(
    conn: &mut rusqlite::Connection,
    troves: &[Trove],
    db_path: &str,
    lifecycle_options: RemoveLifecycleOptions,
) -> Result<()> {
    for trove in troves {
        let Some(trove_id) = trove.id else {
            anyhow::bail!(
                "autoremove lifecycle execution preflight failed for {} {}: trove has no id",
                trove.name,
                trove.version
            );
        };
        let locked_root =
            crate::commands::generation::selected_root::LockedRuntimeRoot::acquire(db_path)?;
        let paths = PackagePayloadOwnership::load(conn, trove_id)?
            .lifecycle_paths()
            .to_vec();
        let native_transaction =
            crate::commands::install::native_events::PreparedNativeTransaction::prepare_remove(
                conn,
                trove_id,
                &trove.name,
                &trove.version,
                paths,
                false,
            );
        let native_transaction = native_transaction.with_context(|| {
            format!(
                "autoremove lifecycle execution preflight failed for {} {}",
                trove.name, trove.version
            )
        })?;
        // This pass is read-only even when root materialization seeds baseline
        // authority. Actual removals prepare their own transaction roots later.
        let preflight_state = conn.savepoint()?;
        let selected = locked_root.prepare(
            &preflight_state,
            format!("Autoremove preflight {}-{}", trove.name, trove.version),
        )?;
        native_transaction
            .preflight(selected.selected_root(), &ExecutionMode::Remove)
            .with_context(|| {
                format!(
                    "autoremove lifecycle execution preflight failed for {} {}",
                    trove.name, trove.version
                )
            })?;
        let selected_root = selected
            .selected_root()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("autoremove selected root is not valid UTF-8"))?;
        super::preflight_ccs_remove_hook(
            &preflight_state,
            trove,
            selected_root,
            lifecycle_options.sandbox_mode,
        )?;
        preflight_state.finish()?;
        drop(selected);
    }

    Ok(())
}

fn plan_autoremove(orphaned: Vec<Trove>) -> AutoremovePlan {
    let mut removable = Vec::new();
    let mut skipped = Vec::new();

    for trove in orphaned {
        if trove.install_source.is_adopted() {
            skipped.push((trove, AutoremoveSkipReason::AdoptedNativeAuthority));
        } else if trove.pinned {
            skipped.push((trove, AutoremoveSkipReason::Pinned));
        } else {
            removable.push(trove);
        }
    }

    AutoremovePlan { removable, skipped }
}

fn print_autoremove_candidates(prefix: &str, troves: &[Trove]) {
    println!("{prefix} {} orphaned package(s):", troves.len());
    for trove in troves {
        print_autoremove_trove(trove);
    }
}

fn print_autoremove_skips(skipped: &[(Trove, AutoremoveSkipReason)]) {
    if skipped.is_empty() {
        return;
    }

    let adopted = skipped
        .iter()
        .filter(|(_, reason)| *reason == AutoremoveSkipReason::AdoptedNativeAuthority)
        .collect::<Vec<_>>();
    if !adopted.is_empty() {
        println!(
            "Skipping adopted orphaned package(s); native package-manager authority is preserved:"
        );
        for (trove, _) in adopted {
            print_autoremove_trove(trove);
        }
    }

    let protected = skipped
        .iter()
        .filter(|(_, reason)| *reason != AutoremoveSkipReason::AdoptedNativeAuthority)
        .collect::<Vec<_>>();
    if !protected.is_empty() {
        println!("Skipping protected orphaned package(s):");
        for (trove, reason) in protected {
            print!("  {} {}", trove.name, trove.version);
            if let Some(arch) = &trove.architecture {
                print!(" [{}]", arch);
            }
            println!(" ({:?})", reason);
        }
    }
}

fn print_autoremove_trove(trove: &Trove) {
    print!("  {} {}", trove.name, trove.version);
    if let Some(arch) = &trove.architecture {
        print!(" [{}]", arch);
    }
    println!();
}

fn autoremove_identity(trove: &Trove) -> (String, String, Option<String>) {
    (
        trove.name.clone(),
        trove.version.clone(),
        trove.architecture.clone(),
    )
}

#[path = "autoremove/plan_output.rs"]
mod plan_output;

#[cfg(test)]
#[path = "autoremove/tests.rs"]
mod tests;
