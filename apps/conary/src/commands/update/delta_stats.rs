// apps/conary/src/commands/update/delta_stats.rs

//! Delta update statistics command handler.

use super::super::open_db;
use anyhow::Result;
use conary_core::db::models::DeltaStats;
use tracing::info;

/// Show delta update statistics
pub fn cmd_delta_stats(db_path: &str) -> Result<()> {
    info!("Showing delta update statistics");

    let conn = open_db(db_path)?;
    let total_stats = DeltaStats::get_total_stats(&conn)?;

    let all_stats = {
        let mut stmt = conn.prepare(
            "SELECT id, changeset_id, total_bytes_saved, deltas_applied, full_downloads, delta_failures, created_at
             FROM delta_stats ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DeltaStats {
                id: Some(row.get(0)?),
                changeset_id: row.get(1)?,
                total_bytes_saved: row.get(2)?,
                deltas_applied: row.get(3)?,
                full_downloads: row.get(4)?,
                delta_failures: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    if all_stats.is_empty() {
        crate::ui::println!("No delta statistics available");
        crate::ui::println!("Run 'conary update' to start tracking delta usage");
        return Ok(());
    }

    crate::ui::println!("=== Delta Update Statistics ===\n");
    crate::ui::println!("Total Statistics:");
    crate::ui::println!("  Delta updates applied: {}", total_stats.deltas_applied);
    crate::ui::println!("  Full artifacts prepared: {}", total_stats.full_downloads);
    crate::ui::println!("  Delta failures: {}", total_stats.delta_failures);

    let total_mb = total_stats.total_bytes_saved as f64 / 1_048_576.0;
    crate::ui::println!("  Total bandwidth saved: {:.2} MB", total_mb);

    if let Some(success_rate) = delta_success_rate(&total_stats) {
        crate::ui::println!("  Delta success rate: {:.1}%", success_rate);
    }

    crate::ui::println!("\nRecent Operations:");
    for (idx, stats) in all_stats.iter().take(10).enumerate() {
        if idx > 0 {
            crate::ui::println!();
        }

        let timestamp = stats.created_at.as_deref().unwrap_or("unknown");
        crate::ui::println!("  [Changeset {}] {}", stats.changeset_id, timestamp);
        crate::ui::println!("    Deltas applied: {}", stats.deltas_applied);
        crate::ui::println!("    Full artifacts prepared: {}", stats.full_downloads);

        if stats.delta_failures > 0 {
            crate::ui::println!("    Delta failures: {}", stats.delta_failures);
        }

        if stats.total_bytes_saved > 0 {
            let saved_mb = stats.total_bytes_saved as f64 / 1_048_576.0;
            crate::ui::println!("    Bandwidth saved: {:.2} MB", saved_mb);
        }
    }

    if all_stats.len() > 10 {
        crate::ui::println!("\n... and {} more operations", all_stats.len() - 10);
    }

    Ok(())
}

fn delta_success_rate(stats: &DeltaStats) -> Option<f64> {
    let attempts = i64::from(stats.deltas_applied) + i64::from(stats.delta_failures);
    (attempts > 0).then(|| f64::from(stats.deltas_applied) / attempts as f64 * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_artifact_preparation_does_not_change_delta_attempt_success() {
        let mut stats = DeltaStats::new(1);
        stats.deltas_applied = 1;
        stats.delta_failures = 1;
        for prepared in [0, 2, 20] {
            stats.full_downloads = prepared;
            assert_eq!(delta_success_rate(&stats), Some(50.0));
        }
        stats.deltas_applied = 0;
        stats.delta_failures = 0;
        assert_eq!(delta_success_rate(&stats), None);
    }
}
