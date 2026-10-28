//! conaryd on a socket in a temporary directory, answering a ledger that
//! moves while it serves.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use conary_core::Store;
use conary_core::id::ChangeId;
use conary_core::log;
use conary_core::op::{Actor, ActorKind, Body};
use conary_core::reader::Reader;

fn observe(store: &mut Store, pr: u64) {
    store
        .record(Body::Observe {
            change: ChangeId::new().unwrap(),
            repo: "o/r".parse().unwrap(),
            pr,
            author: Actor {
                kind: ActorKind::Person,
                login: "someone".into(),
                session: None,
            },
        })
        .unwrap();
}

/// A ledger holding PR 1, and conaryd serving it at `socket`.
fn daemon(dir: &Path, socket: &Path) -> Store {
    let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let public = key.verifying_key();
    let lock = log::Lock::take(dir).unwrap();
    log::save_anchor(&lock, None, "c0").unwrap();
    let mut store = Store::open(lock, key, None).unwrap();
    observe(&mut store, 1);
    let reader = Reader::open(dir, public).unwrap();
    let listener = conaryd::bind(socket).unwrap();
    thread::spawn(move || conaryd::serve(&listener, reader));
    store
}

struct Client {
    lines: BufReader<UnixStream>,
    out: UnixStream,
}

impl Client {
    fn connect(socket: &Path) -> Self {
        let out = UnixStream::connect(socket).unwrap();
        // A daemon that never answers fails the test instead of hanging it.
        out.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Self {
            lines: BufReader::new(out.try_clone().unwrap()),
            out,
        }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.out, "{line}").unwrap();
    }

    fn reply(&mut self) -> Value {
        let mut line = String::new();
        self.lines.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send(&request.to_string());
        let reply = self.reply();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    fn prs(&mut self, id: u64) -> Vec<u64> {
        let reply = self.call(id, "change.list", json!({}));
        let list = reply["result"].as_array().unwrap();
        list.iter().map(|c| c["pr"].as_u64().unwrap()).collect()
    }
}

#[test]
fn a_connection_is_answered_in_order_from_the_ledger_as_it_moves() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run/conaryd.sock");
    let ledger = dir.path().join("ledger");
    let mut store = daemon(&ledger, &socket);

    let mut one = Client::connect(&socket);
    assert_eq!(one.prs(1), [1]);
    // A notification is answered with nothing, a blank line skipped, and the
    // next request still answered in turn.
    one.send(r#"{"jsonrpc":"2.0","method":"change.list"}"#);
    one.send("");
    let got = one.call(2, "change.get", json!({"repo": "o/r", "pr": 1}));
    assert_eq!(got["result"]["url"], "https://github.com/o/r/pull/1");

    // An op recorded while it serves is in the next answer, on every
    // connection.
    observe(&mut store, 2);
    let mut two = Client::connect(&socket);
    assert_eq!(two.prs(3), [1, 2]);
    assert_eq!(one.prs(4), [1, 2]);
    let refused = one.call(5, "change.delete", json!({}));
    assert_eq!(refused["error"]["code"], -32601);
}

#[test]
fn a_line_too_long_is_refused_and_the_connection_closed() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("conaryd.sock");
    let _store = daemon(&dir.path().join("ledger"), &socket);
    let mut client = Client::connect(&socket);
    let long = "x".repeat(usize::try_from(conaryd::MAX_LINE).unwrap() + 1);
    client.send(&long);
    assert_eq!(client.reply()["error"]["code"], -32700);
    let mut rest = Vec::new();
    client.lines.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty());
}

#[test]
fn a_socket_is_taken_over_only_from_a_daemon_thats_gone() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("conaryd.sock");
    // Left behind by one that's gone: taken over.
    drop(UnixListener::bind(&socket).unwrap());
    let live = conaryd::bind(&socket).unwrap();
    // Still answered on: refused, and left alone.
    let e = conaryd::bind(&socket).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AddrInUse, "{e}");
    UnixStream::connect(&socket).unwrap();
    drop(live);

    // Not a socket at all: refused, and kept.
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "mine").unwrap();
    assert!(conaryd::bind(&file).is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
}
