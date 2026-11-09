//! Bounded HTTP transport. Ingress never runs a flow pass on its request thread.
use conary_github::events::{MAX_BODY, Queue, Receiver};
use std::collections::BTreeMap;
use std::io::{self, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn serve(listener: TcpListener, receiver: Receiver, queue: Arc<Queue>) -> io::Result<()> {
    let receiver = Arc::new(Mutex::new(receiver));
    let open = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        let Some(slot) = super::Slot::take(&open, 64) else {
            continue;
        };
        let receiver = Arc::clone(&receiver);
        let queue = Arc::clone(&queue);
        std::thread::Builder::new().spawn(move || {
            let _slot = slot;
            let _ = connection(stream, &receiver, &queue);
        })?;
    }
    Ok(())
}
fn connection(mut stream: TcpStream, receiver: &Mutex<Receiver>, queue: &Queue) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let status = request(&mut stream, receiver, queue).unwrap_or(400);
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}
fn request(stream: &mut TcpStream, receiver: &Mutex<Receiver>, queue: &Queue) -> io::Result<u16> {
    let mut reader = BufReader::new(stream);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut head = Vec::new();
    // Total header cap; never a growing read_line on untrusted input.
    loop {
        if head.len() >= 16384 {
            return Ok(431);
        }
        reader.get_mut().set_read_timeout(Some(
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .max(Duration::from_millis(1)),
        ))?;
        if std::time::Instant::now() >= deadline {
            return Ok(408);
        }
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let Ok(head) = std::str::from_utf8(&head) else {
        return Ok(400);
    };
    let mut lines = head.lines();
    if !lines
        .next()
        .is_some_and(|l| l.starts_with("POST ") && l.ends_with(" HTTP/1.1"))
    {
        return Ok(401);
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let Some((key, value)) = line.split_once(':') else {
            return Ok(400);
        };
        if headers
            .insert(key.to_ascii_lowercase(), value.trim())
            .is_some()
        {
            return Ok(400);
        }
    }
    let signature = headers.get("x-hub-signature-256").copied();
    if signature.is_none() {
        return Ok(401);
    }
    if headers.contains_key("transfer-encoding") {
        return Ok(400);
    }
    let Some(length) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return Ok(400);
    };
    if length > MAX_BODY {
        return Ok(413);
    }
    let mut body = vec![0; length];
    // Each read uses the remainder of one request budget, including the body.
    let mut filled = 0;
    while filled < body.len() {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Ok(408);
        }
        reader.get_mut().set_read_timeout(Some(left))?;
        let n = reader.read(&mut body[filled..])?;
        if n == 0 {
            return Ok(400);
        }
        filled += n;
    }
    Ok(receiver.lock().unwrap().receive(
        signature,
        headers.get("x-github-delivery").copied().unwrap_or(""),
        headers.get("x-github-event").copied().unwrap_or(""),
        &body,
        conary_github::flow::jobs::now(),
        queue,
    ))
}
