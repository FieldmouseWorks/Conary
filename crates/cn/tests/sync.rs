//! `cn sync` against a GitHub on localhost: its lines and exit statuses, as
//! burrow's `burrow-ledger-sync` reads them (0 synced, 75 booked for five
//! minutes on, anything else failed).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

const ANCHOR: &str = "FieldmouseWorks/ledger";

/// How a repo's PR list answers.
#[derive(Clone, Copy)]
enum Pulls {
    /// With no PRs.
    Empty,
    /// 503: GitHub unavailable.
    Unavailable,
    /// 404: refused, which asking again won't change.
    Missing,
}

/// GitHub's refs and their commits, as the ledger's anchor moves them, and
/// each repo's PR list.
#[derive(Default)]
struct State {
    refs: HashMap<String, String>,
    commits: HashMap<String, Value>,
    made: usize,
    pulls: HashMap<String, Pulls>,
}

/// Serves GitHub's API on localhost until the test ends; its root URL.
fn github(pulls: &[(&str, Pulls)]) -> String {
    let state = Arc::new(Mutex::new(State {
        pulls: pulls.iter().map(|(r, p)| (r.to_string(), *p)).collect(),
        ..State::default()
    }));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let state = Arc::clone(&state);
            std::thread::spawn(move || serve(stream.unwrap(), &state));
        }
    });
    url
}

fn serve(stream: TcpStream, state: &Mutex<State>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut words = first.split_whitespace();
    let (method, path) = (words.next().unwrap(), words.next().unwrap());
    let (status, answer) = answer(&mut state.lock().unwrap(), method, path, &body);
    let text = answer.to_string();
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    )
    .unwrap();
}

fn answer(state: &mut State, method: &str, path: &str, body: &Value) -> (u16, Value) {
    let missing = (404, json!({"message": "Not Found"}));
    let (path, _) = path.split_once('?').unwrap_or((path, ""));
    let rest = path.strip_prefix("/repos/").unwrap();
    let parts: Vec<&str> = rest.splitn(3, '/').collect();
    let (repo, rest) = (format!("{}/{}", parts[0], parts[1]), parts[2]);
    let commit = |state: &State, sha: &str| state.commits.get(sha).cloned();
    match (method, rest) {
        ("GET", "pulls") => match state.pulls.get(&repo) {
            Some(Pulls::Empty) => (200, json!([])),
            Some(Pulls::Unavailable) => (503, json!({"message": "Service Unavailable"})),
            Some(Pulls::Missing) | None => missing,
        },
        ("GET", r) if r.starts_with("git/ref/") => {
            let name = format!("refs/{}", &r["git/ref/".len()..]);
            match state.refs.get(&format!("{repo} {name}")) {
                Some(sha) => (200, json!({"ref": name, "object": {"sha": sha}})),
                None => missing,
            }
        }
        ("GET", r) if r.starts_with("git/commits/") => {
            commit(state, &r["git/commits/".len()..]).map_or(missing, |c| (200, c))
        }
        ("POST", "git/trees") => (201, json!({"sha": "e".repeat(40)})),
        ("POST", "git/commits") => {
            state.made += 1;
            let sha = format!("{:040x}", state.made);
            let parents: Vec<Value> = body["parents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| json!({"sha": p}))
                .collect();
            let c = json!({"sha": sha, "message": body["message"], "parents": parents, "tree": {"sha": body["tree"]}});
            state.commits.insert(sha.clone(), c);
            (201, json!({"sha": sha}))
        }
        ("POST", "git/refs") => {
            let name = body["ref"].as_str().unwrap();
            let sha = body["sha"].as_str().unwrap();
            state.refs.insert(format!("{repo} {name}"), sha.into());
            (201, json!({"ref": name, "object": {"sha": sha}}))
        }
        ("PATCH", r) if r.starts_with("git/refs/") => {
            let name = format!("refs/{}", &r["git/refs/".len()..]);
            let sha = body["sha"].as_str().unwrap();
            state.refs.insert(format!("{repo} {name}"), sha.into());
            (200, json!({"ref": name, "object": {"sha": sha}}))
        }
        _ => missing,
    }
}

fn cn(ledger: &Path, url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cn"))
        .args(args)
        .env_clear()
        .env("CONARY_LEDGER", ledger)
        .env("GITHUB_API_URL", url)
        .env("CONARY_ANCHOR_REPO", ANCHOR)
        .env("CONARY_REVIEWERS", "fieldmouse-steward")
        .env("CONARY_OWN_CONTEXTS", "review-settled")
        .env("GH_TOKEN", "a token")
        .output()
        .unwrap()
}

/// A ledger set up against GitHub at `url`.
fn ledger(url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let init = cn(dir.path(), url, &["init"]);
    assert!(init.status.success(), "{init:?}");
    dir
}

/// `cn sync` of `repos`: its exit status, and each line's repo and outcome.
fn sync(github: &[(&str, Pulls)], repos: &[&str]) -> (Option<i32>, Vec<(String, String)>) {
    let url = self::github(github);
    let dir = ledger(&url);
    let mut args = vec!["sync"];
    args.extend(repos);
    let out = cn(dir.path(), &url, &args);
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    let lines = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            (
                v["repo"].as_str().unwrap().into(),
                v["outcome"].as_str().unwrap().into(),
            )
        })
        .collect();
    (out.status.code(), lines)
}

fn ended(each: &[(&str, &str)]) -> Vec<(String, String)> {
    each.iter()
        .map(|(r, o)| (r.to_string(), o.to_string()))
        .collect()
}

#[test]
fn one_repo_exits_0_when_synced_75_when_github_is_unavailable_and_1_when_it_failed() {
    let r = "FieldmouseWorks/remi";
    assert_eq!(
        sync(&[(r, Pulls::Empty)], &[r]),
        (Some(0), ended(&[(r, "done")]))
    );
    assert_eq!(
        sync(&[(r, Pulls::Unavailable)], &[r]),
        (Some(75), ended(&[(r, "unavailable")]))
    );
    assert_eq!(
        sync(&[(r, Pulls::Missing)], &[r]),
        (Some(1), ended(&[(r, "failed")]))
    );
}

#[test]
fn several_repos_each_get_a_line_and_the_exit_says_the_worst() {
    let [a, b, c] = ["o/a", "o/b", "o/c"];
    let all = [
        (a, Pulls::Unavailable),
        (b, Pulls::Missing),
        (c, Pulls::Empty),
    ];
    assert_eq!(
        sync(&all, &[a, b, c]),
        (
            Some(1),
            ended(&[(a, "unavailable"), (b, "failed"), (c, "done")])
        )
    );
    assert_eq!(
        sync(&all, &[a, c]),
        (Some(75), ended(&[(a, "unavailable"), (c, "done")]))
    );
    assert_eq!(
        sync(&all, &[c, c]),
        (Some(0), ended(&[(c, "done"), (c, "done")]))
    );
}
