// apps/conary/src/commands/ccs/inspect.rs

//! CCS package inspection and verification
//!
//! Commands for inspecting package contents and verifying signatures.

use anyhow::{Context, Result};
use conary_core::ccs::UntrustedPackageInspection;
use std::path::Path;

mod render;

/// Inspect a CCS package
pub fn cmd_ccs_inspect(
    package: &str,
    show_files: bool,
    show_hooks: bool,
    show_deps: bool,
    format: &str,
) -> Result<()> {
    let path = Path::new(package);

    if !path.exists() {
        anyhow::bail!("Package not found: {}", package);
    }

    // Load and parse the package
    let pkg = UntrustedPackageInspection::inspect_untrusted_file(path)
        .context("Failed to inspect untrusted CCS package")?;

    // Output in requested format
    if format == "json" {
        render::print_json(&pkg, show_files, show_hooks, show_deps)?;
    } else {
        // Human-readable output
        render::print_summary(&pkg);

        if show_files {
            println!();
            render::print_files(&pkg);
        }

        if show_hooks {
            println!();
            render::print_hooks(&pkg);
        }

        if show_deps {
            println!();
            render::print_dependencies(&pkg);
        }
    }

    Ok(())
}
