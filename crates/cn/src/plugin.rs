use conary_core::plugin::{Limits, Plugin};
use std::path::PathBuf;
use std::time::Duration;

#[derive(clap::Subcommand)]
pub enum PluginCommand {
    /// Send the contract's fixtures to every advertised kind.
    Check {
        path: PathBuf,
        /// Deadline per invocation in milliseconds, including describe.
        #[arg(long, default_value_t = 30_000)]
        deadline_ms: u64,
        /// Maximum response bytes per invocation.
        #[arg(long, default_value_t = 1_048_576)]
        response_bytes: usize,
    },
}
pub fn run(command: &PluginCommand) -> super::Result<()> {
    let PluginCommand::Check {
        path,
        deadline_ms,
        response_bytes,
    } = command;
    let plugin = Plugin::describe(
        path,
        Limits {
            deadline: Duration::from_millis(*deadline_ms),
            response_bytes: *response_bytes,
        },
    )?;
    let checked = plugin.check()?;
    println!(
        "{}",
        serde_json::json!({"by": plugin.identity, "checked": checked})
    );
    Ok(())
}
