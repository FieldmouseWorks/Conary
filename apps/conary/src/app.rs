// apps/conary/src/app.rs
//! Conary application bootstrap and top-level error presentation.

use anyhow::Result;
use clap::Parser;

use crate::cli::Cli;
use crate::dispatch;

pub async fn run() -> Result<()> {
    crate::test_hooks::initialize()?;
    let cli = Cli::parse();
    conary_bootstrap::init_cli_tracing(crate::logging::verbosity_directive(
        cli.quiet,
        cli.log_verbose,
    ));

    if cli.help_advanced {
        print!("{}", crate::cli::render_advanced_help());
        return Ok(());
    }
    dispatch::dispatch(cli).await
}

pub fn report_error(err: &anyhow::Error) {
    crate::ui::diagnostics::report_error(err);
}
