//! `conary-mcp`, Conary's MCP server: the ledger's calls as tools, over
//! stdio, each forwarded to conaryd. A thin layer over
//! [`conary_core::mcp`] and [`conaryd::Client`].

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use conary_core::mcp;

#[derive(Parser)]
#[command(
    name = "conary-mcp",
    about = "conary-mcp: the ledger's calls as MCP tools, over stdio, answered by conaryd"
)]
struct Cli {
    /// conaryd's socket.
    #[arg(long, env = "CONARY_SOCKET", default_value_os_t = conaryd::default_socket())]
    socket: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut conaryd = conaryd::Client::new(&cli.socket);
    let mut out = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else {
            return ExitCode::FAILURE;
        };
        if line.trim().is_empty() {
            continue;
        }
        let forward =
            |method: &str, params: &_| conaryd.call(method, params).map_err(|e| e.to_string());
        if let Some(response) = mcp::answer(&line, forward)
            && writeln!(out, "{response}")
                .and_then(|()| out.flush())
                .is_err()
        {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
