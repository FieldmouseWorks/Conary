//! Bounded HTTP/1.1 for conaryd's TCP listeners (the webhook receiver and
//! the gate-reuse lookup): one POST per connection, read within one time
//! budget, its head and body capped, and the connection closed after one
//! answer. Each connection is served on its own thread, up to a limit.

use std::collections::BTreeMap;
use std::io::{self, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

/// The most a request's head (its request line and headers) may take.
pub const MAX_HEAD: usize = 16384;
/// How long a request has to arrive, its body included.
const BUDGET: Duration = Duration::from_secs(5);
/// How long reading or writing may stall before the connection is closed.
const STALL: Duration = Duration::from_secs(3);

/// A request's headers, their names lowercased. A name sent twice is
/// refused.
pub type Headers = BTreeMap<String, String>;

/// A POST, read whole.
#[derive(Debug)]
pub struct Request {
    pub headers: Headers,
    pub body: Vec<u8>,
}

/// Serves every connection to `listener` with `handle`, each on its own
/// thread, at most `most` at once: one more is closed unanswered. Returns
/// only when accepting fails.
pub fn serve<F>(listener: TcpListener, most: usize, handle: F) -> io::Result<()>
where
    F: Fn(TcpStream) + Send + Sync + 'static,
{
    let handle = Arc::new(handle);
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        let Some(slot) = super::Slot::take(&open, most) else {
            continue;
        };
        let handle = Arc::clone(&handle);
        std::thread::Builder::new().spawn(move || {
            let _slot = slot;
            let _ = stream.set_read_timeout(Some(STALL));
            let _ = stream.set_write_timeout(Some(STALL));
            handle(stream);
        })?;
    }
    Ok(())
}

/// Reads one POST from `stream` within the time budget: a head of at most
/// [`MAX_HEAD`] bytes, then, once `admit` takes its headers, a body of
/// exactly its `Content-Length`, at most `max` bytes. `Err` is the status
/// to answer instead: 401 for another method than POST (as the webhook
/// always has), 431 for a head too long, 413 for a body too long, 408 for a
/// request too slow, 400 for one that doesn't read, or `admit`'s.
pub fn read(
    stream: &mut TcpStream,
    max: usize,
    admit: impl FnOnce(&Headers) -> Result<(), u16>,
) -> io::Result<Result<Request, u16>> {
    let mut reader = BufReader::new(stream);
    let deadline = Instant::now() + BUDGET;
    let left = || deadline.saturating_duration_since(Instant::now());
    let mut head = Vec::new();
    // Byte by byte, against a total cap: never a growing line on untrusted
    // input.
    loop {
        if head.len() >= MAX_HEAD {
            return Ok(Err(431));
        }
        if left().is_zero() {
            return Ok(Err(408));
        }
        reader
            .get_mut()
            .set_read_timeout(Some(left().max(Duration::from_millis(1))))?;
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let Ok(head) = std::str::from_utf8(&head) else {
        return Ok(Err(400));
    };
    let mut lines = head.lines();
    if !lines
        .next()
        .is_some_and(|l| l.starts_with("POST ") && l.ends_with(" HTTP/1.1"))
    {
        return Ok(Err(401));
    }
    let mut headers = Headers::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Ok(Err(400));
        };
        if headers
            .insert(name.to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Ok(Err(400));
        }
    }
    if let Err(status) = admit(&headers) {
        return Ok(Err(status));
    }
    if headers.contains_key("transfer-encoding") {
        return Ok(Err(400));
    }
    let Some(length) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return Ok(Err(400));
    };
    if length > max {
        return Ok(Err(413));
    }
    let mut body = vec![0; length];
    let mut filled = 0;
    while filled < body.len() {
        if left().is_zero() {
            return Ok(Err(408));
        }
        reader.get_mut().set_read_timeout(Some(left()))?;
        let n = reader.read(&mut body[filled..])?;
        if n == 0 {
            return Ok(Err(400));
        }
        filled += n;
    }
    Ok(Ok(Request { headers, body }))
}

/// Answers with `status`, and `json` as the body if there is one, then
/// closes.
pub fn respond(stream: &mut TcpStream, status: u16, json: Option<&[u8]>) -> io::Result<()> {
    let body = json.unwrap_or_default();
    let kind = if json.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\n{kind}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}
