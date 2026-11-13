//! conaryd itself, its flow worker on, while GitHub is unavailable: the
//! failed pass is booked for recovery and the ledger socket goes on answering.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use conary_core::Store;
use conary_core::id::ChangeId;
use conary_core::log;
use conary_core::op::{Actor, ActorKind, Body};
use ring::hmac;
use serde_json::{Value, json};

const SECRET: &str = "a-webhook-secret-that-stays-unlogged";
const MARKER: &str = "a-body-marker-that-stays-unlogged";

/// A ledger in `dir` holding PR 1.
fn ledger(dir: &Path) {
    let lock = log::Lock::take(dir).unwrap();
    let key = log::load_or_create_key(&log::key_path(dir)).unwrap();
    log::save_anchor(&lock, None, "c0").unwrap();
    let mut store = Store::open(lock, key, None).unwrap();
    store
        .record(Body::Observe {
            change: ChangeId::new().unwrap(),
            repo: "o/r".parse().unwrap(),
            pr: 1,
            author: Actor {
                kind: ActorKind::Person,
                login: "someone".into(),
                session: None,
            },
        })
        .unwrap();
}

/// A local address nothing listens on, once its listener is dropped.
fn free() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn signature(body: &[u8]) -> String {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, SECRET.as_bytes()), body);
    let hex: String = tag.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
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
fn the_socket_answers_while_github_is_unavailable_and_the_pass_is_booked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ledger(&root.join("ledger"));
    let data = root.join("data");
    std::fs::write(root.join("secret"), SECRET).unwrap();
    std::fs::write(root.join("flow.json"), "{}").unwrap();
    std::fs::write(root.join("policy.json"), "{}").unwrap();
    let listen = free();
    let config = json!({
        "listen": listen.to_string(),
        "secret_path": root.join("secret"),
        "repos": ["o/r"],
        "data": data,
        "flow_config": root.join("flow.json"),
        "policy": root.join("policy.json"),
        "token_command": "echo token",
        "worker": root.join("cn"),
        // GitHub, down: nothing answers there.
        "api": format!("http://{}", free()),
    });
    std::fs::write(root.join("events.json"), config.to_string()).unwrap();
    let socket = root.join("run/conaryd.sock");
    let mut command = Command::new(env!("CARGO_BIN_EXE_conaryd"));
    command
        .arg("--ledger")
        .arg(root.join("ledger"))
        .arg("--socket")
        .arg(&socket)
        .arg("--events-config")
        .arg(root.join("events.json"))
        .stderr(Stdio::piped());
    for proxy in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        command.env_remove(proxy).env_remove(proxy.to_lowercase());
    }
    let mut daemon = Daemon(command.spawn().unwrap());
    let alive = |daemon: &mut Daemon| {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            let mut err = String::new();
            daemon
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut err)
                .unwrap();
            panic!("conaryd stopped ({status}) instead of booking recovery:\n{err}");
        }
    };

    let body = serde_json::to_vec(&json!({
        "repository": {"full_name": "o/r"},
        "pull_request": {"number": 1},
        "marker": MARKER,
    }))
    .unwrap();
    let signed = signature(&body);
    let started = Instant::now();
    let mut stream = loop {
        match TcpStream::connect(listen) {
            Ok(stream) => break stream,
            Err(_) if started.elapsed() < Duration::from_secs(10) => {
                alive(&mut daemon);
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("the webhook listener never came up: {e}"),
        }
    };
    write!(
        stream,
        "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nX-Hub-Signature-256: {signed}\r\nX-GitHub-Delivery: one\r\nX-GitHub-Event: pull_request\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 202"), "{answer}");

    // The pass fails on GitHub, and its repo's recovery is booked.
    let mailbox = data.join("events.json");
    let recovery = loop {
        alive(&mut daemon);
        let state: Value = std::fs::read(&mailbox)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if state["recovery"]["o/r"].is_object() {
            break state["recovery"]["o/r"].clone();
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no recovery booked: {state}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(recovery["attempts"], 1);

    // The ledger's calls are still answered, from the same conaryd.
    let mut client = conaryd::Client::new(&socket);
    let list = client.call("change.list", &json!({})).unwrap();
    let prs: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["pr"].as_u64().unwrap())
        .collect();
    assert_eq!(prs, [1]);
    thread::sleep(Duration::from_millis(200));
    alive(&mut daemon);

    daemon.0.kill().unwrap();
    daemon.0.wait().unwrap();
    let mut err = String::new();
    daemon
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(
        err.contains("a flow pass of o/r failed (1 in a row)"),
        "{err}"
    );
    for kept in [SECRET, MARKER, signed.as_str(), "token"] {
        assert!(!err.contains(kept), "the log holds {kept:?}:\n{err}");
    }
}
