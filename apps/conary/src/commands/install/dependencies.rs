// apps/conary/src/commands/install/dependencies.rs

//! Dependency resolution for package installation
//!
//! Runtime requirements are parsed from package metadata, solved against
//! Conary's persisted provider graph, and completed from configured
//! repositories.

use super::dep_resolution;
use super::{BatchInstaller, InstallPhase, InstallProgress};
use anyhow::{Context, Result};
use conary_core::packages::PackageFormat;
use conary_core::repository::dependency_model::RepositoryRequirementKind;
use conary_core::resolver::{SatResolution, SatSource};
use conary_core::scriptlet::SandboxMode;
use tracing::info;

/// Context for the dependency analysis phase.
pub(super) struct DepAnalysisContext<'a> {
    pub(super) conn: &'a rusqlite::Connection,
    pub(super) pkg: &'a dyn PackageFormat,
    pub(super) no_deps: bool,
    pub(super) dry_run: bool,
    pub(super) yes: bool,
    pub(super) allow_downgrade: bool,
    pub(super) db_path: &'a str,
    pub(super) root: &'a str,
    pub(super) sandbox_mode: SandboxMode,
    pub(super) policy: &'a conary_core::repository::resolution_policy::ResolutionPolicy,
}

#[derive(PartialEq, Eq)]
pub(super) enum DependencyDecision {
    Continue,
    Cancelled,
}

/// Handle dependency analysis: resolve, prompt, and install repository deps.
pub(super) async fn handle_dependencies(
    ctx: &DepAnalysisContext<'_>,
    report: &mut super::report::InstallReport,
) -> Result<DependencyDecision> {
    let runtime_requirement_count = ctx
        .pkg
        .requirements()
        .iter()
        .filter(|requirement| {
            matches!(
                requirement.kind,
                RepositoryRequirementKind::Depends | RepositoryRequirementKind::PreDepends
            )
        })
        .count();

    if ctx.no_deps && runtime_requirement_count != 0 {
        info!("Skipping dependency check (--no-deps specified)");
        crate::ui::println!(
            "Skipping {} dependencies (--no-deps specified)",
            runtime_requirement_count
        );
        return Ok(DependencyDecision::Continue);
    }

    if runtime_requirement_count == 0 {
        return Ok(DependencyDecision::Continue);
    }

    let progress = InstallProgress::single("Installing");
    progress.set_phase(ctx.pkg.name(), InstallPhase::ResolvingDeps);
    info!(
        "Resolving {} dependencies with SAT solver...",
        runtime_requirement_count
    );
    crate::ui::println!("Checking dependencies for {}...", ctx.pkg.name());

    let sat_result = conary_core::resolver::solve_package_requirements_with_policy(
        ctx.conn, ctx.pkg, ctx.policy,
    )
    .with_context(|| format!("Failed to resolve dependencies for '{}'", ctx.pkg.name()))?;

    // If SAT reports a conflict, surface it
    if let Some(ref conflict_msg) = sat_result.conflict_message {
        crate::ui::eprintln!("\nDependency conflicts detected:");
        crate::ui::eprintln!("  {}", conflict_msg);
        return Err(anyhow::anyhow!(
            "Cannot install {}: dependency conflict(s) detected",
            ctx.pkg.name(),
        ));
    }

    let selected = resolved_repository_deps_from_sat_result(&sat_result, ctx.pkg.name());

    if selected.is_empty() {
        crate::ui::println!("All dependencies already satisfied");
        return Ok(DependencyDecision::Continue);
    }

    info!("Found {} missing dependencies", selected.len());
    let dep_plan = dep_resolution::DepResolutionPlan {
        to_install: selected,
        unresolvable: Vec::new(),
    };

    // Confirmation prompt for non-trivial dependency installs
    let total_changes = dep_plan.to_install.len();
    if total_changes > 0 && !ctx.dry_run && !ctx.yes {
        let input = progress.suspend(|| -> Result<String> {
            // The progress coordinator already owns the terminal here. Nested
            // ui writes would try to acquire its lock again.
            use std::io::Write;
            let mut output = std::io::stdout().lock();
            writeln!(output)?;
            write!(
                output,
                "Proceed with {} dependency changes? [Y/n] ",
                total_changes
            )?;
            output.flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            Ok(input)
        })?;
        let input = input.trim().to_lowercase();
        if input == "n" || input == "no" {
            crate::ui::println!("Cancelled.");
            return Ok(DependencyDecision::Cancelled);
        }
    }

    handle_dep_installs(ctx, &dep_plan, &progress, report).await?;

    // Check for unresolvable dependencies
    check_unresolvable_deps(ctx, &dep_plan)?;

    Ok(DependencyDecision::Continue)
}

pub(super) fn resolved_repository_deps_from_sat_result(
    sat_result: &SatResolution,
    required_by: &str,
) -> Vec<dep_resolution::ResolvedDep> {
    sat_result
        .install_order
        .iter()
        .filter(|p| p.source == SatSource::Repository)
        .cloned()
        .map(|package| dep_resolution::ResolvedDep {
            package,
            required_by: vec![required_by.to_string()],
        })
        .collect()
}

/// Handle packages that need to be installed from repos.
async fn handle_dep_installs(
    ctx: &DepAnalysisContext<'_>,
    dep_plan: &dep_resolution::DepResolutionPlan,
    progress: &InstallProgress,
    report: &mut super::report::InstallReport,
) -> Result<()> {
    if dep_plan.to_install.is_empty() {
        return Ok(());
    }

    let selections = dep_plan
        .to_install
        .iter()
        .cloned()
        .map(
            |selected| super::repository_batch::RepositoryBatchSelection {
                selected,
                install_reason: conary_core::db::models::InstallReason::Dependency,
                selection_reason: format!("Required by {}", ctx.pkg.name()),
                allow_downgrade: ctx.allow_downgrade,
                intent: super::InstallIntent::PackageChange,
            },
        )
        .collect();
    if !ctx.dry_run {
        crate::ui::println!("  Installing {} dependencies:", dep_plan.to_install.len());
        for dependency in &dep_plan.to_install {
            crate::ui::println!("    {}", dependency.package.name);
        }
        progress.set_phase(ctx.pkg.name(), InstallPhase::InstallingDeps);
    }
    let mode = if ctx.dry_run {
        super::repository_batch::RepositoryBatchMode::for_preview(report.projection.as_deref())
    } else {
        super::repository_batch::RepositoryBatchMode::Install
    };
    let prepared =
        super::repository_batch::prepare_repository_batch(ctx.db_path, selections, mode).await?;
    let installer = BatchInstaller::new(ctx.db_path, ctx.sandbox_mode);
    if ctx.dry_run {
        report.planned.extend(prepared.preview(
            installer,
            report.projection.as_deref(),
            std::path::Path::new(ctx.root),
        )?);
    } else {
        report.extend(prepared.install_with_result(installer)?.report);
        crate::ui::row(
            crate::ui::Status::Ok,
            &[&format!(
                "Installed {} dependencies",
                dep_plan.to_install.len()
            )],
        );
    }

    Ok(())
}

/// Check for unresolvable dependencies and report them.
fn check_unresolvable_deps(
    ctx: &DepAnalysisContext<'_>,
    dep_plan: &dep_resolution::DepResolutionPlan,
) -> Result<()> {
    if dep_plan.unresolvable.is_empty() {
        return Ok(());
    }

    crate::ui::eprintln!("\nUnresolvable dependencies:");
    for dep in &dep_plan.unresolvable {
        crate::ui::eprintln!(
            "  {} {} (required by: {})",
            dep.name,
            dep.constraint,
            dep.required_by.join(", ")
        );
    }
    if let Ok(repos) = conary_core::db::models::Repository::list_all(ctx.conn) {
        let repo_names: Vec<&str> = repos.iter().map(|repo| repo.name.as_str()).collect();
        if repo_names.is_empty() {
            crate::ui::eprintln!("No repositories configured (run 'conary repo add' first)");
        } else {
            crate::ui::eprintln!("Repositories searched: {}", repo_names.join(", "));
        }
    }

    Err(anyhow::anyhow!(
        "Cannot install {}: {} requirements have no installed or repository provider",
        ctx.pkg.name(),
        dep_plan.unresolvable.len()
    ))
}
