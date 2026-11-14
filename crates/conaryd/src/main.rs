//! `conaryd`, Conary's daemon: a thin layer over [`conaryd`]'s library.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use conary_core::log;
use conary_core::reader::Reader;
use conaryd::Limits;

#[derive(Parser)]
#[command(
    name = "conaryd",
    about = "conaryd: the ledger's calls, served over a Unix socket"
)]
struct Cli {
    /// The ledger's directory.
    #[arg(long, env = "CONARY_LEDGER", default_value_os_t = log::default_dir())]
    ledger: PathBuf,
    /// Enable verified flow ingress using this machine-owned JSON configuration.
    #[arg(long, env = "CONARY_EVENTS_CONFIG")]
    events_config: Option<PathBuf>,
    /// The socket to listen on.
    #[arg(long, env = "CONARY_SOCKET", default_value_os_t = conaryd::default_socket())]
    socket: PathBuf,
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let key = log::load_public_key(&log::public_path(&log::key_path(&cli.ledger)))
        .map_err(|e| format!("{e} (has `cn init` set up {}?)", cli.ledger.display()))?;
    let reader = Reader::open(&cli.ledger, key)?;
    let bound = conaryd::bind(&cli.socket)
        .map_err(|e| format!("listening at {}: {e}", cli.socket.display()))?;
    if let Some(path) = &cli.events_config {
        let config: conary_github::events::Runtime = serde_json::from_slice(&std::fs::read(path)?)?;
        let queue = std::sync::Arc::new(
            conary_github::events::Queue::open(&config.data).map_err(|e| e.to_string())?,
        );
        let receiver = conary_github::events::Receiver::open(
            std::fs::read(&config.secret_path)?,
            config.repos.iter().map(ToString::to_string),
            &config.data,
        )
        .map_err(|e| e.to_string())?;
        let listener = std::net::TcpListener::bind(config.listen)?;
        let incoming = std::sync::Arc::clone(&queue);
        std::thread::Builder::new().spawn(move || {
            if conaryd::webhook::serve(listener, receiver, incoming).is_err() {
                eprintln!("conaryd: webhook listener stopped");
                std::process::exit(1);
            }
        })?;
        let ledger = cli.ledger.clone();
        std::thread::Builder::new().spawn(move || {
            // A failed pass books its repo's recovery; only a mailbox that
            // can't be written stops the worker.
            if config.run(&ledger, &queue).is_err() {
                eprintln!("conaryd: flow worker stopped: its mailbox can't be written");
                std::process::exit(1);
            }
        })?;
    }
    eprintln!(
        "conaryd: the ledger in {}, at {}",
        cli.ledger.display(),
        cli.socket.display()
    );
    conaryd::serve(&bound.listener, reader, Limits::default())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("conaryd: {e}");
            ExitCode::FAILURE
        }
    }
}
