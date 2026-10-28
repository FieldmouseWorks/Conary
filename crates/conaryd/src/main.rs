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
