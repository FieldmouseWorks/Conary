use conary_core::op::Repo;
use conary_core::{Policy, log};
use conary_github::events::Runtime;
use conary_github::flow::{Config, Pass, Pause, Scope, jobs, pause, schema};
use conary_github::{Api, Tokens, anchor};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum FlowCommand {
    /// Read, decide and act once on the repository's open pull requests.
    Pass(Args),
    /// Request the same pass now, optionally for one pull request.
    Kick(Args),
    /// A dry pass beside what the steward running the repository says of
    /// each PR now: one JSON line per PR, disagreements first, then a tally.
    Compare(Args),
    /// Every option of the flow's configuration and conaryd's events and
    /// gate-reuse configurations: their schema, or what a machine's files
    /// set.
    Settings(SettingsArgs),
    /// Pause the repository: the next decision stops every worker its PRs
    /// have running and starts nothing, until it's resumed. Needs no
    /// conaryd, GitHub, build or deploy, and is always allowed.
    Pause(PauseArgs),
    /// Lift the repository's pause: the next pass carries on from each PR's
    /// saved state.
    Resume(ResumeArgs),
}
#[derive(clap::Args)]
pub struct PauseArgs {
    repo: Repo,
    /// Why it's paused, shown in each PR's decision while it is.
    #[arg(long)]
    reason: String,
    #[arg(long, env = "CONARY_DATA", default_value_os_t = log::default_dir().join("data"))]
    data: PathBuf,
}
#[derive(clap::Args)]
pub struct ResumeArgs {
    repo: Repo,
    #[arg(long, env = "CONARY_DATA", default_value_os_t = log::default_dir().join("data"))]
    data: PathBuf,
}
#[derive(clap::Args)]
pub struct SettingsArgs {
    /// Print the schema of all three configurations: each option's type, allowed
    /// values, default, description, whether it starts paid work and
    /// whether it's live. No configuration file is read.
    #[arg(long)]
    schema: bool,
    /// The flow configuration whose options to print, each with its
    /// effective value and whether the file or the default set it.
    #[arg(long, env = "CONARY_FLOW_CONFIG")]
    config: Option<PathBuf>,
    /// conaryd's events configuration whose options to print, the ones that
    /// take effect only at a restart listed apart.
    #[arg(long, env = "CONARY_EVENTS_CONFIG")]
    events_config: Option<PathBuf>,
    /// conaryd's gate-reuse configuration whose options to print, the ones
    /// that take effect only at a restart listed apart.
    #[arg(long, env = "CONARY_GATE_REUSE_CONFIG")]
    gate_reuse_config: Option<PathBuf>,
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
    let args = match command {
        FlowCommand::Settings(args) => return settings(args),
        FlowCommand::Pause(args) => {
            let paused = Pause {
                scope: Scope::Repository,
                reason: args.reason.clone(),
                at: jobs::now(),
            };
            pause::pause(&args.data, &args.repo, &paused).map_err(own)?;
            println!("{}", serde_json::to_string(&paused)?);
            return Ok(());
        }
        FlowCommand::Resume(args) => return pause::resume(&args.data, &args.repo).map_err(own),
        FlowCommand::Pass(args) | FlowCommand::Kick(args) | FlowCommand::Compare(args) => args,
    };
    let compare = matches!(command, FlowCommand::Compare(_));
    let dry_run = args.dry_run || compare;
    let config = Config::load(&args.config).map_err(own)?;
    let policy = Policy::load(&args.policy)?;
    let tokens: Tokens = super::choose_tokens(
        args.token_command.as_deref(),
        std::env::var("GH_TOKEN").ok().as_deref(),
        std::env::var("GITHUB_TOKEN").ok().as_deref(),
    )?;
    let api = Api::new(&args.api, tokens);
    let needs_ledger =
        config.settings.train || config.evidence.is_some() || args.anchor_repo.is_some();
    let mut anchored = if needs_ledger && !dry_run {
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
    let observed = if anchored.is_none() {
        Some(super::read(cli, None)?)
    } else {
        None
    };
    // One pass, then this CLI exits: whoever runs it runs the next one.
    let process = jobs::Process {
        worker: std::env::current_exe()?,
        ended: None,
    };
    let pass = Pass {
        config: &config,
        policy: &policy,
        root: &args.data,
        launcher: &process,
        now: jobs::now(),
        dry_run,
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
    let reports = reports?;
    if compare {
        let (lines, tally) =
            conary_github::flow::beside(&api, reports, &config.settings.review_authors);
        for line in lines {
            println!("{}", serde_json::to_string(&line)?);
        }
        println!("{}", serde_json::to_string(&tally)?);
        return Ok(());
    }
    for report in reports {
        println!("{}", serde_json::to_string(&report)?);
    }
    Ok(())
}

/// `cn flow settings`: one JSON document, keyed by configuration.
fn settings(args: &SettingsArgs) -> super::Result<()> {
    let mut out = serde_json::Map::new();
    if args.schema {
        out.insert("flow".into(), schema::flow().map_err(own)?);
        out.insert("events".into(), schema::events().map_err(own)?);
        out.insert("gate_reuse".into(), schema::reuse().map_err(own)?);
    } else {
        if args.config.is_none() && args.events_config.is_none() && args.gate_reuse_config.is_none()
        {
            return Err("give --schema, or a configuration with --config, --events-config or --gate-reuse-config".into());
        }
        if let Some(path) = &args.config {
            let listing = schema::effective::<Config>(&std::fs::read(path)?).map_err(own)?;
            out.insert("flow".into(), serde_json::to_value(listing)?);
        }
        if let Some(path) = &args.events_config {
            let listing = schema::effective::<Runtime>(&std::fs::read(path)?).map_err(own)?;
            out.insert("events".into(), serde_json::to_value(listing)?);
        }
        if let Some(path) = &args.gate_reuse_config {
            let listing =
                schema::effective::<conary_github::reuse::Settings>(&std::fs::read(path)?)
                    .map_err(own)?;
            out.insert("gate_reuse".into(), serde_json::to_value(listing)?);
        }
    }
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
/// The library's errors, as this command's.
fn own(e: Box<dyn std::error::Error + Send + Sync>) -> Box<dyn std::error::Error> {
    e
}
