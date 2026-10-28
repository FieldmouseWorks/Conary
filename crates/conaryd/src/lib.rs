//! conaryd (`docs/conaryd.md`): the ledger's calls ([`conary_core::rpc`])
//! over a Unix socket, one JSON-RPC request per line, each connection on its
//! own thread. It reads the ledger and never writes it: `cn` and the steward
//! still record the ops, and conaryd reads the log again when it moves.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use conary_core::reader::Reader;
use conary_core::rpc;

/// The longest request line conaryd reads. A longer one is refused, and the
/// connection closed: no call needs more.
pub const MAX_LINE: u64 = 1 << 20;

/// Where conaryd listens when nothing says otherwise: in the XDG runtime
/// directory, or `/run/conary` for a system service.
pub fn default_socket() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(|| PathBuf::from("/run"), PathBuf::from)
        .join("conary/conaryd.sock")
}

/// Listens at `path`, making its directory if there's none. A socket left
/// there by a conaryd that's gone is taken over; one that another conaryd
/// still answers on, or a file that isn't a socket, is refused.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    match fs::symlink_metadata(path) {
        Ok(m) if !m.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is there and isn't a socket", path.display()),
            ));
        }
        Ok(_) => match UnixStream::connect(path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("another conaryd answers at {}", path.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path)?,
            Err(e) => return Err(e),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    UnixListener::bind(path)
}

/// Answers every connection to `listener` from `reader`, until accepting
/// one fails.
pub fn serve(listener: &UnixListener, reader: Reader) -> io::Result<()> {
    let reader = Arc::new(Mutex::new(reader));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => continue,
            Err(e) => return Err(e),
        };
        let reader = Arc::clone(&reader);
        thread::spawn(move || {
            // A client that goes away mid-answer is its own business.
            let _ = connection(&stream, &reader);
        });
    }
    Ok(())
}

/// Answers one connection's requests, a line each, in order.
fn connection(stream: &UnixStream, reader: &Mutex<Reader>) -> io::Result<()> {
    let mut lines = BufReader::new(stream);
    let mut out = stream;
    let mut line = Vec::new();
    loop {
        line.clear();
        if (&mut lines)
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            return Ok(());
        }
        if line.len() as u64 > MAX_LINE {
            let refused = rpc::refused(&format!("a request over {MAX_LINE} bytes"));
            return writeln!(out, "{refused}");
        }
        let text = String::from_utf8_lossy(&line);
        if text.trim().is_empty() {
            continue;
        }
        // The lock is held only while the ledger is brought up to date: the
        // call reads its own snapshot, beside every other call.
        let ledger = || {
            let mut reader = reader.lock().unwrap_or_else(PoisonError::into_inner);
            reader.refresh().map_err(|e| e.to_string())
        };
        if let Some(response) = rpc::answer(&text, ledger) {
            writeln!(out, "{response}")?;
        }
    }
}
