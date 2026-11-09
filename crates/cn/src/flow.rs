use conary_core::op::Repo;
use conary_core::{Policy, log};
use conary_github::flow::{Config, Pass, jobs};
use conary_github::{Api, Tokens, anchor};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum FlowCommand {
    /// Read, decide and act once on the repository's open pull requests.
    Pass(Args),
    /// Request the same pass now, optionally for one pull request.
    Kick(Args),
}
#[derive(clap::Args)]
pub struct Args {
    repo: Repo,
    pr: Option<u64>,
    /// Read and decide only. JSON lines, one complete decision per PR.
    #[arg(long)]
    dry_run: bool,
    /// Mutable state, separate from the append-only ledger.
    #[arg(long, env = "CONARY_DATA", default_value_os_t = log::default_dir().join("data"))]
    data: PathBuf,
    #[arg(long, env = "CONARY_FLOW_CONFIG")]
    config: PathBuf,
    #[arg(long, env = "CONARY_POLICY")]
    policy: PathBuf,
    #[arg(long, env = "GITHUB_API_URL", default_value = "https://api.github.com")]
    api: String,
    #[arg(long, env = "CONARY_TOKEN_COMMAND", hide_env_values = true)]
    token_command: Option<String>,
    /// Required for train operations or recording negative evidence.
    #[arg(long, env = "CONARY_ANCHOR_REPO")]
    anchor_repo: Option<String>,
}
pub fn run(cli: &super::Cli, command: &FlowCommand) -> super::Result<()> {
    let (FlowCommand::Pass(args) | FlowCommand::Kick(args)) = command;
    let config: Config = serde_json::from_slice(&std::fs::read(&args.config)?)?;
    let policy = Policy::load(&args.policy)?;
    let tokens: Tokens = super::choose_tokens(
        args.token_command.as_deref(),
        std::env::var("GH_TOKEN").ok().as_deref(),
        std::env::var("GITHUB_TOKEN").ok().as_deref(),
    )?;
    let api = Api::new(&args.api, tokens);
    let needs_ledger = config.settings.train || config.negative.is_some();
    let mut anchored = if needs_ledger && !args.dry_run {
        let anchor_repo = args
            .anchor_repo
            .as_deref()
            .ok_or("flow evidence and train operations require --anchor-repo")?;
        Some(anchor::open(
            &cli.ledger,
            log::load_signing_key(&log::key_path(&cli.ledger))?,
            &api,
            anchor_repo,
        )?)
    } else {
        None
    };
    let observed = if needs_ledger && args.dry_run {
        Some(super::read(cli, None)?)
    } else {
        None
    };
    let process = jobs::Process(std::env::current_exe()?);
    let pass = Pass {
        config: &config,
        policy: &policy,
        root: &args.data,
        launcher: &process,
        now: jobs::now(),
        dry_run: args.dry_run,
        ledger: observed.as_ref(),
    };
    let reports = pass
        .run(
            &api,
            &args.repo,
            args.pr,
            anchored.as_mut().map(|a| &mut a.store),
        )
        .map_err(|e| -> Box<dyn std::error::Error> { e });
    // Any ledger ops recorded before a per-PR failure still get anchored.
    if let Some(anchored) = &mut anchored {
        anchored.publish(&api)?;
    }
    for report in reports? {
        println!("{}", serde_json::to_string(&report)?);
    }
    Ok(())
}
