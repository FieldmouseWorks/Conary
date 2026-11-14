//! The gate-reuse lookup's HTTP interface: its listener, in process, and
//! conaryd itself serving it from a machine's configuration.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use conary_core::Store;
use conary_core::log;
use conary_github::reuse::{CONTRACT, MAX_BODY, Reply};
use serde_json::{Value, json};

/// Sends `head` and then `body` raw, and reads the answer whole: its
/// status line, headers and body.
fn send(addr: SocketAddr, head: &str, body: &[u8]) -> (String, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    // A peer that answers before it has read the body may close first.
    let _ = stream.write_all(body);
    let mut out = Vec::new();
    stream.read_to_end(&mut out).unwrap();
    let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(out[..at].to_vec()).unwrap();
    let (status, headers) = head.split_once("\r\n").unwrap_or((&head, ""));
    (status.into(), headers.into(), out[at + 4..].to_vec())
}

fn post(addr: SocketAddr, authorization: Option<&str>, body: &[u8]) -> (String, String, Vec<u8>) {
    let auth = authorization.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
    let head = format!(
        "POST /gate-reuse HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\n\r\n",
        body.len()
    );
    send(addr, &head, body)
}

/// What the lookup was handed: each request's authorization and body.
type Seen = Arc<Mutex<Vec<(Option<String>, Vec<u8>)>>>;

#[test]
fn the_listener_hands_each_request_to_the_lookup_and_refuses_what_isnt_one() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let kept = Arc::clone(&seen);
    thread::spawn(move || {
        conaryd::reuse::serve(listener, move |auth, body| {
            kept.lock()
                .unwrap()
                .push((auth.map(str::to_owned), body.to_vec()));
            Reply {
                status: 200,
                body: json!({"contract": CONTRACT, "query": "q", "evidence": "miss", "why": "w"}),
            }
        })
        .unwrap();
    });

    let (status, headers, body) = post(addr, Some("Bearer a.b.c"), br#"{"a":1}"#);
    assert_eq!(status, "HTTP/1.1 200 Response");
    assert!(
        headers.contains("Content-Type: application/json"),
        "{headers}"
    );
    let answer: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(answer["evidence"], "miss");
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [(Some("Bearer a.b.c".into()), br#"{"a":1}"#.to_vec())]
    );
    let (status, _, _) = post(addr, None, b"{}");
    assert_eq!(status, "HTTP/1.1 200 Response");
    assert_eq!(seen.lock().unwrap()[1].0, None);

    // Never handed on: another method, a body too long, a length not given,
    // a chunked body, a header twice, a head past its cap.
    let before = seen.lock().unwrap().len();
    let refused = |head: &str, body: &[u8]| send(addr, head, body).0;
    assert!(refused("GET / HTTP/1.1\r\nHost: x\r\n\r\n", b"").starts_with("HTTP/1.1 401"));
    let long = format!(
        "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY + 1
    );
    assert!(refused(&long, b"").starts_with("HTTP/1.1 413"));
    assert!(refused("POST / HTTP/1.1\r\n\r\n", b"").starts_with("HTTP/1.1 400"));
    assert!(
        refused(
            "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n",
            b"{}"
        )
        .starts_with("HTTP/1.1 400")
    );
    assert!(
        refused(
            "POST / HTTP/1.1\r\nAuthorization: a\r\nauthorization: b\r\nContent-Length: 2\r\n\r\n",
            b"{}"
        )
        .starts_with("HTTP/1.1 400")
    );
    // Exactly the cap, unended, so the listener has read all of it when it
    // answers.
    let start = "POST / HTTP/1.1\r\nX: ";
    let huge = format!(
        "{start}{}",
        "x".repeat(conaryd::http::MAX_HEAD - start.len())
    );
    assert!(refused(&huge, b"").starts_with("HTTP/1.1 431"));
    assert_eq!(seen.lock().unwrap().len(), before);
}

/// A ledger in `dir`, set up as `cn init` sets one up.
fn ledger(dir: &Path) {
    let lock = log::Lock::take(dir).unwrap();
    let key = log::load_or_create_key(&log::key_path(dir)).unwrap();
    log::save_anchor(&lock, None, "c0").unwrap();
    Store::open(lock, key, None).unwrap();
}

/// Kills conaryd however the test ends.
struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn conaryd_answers_lookups_from_the_machines_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ledger(&root.join("ledger"));
    let listen = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let nothing = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let config = json!({
        "listen": listen.to_string(),
        "repos": ["example/project"],
        "token_command": "echo token",
        "signer": "example/ci/.github/workflows/gate.yml",
        // GitHub, down: nothing answers there.
        "api": format!("http://{nothing}"),
    });
    std::fs::write(root.join("gate-reuse.json"), config.to_string()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_conaryd"));
    command
        .arg("--ledger")
        .arg(root.join("ledger"))
        .arg("--socket")
        .arg(root.join("run/conaryd.sock"))
        .arg("--gate-reuse-config")
        .arg(root.join("gate-reuse.json"))
        .stderr(Stdio::piped());
    let mut daemon = Daemon(command.spawn().unwrap());
    let started = Instant::now();
    while TcpStream::connect(listen).is_err() {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            let mut err = String::new();
            daemon
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut err)
                .unwrap();
            panic!("conaryd stopped ({status}):\n{err}");
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "it never listened"
        );
        thread::sleep(Duration::from_millis(50));
    }

    let tree = "7".repeat(40);
    let gate = json!({"source": "tree", "workflow": {"commit": "5".repeat(40),
        "file": ".github/workflows/gate.yml", "repository": "example/ci", "tree": tree}});
    let request = |repo: &str| {
        serde_json::to_vec(&json!({"commit": "c".repeat(40), "contract": CONTRACT,
            "gate": gate, "repository": repo, "tree": tree}))
        .unwrap()
    };
    let refused = |auth: Option<&str>, body: &[u8]| {
        let (status, _, body) = post(listen, auth, body);
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["contract"], CONTRACT);
        assert!(body.get("evidence").is_none(), "{body}");
        status
    };
    assert!(refused(Some("Bearer x"), b"{}").starts_with("HTTP/1.1 400"));
    assert!(refused(Some("Bearer x"), &request("example/other")).starts_with("HTTP/1.1 403"));
    assert!(refused(None, &request("example/project")).starts_with("HTTP/1.1 401"));
    assert!(daemon.0.try_wait().unwrap().is_none());
}
