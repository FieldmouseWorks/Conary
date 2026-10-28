//! conaryd (`docs/conaryd.md`): the ledger's calls ([`conary_core::rpc`])
//! over a Unix socket, one JSON-RPC request per line, each connection on its
//! own thread. It reads the ledger and never writes it: `cn` and the steward
//! still record the ops, and conaryd reads the log again when it moves. It
//! runs no git, and nothing else a caller could point it at.

use std::fs::{self, File, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use conary_core::reader::Reader;
use conary_core::rpc;
use serde_json::{Value, json};

/// The longest request line conaryd reads. A longer one is refused, and the
/// connection closed: no call needs more.
pub const MAX_LINE: u64 = 1 << 20;

/// What one conaryd takes on, so no client, or many, can use it up.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Connections served at once. One more is refused and closed.
    pub connections: usize,
    /// How long a connection may send nothing before it's closed. A client
    /// that keeps one open, like the MCP server, connects again.
    pub idle: Duration,
    /// How long a response may take to write, to a client that stopped
    /// reading, before its connection is closed.
    pub write: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: 64,
            idle: Duration::from_secs(60),
            write: Duration::from_secs(10),
        }
    }
}

/// Where conaryd listens when nothing says otherwise: in the XDG runtime
/// directory, or `/run/conary` for a system service.
pub fn default_socket() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(|| PathBuf::from("/run"), PathBuf::from)
        .join("conary/conaryd.sock")
}

/// conaryd's hold on its socket: the listener, and the lock beside the
/// socket that keeps any other conaryd off it while this one runs.
#[derive(Debug)]
pub struct Bound {
    pub listener: UnixListener,
    _lock: File,
}

fn in_use(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::AddrInUse, message)
}

/// Listens at `path`, making its directory if there's none. The lock file
/// beside it (`path` with `.lock` on the end) is taken first, so of any
/// conaryds starting at once one listens and the rest are refused. A socket
/// left there by a conaryd that's gone is taken over; one something still
/// answers on, or a file that isn't a socket, is refused.
pub fn bind(path: &Path) -> io::Result<Bound> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(name))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(in_use(format!("another conaryd holds {}", path.display())));
        }
        Err(TryLockError::Error(e)) => return Err(e),
    }
    match fs::symlink_metadata(path) {
        Ok(m) if !m.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is there and isn't a socket", path.display()),
            ));
        }
        Ok(_) => match UnixStream::connect(path) {
            Ok(_) => return Err(in_use(format!("{} is answered on", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path)?,
            Err(e) => return Err(e),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(Bound {
        listener: UnixListener::bind(path)?,
        _lock: lock,
    })
}

/// One of the connections [`Limits::connections`] allows, given back when
/// it's dropped.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(open: &Arc<AtomicUsize>, most: usize) -> Option<Self> {
        open.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
            (n < most).then_some(n + 1)
        })
        .ok()
        .map(|_| Self(Arc::clone(open)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Answers every connection to `listener` from `reader`, for as long as
/// conaryd runs. A connection it can't take on (one over the limit, or one
/// it can't start a thread for) is closed, and the rest go on being served.
pub fn serve(listener: &UnixListener, reader: Reader, limits: Limits) -> ! {
    let reader = Arc::new(Mutex::new(reader));
    let open = Arc::new(AtomicUsize::new(0));
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) => {
                // Out of descriptors, say: what's open is still served, and
                // the next accept waits a moment for some to close.
                eprintln!("conaryd: accepting a connection: {e}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let Some(slot) = Slot::take(&open, limits.connections) else {
            let _ = refuse(&stream, limits, "conaryd is serving all it takes");
            continue;
        };
        let reader = Arc::clone(&reader);
        let started = thread::Builder::new().spawn(move || {
            let _slot = slot;
            // A client that goes away mid-answer is its own business.
            let _ = connection(&stream, &reader, limits);
        });
        if let Err(e) = started {
            // The connection and its slot went with the thread.
            eprintln!("conaryd: starting a connection's thread: {e}");
        }
    }
}

fn refuse(stream: &UnixStream, limits: Limits, message: &str) -> io::Result<()> {
    stream.set_write_timeout(Some(limits.write))?;
    writeln!(&*stream, "{}", rpc::refused(message))
}

/// Answers one connection's requests, a line each, in order.
fn connection(stream: &UnixStream, reader: &Mutex<Reader>, limits: Limits) -> io::Result<()> {
    stream.set_read_timeout(Some(limits.idle))?;
    stream.set_write_timeout(Some(limits.write))?;
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
            return refuse(stream, limits, &format!("a request over {MAX_LINE} bytes"));
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

/// A connection to conaryd, made when it's first needed and made again when
/// it drops: conaryd restarting costs a client nothing.
pub struct Client {
    socket: PathBuf,
    conn: Option<(BufReader<UnixStream>, UnixStream)>,
    next: u64,
}

/// What went wrong with a call: conaryd couldn't be reached, or it answered
/// with an error.
#[derive(Debug)]
pub enum CallError {
    Io(io::Error),
    Rpc(rpc::Error),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "conaryd isn't reachable: {e}"),
            Self::Rpc(e) => write!(f, "conaryd: {} ({})", e.message, e.code),
        }
    }
}

impl std::error::Error for CallError {}

impl Client {
    pub fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_owned(),
            conn: None,
            next: 0,
        }
    }

    /// `method`'s result, from conaryd. Every call only reads, so one that
    /// fails on a connection that dropped is made once more on a new one.
    pub fn call(&mut self, method: &str, params: &Value) -> Result<Value, CallError> {
        let had = self.conn.is_some();
        match self.once(method, params) {
            Err(CallError::Io(_)) if had => {
                self.conn = None;
                self.once(method, params)
            }
            other => other,
        }
    }

    fn once(&mut self, method: &str, params: &Value) -> Result<Value, CallError> {
        let got = self.exchange(method, params);
        if got.is_err() {
            self.conn = None;
        }
        let reply = got.map_err(CallError::Io)?;
        if let Some(e) = reply.get("error") {
            let code = e["code"].as_i64().unwrap_or(rpc::Error::FAILED);
            let message = e["message"].as_str().unwrap_or("an error").to_owned();
            return Err(CallError::Rpc(rpc::Error::new(code, message)));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    fn exchange(&mut self, method: &str, params: &Value) -> io::Result<Value> {
        if self.conn.is_none() {
            let out = UnixStream::connect(&self.socket)?;
            self.conn = Some((BufReader::new(out.try_clone()?), out));
        }
        let (lines, out) = self.conn.as_mut().expect("connected above");
        self.next += 1;
        let id = self.next;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(out, "{request}")?;
        let mut line = String::new();
        if lines.read_line(&mut line)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let reply: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        if reply["id"] != id {
            return Err(io::Error::other(format!(
                "a reply to {}, not {id}",
                reply["id"]
            )));
        }
        Ok(reply)
    }
}
