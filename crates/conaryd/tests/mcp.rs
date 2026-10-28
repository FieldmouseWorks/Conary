//! conary-mcp as an MCP client starts it, over stdio, in front of a conaryd
//! serving a ledger; and the client it forwards with.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use serde_json::{Value, json};

use conary_core::Store;
use conary_core::id::{ChangeId, CommitId, RevisionId};
use conary_core::log;
use conary_core::op::{Actor, ActorKind, Authority, Body};
use conary_core::reader::Reader;
use conaryd::Limits;

/// Git in `dir`, as a person with no config of their own would run it.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A checkout of o/r with two commits, and conaryd at `socket` serving a
/// ledger in which PR 1 landed the second.
fn daemon(root: &Path, socket: &Path) -> (PathBuf, Store) {
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "--quiet", "-b", "main"]);
    git(
        &work,
        &["remote", "add", "origin", "https://github.com/o/r"],
    );
    let commit = |text: &str| -> CommitId {
        std::fs::write(work.join("f"), text).unwrap();
        git(&work, &["add", "f"]);
        git(&work, &["commit", "--quiet", "-m", text]);
        git(&work, &["rev-parse", "HEAD"]).parse().unwrap()
    };
    let (a, b) = (commit("one\n"), commit("one\ntwo\n"));

    let ledger = root.join("ledger");
    let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let public = key.verifying_key();
    let lock = log::Lock::take(&ledger).unwrap();
    log::save_anchor(&lock, None, "c0").unwrap();
    let mut store = Store::open(lock, key, None).unwrap();
    let change = ChangeId::new().unwrap();
    for body in [
        Body::Observe {
            change,
            repo: "o/r".parse().unwrap(),
            pr: 1,
            author: Actor {
                kind: ActorKind::Agent,
                login: "someone".into(),
                session: None,
            },
        },
        Body::Revision {
            change,
            base: a.clone(),
            head: b.clone(),
        },
        Body::Land {
            change,
            revision: RevisionId::new(&a, &b),
            before: a,
            after: b,
            by: Authority::Status {
                login: "steward".into(),
            },
        },
    ] {
        store.record(body).unwrap();
    }
    let reader = Reader::open(&ledger, public).unwrap();
    let bound = conaryd::bind(socket).unwrap();
    thread::spawn(move || conaryd::serve(&bound.listener, reader, Limits::default()));
    (work, store)
}

#[test]
fn an_agent_lists_the_tools_and_calls_them_through_conaryd() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("conaryd.sock");
    let (work, _store) = daemon(dir.path(), &socket);
    let mut mcp = Command::new(env!("CARGO_BIN_EXE_conary-mcp"))
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = mcp.stdin.take().unwrap();
    let mut stdout = BufReader::new(mcp.stdout.take().unwrap());
    let mut ask = |id: u64, method: &str, params: Value| -> Value {
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{line}").unwrap();
        let mut reply = String::new();
        stdout.read_line(&mut reply).unwrap();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["id"], id, "{reply}");
        reply["result"].clone()
    };
    let text = |result: &Value| -> Value {
        assert_eq!(result["isError"], false, "{result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    };

    let init = ask(
        0,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    assert_eq!(init["protocolVersion"], "2025-06-18");
    let tools = ask(1, "tools/list", json!({}));
    let names: Vec<_> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "change_get",
            "change_list",
            "evidence_query",
            "landings",
            "why"
        ]
    );

    let changes = text(&ask(2, "tools/call", json!({"name": "change_list"})));
    assert_eq!(changes[0]["url"], "https://github.com/o/r/pull/1");

    // why blames here and answers from conaryd's landings.
    let at =
        |line: u64| json!({"name": "why", "arguments": {"dir": work, "path": "f", "line": line}});
    let why = text(&ask(3, "tools/call", at(2)));
    assert_eq!(
        (&why["landed"]["pr"], &why["source"]),
        (&json!(1), &json!("ledger"))
    );
    // The first line came before that landing, which doesn't answer for it.
    assert!(text(&ask(4, "tools/call", at(1)))["landed"].is_null());

    // conaryd's refusal is the tool's error, for the agent to read.
    let refused = ask(
        5,
        "tools/call",
        json!({"name": "change_get", "arguments": {"change": "not an id"}}),
    );
    assert_eq!(refused["isError"], true);
    let said = refused["content"][0]["text"].as_str().unwrap();
    assert!(said.starts_with("conaryd: "), "{said}");

    drop(stdin);
    assert!(mcp.wait().unwrap().success());
}

#[test]
fn the_client_says_when_conaryd_isnt_there_and_reconnects_when_it_drops() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("conaryd.sock");
    let mut client = conaryd::Client::new(&socket);
    let e = client.call("change.list", &json!({})).unwrap_err();
    assert!(matches!(e, conaryd::CallError::Io(_)), "{e}");

    // A conaryd that answers one request on each connection, then hangs up.
    let listener = UnixListener::bind(&socket).unwrap();
    thread::spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let stream = stream.unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let id = serde_json::from_str::<Value>(&line).unwrap()["id"].clone();
            let reply = json!({"jsonrpc": "2.0", "id": id, "result": n});
            writeln!(&stream, "{reply}").unwrap();
        }
    });
    assert_eq!(client.call("change.list", &json!({})).unwrap(), 0);
    assert_eq!(client.call("change.list", &json!({})).unwrap(), 1);
}
