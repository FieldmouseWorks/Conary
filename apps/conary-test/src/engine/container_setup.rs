// apps/conary-test/src/engine/container_setup.rs
//! Shared container initialization logic for test runners and service code.
//!
//! Extracted from the runner and service so both paths use identical
//! database-init + repo-setup sequences.

use anyhow::{Context, bail};

use crate::config::distro::GlobalConfig;
use crate::container::{ContainerBackend, ContainerId};

/// Initialize conary database and repos inside a test container.
///
/// `configure_remi_override` gates test-only replacement of the packaged Remi
/// seed when a non-production endpoint is configured. `system init` creates
/// every built-in source feed; the container harness keeps only its selected
/// feed so an unqualified test sync remains deterministic.
pub async fn initialize_container_state(
    config: &GlobalConfig,
    distro: &str,
    configure_remi_override: bool,
    backend: &dyn ContainerBackend,
    container_id: &ContainerId,
) -> anyhow::Result<()> {
    use std::time::Duration;

    let distro_config = config
        .distros
        .get(distro)
        .with_context(|| format!("unknown distro: {distro}"))?;
    let selected_seed = format!("remi-{}", distro_config.remi_distro);
    if distro_config.repo_name != selected_seed {
        bail!(
            "distro {distro} must use packaged repository seed '{selected_seed}', not '{}'",
            distro_config.repo_name,
        );
    }
    let db_parent = std::path::Path::new(&config.paths.db)
        .parent()
        .context("db path has no parent directory")?
        .display()
        .to_string();
    let init_cmd = format!(
        "mkdir -p {db_parent} && {} system init --db-path {}",
        config.paths.conary_bin, config.paths.db
    );
    let init_result = backend
        .exec(
            container_id,
            &["sh", "-c", &init_cmd],
            Duration::from_secs(120),
        )
        .await?;
    if init_result.exit_code != 0 {
        bail!(
            "failed to initialize conary database: {}{}",
            init_result.stdout,
            init_result.stderr
        );
    }

    for repo in &config.setup.remove_default_repos {
        let remove_cmd = format!(
            "{} repo remove {} --db-path {} >/dev/null 2>&1 || true",
            config.paths.conary_bin, repo, config.paths.db
        );
        backend
            .exec(
                container_id,
                &["sh", "-c", &remove_cmd],
                Duration::from_secs(30),
            )
            .await?;
    }

    for feed in conary_core::repository::supported_profiles::public_profiles() {
        let repo = format!("remi-{}", feed.id());
        if repo == selected_seed {
            continue;
        }
        let remove_cmd = format!(
            "{} repo remove {} --db-path {} >/dev/null 2>&1 || true",
            config.paths.conary_bin, repo, config.paths.db
        );
        backend
            .exec(
                container_id,
                &["sh", "-c", &remove_cmd],
                Duration::from_secs(30),
            )
            .await?;
    }

    if configure_remi_override
        && config.remi.endpoint.trim_end_matches('/') != "https://remi.conary.io"
    {
        let replace_repo_cmd = format!(
            "{} repo remove {} --db-path {} && {} repo add {} {} --package-format json --default-strategy remi --remi-endpoint {} --source-profile {} --db-path {}",
            config.paths.conary_bin,
            distro_config.repo_name,
            config.paths.db,
            config.paths.conary_bin,
            distro_config.repo_name,
            config.remi.endpoint,
            config.remi.endpoint,
            distro_config.remi_distro,
            config.paths.db
        );
        let replace_result = backend
            .exec(
                container_id,
                &["sh", "-c", &replace_repo_cmd],
                Duration::from_secs(60),
            )
            .await?;
        if replace_result.exit_code != 0 {
            bail!(
                "failed to replace packaged Remi seed for test endpoint: {}{}",
                replace_result.stdout,
                replace_result.stderr
            );
        }
    }

    verify_selected_seed(
        backend,
        container_id,
        &config.paths.db,
        &distro_config.repo_name,
    )
    .await
}

fn selected_seed_query(name: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM repositories WHERE name = '{}' AND enabled = 1;",
        name.replace('\'', "''")
    )
}

async fn verify_selected_seed(
    backend: &dyn ContainerBackend,
    container_id: &ContainerId,
    db_path: &str,
    name: &str,
) -> anyhow::Result<()> {
    // Onboarding evidence comes from persisted state, never human status tags.
    let query = selected_seed_query(name);
    let result = backend
        .exec(
            container_id,
            &["sqlite3", "-readonly", db_path, &query],
            std::time::Duration::from_secs(30),
        )
        .await?;
    if result.exit_code != 0 {
        bail!(
            "failed to inspect packaged repository seed: {}{}",
            result.stdout,
            result.stderr
        );
    }
    let count: u64 = result
        .stdout
        .trim()
        .parse()
        .context("invalid packaged repository seed count")?;
    if count != 1 {
        bail!(
            "packaged onboarding must leave one enabled selected Remi source '{name}'; found {count}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{ExecResult, mock::MockBackend};

    #[test]
    fn selected_seed_query_requires_the_exact_enabled_source() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE repositories (name TEXT, enabled INTEGER);")
            .unwrap();
        let name = "remi-quoted' OR 1=1 --";
        conn.execute(
            "INSERT INTO repositories VALUES (?1, 1), ('other', 1)",
            [name],
        )
        .unwrap();
        let count = |query: String| {
            conn.query_row(&query, [], |row| row.get::<_, i64>(0))
                .unwrap()
        };
        assert_eq!(count(selected_seed_query(name)), 1);
        assert_eq!(count(selected_seed_query("missing")), 0);
        conn.execute(
            "UPDATE repositories SET enabled = 0 WHERE name = ?1",
            [name],
        )
        .unwrap();
        assert_eq!(count(selected_seed_query(name)), 0);
    }

    #[tokio::test]
    async fn selected_seed_inspection_is_read_only_and_rejects_missing_or_invalid_evidence() {
        for (exit_code, stdout, valid) in [
            (0, "1\n", true),
            (0, "0\n", false),
            (0, "2\n", false),
            (0, "Repositories: [info] remi-fixture", false),
            (1, "1\n", false),
        ] {
            let backend = MockBackend::new(vec![ExecResult {
                exit_code,
                stdout: stdout.into(),
                stderr: String::new(),
            }]);
            let result = verify_selected_seed(
                &backend,
                &"fixture".into(),
                "/db with ' quotes.db",
                "remi-fixture",
            )
            .await;
            assert_eq!(result.is_ok(), valid, "{result:?}");
            let calls = backend.exec_calls();
            assert_eq!(
                calls[0],
                vec![
                    "sqlite3",
                    "-readonly",
                    "/db with ' quotes.db",
                    &selected_seed_query("remi-fixture")
                ]
            );
        }
    }
}
