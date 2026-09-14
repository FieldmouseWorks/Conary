// apps/remi/src/bin/remi/native_oracle_retention_command.rs

//! Inspect and release exact export-owned diagnostic catalog retention.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use remi::server::{inspect_native_oracle_input_retention, release_native_oracle_input_retention};

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Authenticate the complete retained export and registered catalog set.
    Inspect(InspectArgs),
    /// Release only the named export's complete durable revision pin set.
    Release(ReleaseArgs),
}

#[derive(Args)]
pub(crate) struct InspectArgs {
    #[arg(long, default_value = "/conary/metadata/conary.db")]
    db: PathBuf,
    #[arg(long, default_value = "/conary/catalogs")]
    catalog_dir: PathBuf,
    #[arg(long)]
    input_dir: PathBuf,
    #[arg(long)]
    export_id: String,
}

#[derive(Args)]
pub(crate) struct ReleaseArgs {
    #[arg(long, default_value = "/conary/metadata/conary.db")]
    db: PathBuf,
    #[arg(long)]
    export_id: String,
    /// Canonical native input manifest digest from the authenticated export.
    #[arg(long)]
    input_manifest_sha256: String,
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::Inspect(args) => {
            let result = inspect_native_oracle_input_retention(
                &args.db,
                &args.catalog_dir,
                &args.input_dir,
                &args.export_id,
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Release(args) => {
            let result = release_native_oracle_input_retention(
                &args.db,
                &args.export_id,
                &args.input_manifest_sha256,
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }
    Ok(())
}
