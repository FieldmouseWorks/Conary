//! Passes of different repositories run at once, one at a time per
//! repository: conaryd's worker over a GitHub that answers HTTP, both
//! repositories' passes writing to one ledger anchored in a third.
use super::events_recovery::booked;
use crate::events::{FlowConfig, Kick, Queue, Recovery, Runtime, Work};
use crate::{Api, Tokens, anchor};
use conary_core::log;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const ANCHOR: &str = "o/ledger";
/// The repository whose API hangs, first in the mailbox's order.
const HANGING: &str = "o/a";
const ANSWERING: &str = "o/b";
/// Each call's cut-off here: the hanging read's deadline.
const CALL_FOR: Duration = Duration::from_secs(5);

/// GitHub, over HTTP: the anchor's refs and commits kept as written, every
/// call about [`HANGING`] held unanswered, and [`ANSWERING`] an empty
/// repository.
#[derive(Default)]
struct GitHub {
    refs: Mutex<BTreeMap<String, String>>,
    commits: Mutex<Vec<(String, Vec<String>)>>,
    /// Calls about [`HANGING`] held open now, the client still waiting.
    hanging: Mutex<usize>,
    asked: Mutex<Vec<String>>,
}

impl GitHub {
    fn serve() -> (Arc<Self>, String) {
        let github = Arc::new(Self::default());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let serving = Arc::clone(&github);
        thread::spawn(move || {
            for conn in listener.incoming() {
                let github = Arc::clone(&serving);
                thread::spawn(move || github.answer(conn.unwrap()));
            }
        });
        (github, base)
    }

    fn hanging(&self) -> usize {
        *self.hanging.lock().unwrap()
    }

    fn answer(&self, mut conn: TcpStream) {
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header == "\r\n" {
                break;
            }
            if let Some(n) = header.to_lowercase().strip_prefix("content-length:") {
                length = n.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let mut words = line.split(' ');
        let (method, path) = (words.next().unwrap(), words.next().unwrap());
        self.asked.lock().unwrap().push(format!("{method} {path}"));
        if path.starts_with(&format!("/repos/{HANGING}/")) {
            // Held until the client gives up on it: its read's deadline.
            *self.hanging.lock().unwrap() += 1;
            let _ = reader.read(&mut [0; 1]);
            *self.hanging.lock().unwrap() -= 1;
            return;
        }
        let body: Value = serde_json::from_slice(&body).unwrap_or_default();
        let (status, answer) = self.route(method, path, &body);
        let answer = answer.to_string();
        let _ = write!(
            conn,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
            answer.len()
        );
    }

    fn route(&self, method: &str, path: &str, body: &Value) -> (&'static str, Value) {
        let (path, _query) = path.split_once('?').unwrap_or((path, ""));
        if let Some(git) = path.strip_prefix(&format!("/repos/{ANCHOR}/git/")) {
            return self.git(method, git, body);
        }
        let Some(rest) = path.strip_prefix(&format!("/repos/{ANSWERING}")) else {
            return ("404 Not Found", json!({}));
        };
        match (method, rest) {
            ("GET", "/pulls") => ("200 OK", json!([])),
            _ => ("404 Not Found", json!({})),
        }
    }

    /// The anchor repository's refs and commits.
    fn git(&self, method: &str, git: &str, body: &Value) -> (&'static str, Value) {
        let mut refs = self.refs.lock().unwrap();
        let mut commits = self.commits.lock().unwrap();
        let commit = |sha: &str, commits: &[(String, Vec<String>)]| {
            let (message, parents) = &commits[sha.parse::<usize>().unwrap()];
            json!({
                "sha": sha,
                "message": message,
                "parents": parents.iter().map(|p| json!({"sha": p})).collect::<Vec<_>>(),
            })
        };
        match (method, git.split_once('/').unwrap_or((git, ""))) {
            ("GET", ("ref", name)) => match refs.get(&format!("refs/{name}")) {
                Some(sha) => ("200 OK", json!({"object": {"sha": sha}})),
                None => ("404 Not Found", json!({})),
            },
            ("GET", ("commits", sha)) => ("200 OK", commit(sha, &commits)),
            ("POST", ("trees", "")) => ("201 Created", json!({"sha": "tree"})),
            ("POST", ("commits", "")) => {
                let parents = body["parents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p.as_str().unwrap().to_owned())
                    .collect();
                let message = body["message"].as_str().unwrap().to_owned();
                commits.push((message, parents));
                (
                    "201 Created",
                    commit(&(commits.len() - 1).to_string(), &commits),
                )
            }
            ("POST", ("refs", "")) => {
                let name = body["ref"].as_str().unwrap().to_owned();
                refs.insert(name, body["sha"].as_str().unwrap().to_owned());
                ("201 Created", json!({}))
            }
            ("PATCH", ("refs", name)) => {
                refs.insert(
                    format!("refs/{name}"),
                    body["sha"].as_str().unwrap().to_owned(),
                );
                ("200 OK", json!({}))
            }
            _ => ("404 Not Found", json!({})),
        }
    }
}

fn api(base: &str) -> Api {
    Api::through(base, Tokens::given("t"), None, CALL_FOR)
}

/// conaryd's configuration for both repositories, the ledger anchored in
/// [`ANCHOR`]: each repository's pass opens the ledger for writing.
fn runtime(root: &Path, base: &str) -> Runtime {
    std::fs::write(root.join("flow.json"), "{}").unwrap();
    std::fs::write(root.join("policy.json"), "{}").unwrap();
    Runtime {
        listen: "127.0.0.1:0".parse().unwrap(),
        secret_path: root.join("secret"),
        repos: vec![HANGING.parse().unwrap(), ANSWERING.parse().unwrap()],
        data: root.join("data"),
        flow_config: FlowConfig::One(root.join("flow.json")),
        policy: root.join("policy.json"),
        token_command: "unused".into(),
        anchor_repo: Some(ANCHOR.into()),
        worker: root.join("cn"),
        api: base.into(),
    }
}

fn kick(queue: &Queue, repo: &str) {
    queue
        .kick(Kick {
            repo: repo.into(),
            pr: None,
        })
        .unwrap();
}

#[derive(Debug, PartialEq, Eq)]
enum Heard {
    Passed(String),
    Failed(String, Recovery),
}

#[test]
fn a_repository_whose_api_hangs_delays_only_its_own_pass() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (github, base) = GitHub::serve();
    let ledger = root.join("ledger");
    anchor::init(&ledger, &log::key_path(&ledger), &api(&base), ANCHOR).unwrap();
    let runtime = runtime(root, &base);
    let queue = Arc::new(Queue::open(&runtime.data).unwrap());
    kick(&queue, HANGING);
    let started = Instant::now();
    let (tx, heard) = mpsc::channel();
    {
        let queue = Arc::clone(&queue);
        thread::spawn(move || {
            let failed = tx.clone();
            queue.work(
                |work| {
                    let reports = runtime.pass_through(api(&base), &ledger, work, &queue);
                    if reports.is_ok() {
                        tx.send(Heard::Passed(work.repo.clone())).unwrap();
                    }
                    reports
                },
                |repo, recovery| failed.send(Heard::Failed(repo.into(), recovery)).unwrap(),
            )
        });
    }
    let next = || heard.recv_timeout(CALL_FOR * 4).unwrap();
    // The second repository is kicked once the first's read hangs.
    while github.hanging() == 0 {
        assert!(started.elapsed() < CALL_FOR, "the first's read never came");
        thread::sleep(Duration::from_millis(10));
    }
    kick(&queue, ANSWERING);

    // Its pass ends while the first's read still hangs.
    let first = next();
    assert_eq!(
        first,
        Heard::Passed(ANSWERING.into()),
        "the second repository's pass waited behind the first's hanging read"
    );
    assert_eq!(github.hanging(), 1, "the first's read is still held");
    let asked = github.asked.lock().unwrap().clone();
    assert!(
        asked
            .iter()
            .any(|a| a.starts_with(&format!("GET /repos/{ANSWERING}/"))),
        "{asked:?}"
    );

    // Once its read's deadline passes, the first books its recovery.
    let Heard::Failed(repo, recovery) = next() else {
        panic!("the hanging repository's pass didn't fail");
    };
    assert_eq!(repo, HANGING);
    assert_eq!(recovery.attempts, 1);
    assert!(started.elapsed() >= CALL_FOR, "{:?}", started.elapsed());
    assert_eq!(github.hanging(), 0);
    assert_eq!(queue.recovery(HANGING), Some(recovery));
    assert_eq!(queue.recovery(ANSWERING), None);
}

#[test]
fn two_passes_of_one_repository_never_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Arc::new(Queue::open(dir.path()).unwrap());
    let running: Arc<Mutex<BTreeSet<String>>> = Arc::default();
    let overlapped = Arc::new(AtomicBool::new(false));
    let (started, passes) = mpsc::channel::<String>();
    let (go_on, release) = mpsc::channel::<()>();
    let release = Mutex::new(release);
    kick(&queue, "o/r");
    {
        let (queue, running, overlapped) = (
            Arc::clone(&queue),
            Arc::clone(&running),
            Arc::clone(&overlapped),
        );
        thread::spawn(move || {
            queue.work(
                |work: &Work| {
                    if !running.lock().unwrap().insert(work.repo.clone()) {
                        overlapped.store(true, Ordering::SeqCst);
                    }
                    started.send(work.repo.clone()).unwrap();
                    if work.repo == "o/r" {
                        release.lock().unwrap().recv().unwrap();
                    }
                    running.lock().unwrap().remove(&work.repo);
                    Ok(vec![])
                },
                |_, _| {},
            )
        });
    }
    let next = || passes.recv_timeout(Duration::from_secs(5)).ok();
    let none = || passes.recv_timeout(Duration::from_millis(300)).ok();
    assert_eq!(next().as_deref(), Some("o/r"));
    // Kicked again while its pass runs: its dirty bit, and another
    // repository's pass runs meanwhile.
    kick(&queue, "o/r");
    kick(&queue, "o/r");
    kick(&queue, "a/x");
    assert_eq!(next().as_deref(), Some("a/x"));
    assert_eq!(
        none(),
        None,
        "a second pass of o/r started beside its first"
    );
    go_on.send(()).unwrap();
    assert_eq!(
        next().as_deref(),
        Some("o/r"),
        "its kicks run it again after"
    );
    go_on.send(()).unwrap();
    assert_eq!(none(), None, "its kicks coalesce into one pass");
    assert!(!overlapped.load(Ordering::SeqCst));
}

#[test]
fn a_running_repositorys_status_echo_waits_for_its_pass_beside_anothers() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    kick(&queue, "o/r");
    let ours = queue.take(1).unwrap().unwrap();
    // Its status's event comes before the response naming its ID, while
    // another repository's pass is taken.
    queue.status("o/r", 7).unwrap();
    kick(&queue, "a/x");
    let theirs = queue.take(1).unwrap().unwrap();
    assert_eq!(theirs.repo, "a/x");
    queue.own_status("o/r", 7, 1).unwrap();
    queue.finish(&theirs, &[], 1).unwrap();
    queue.finish(&ours, &[], 1).unwrap();
    assert!(
        queue.take(2).unwrap().is_none(),
        "its own status's echo ran it again"
    );
}

#[test]
fn every_pass_running_as_conaryd_stopped_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    kick(&queue, "a/x");
    kick(&queue, "o/r");
    queue.take(1).unwrap().unwrap();
    queue.take(1).unwrap().unwrap();
    drop(queue);
    let mailbox = || -> Value {
        serde_json::from_slice(&std::fs::read(dir.path().join("events.json")).unwrap()).unwrap()
    };
    assert_eq!(mailbox()["running"], json!(["a/x", "o/r"]));
    // An earlier conaryd reads no repository running, and kicks each it
    // lists as it starts: the mailbox has no `active` for it to misread.
    assert_eq!(mailbox().get("active"), None);
    let queue = Queue::open(dir.path()).unwrap();
    let mut ran = vec![];
    while let Some(work) = queue.take(1).unwrap() {
        ran.push(work.repo);
    }
    assert_eq!(ran, ["a/x", "o/r"]);
}

#[test]
fn a_mailbox_an_earlier_conaryd_wrote_opens() {
    let dir = tempfile::tempdir().unwrap();
    let mailbox = json!({"pending": ["a/x"], "active": "o/r", "waits": {}});
    std::fs::write(dir.path().join("events.json"), mailbox.to_string()).unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    assert_eq!(queue.take(1).unwrap().unwrap().repo, "a/x");
}

/// Kicks, status events, bookings and a due recovery of a repository whose
/// pass runs wake nothing: its pass ending does.
#[test]
fn the_sleeper_waits_for_a_running_pass_rather_than_spin_on_its_repository() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Arc::new(Queue::open(dir.path()).unwrap());
    let now = crate::flow::jobs::now();
    kick(&queue, "o/r");
    let work = queue.take(now - 100).unwrap().unwrap();
    queue.finish(&work, &[booked(now + 1)], now - 100).unwrap();
    kick(&queue, "o/r");
    let work = queue.take(now - 100).unwrap().unwrap();
    assert_eq!(queue.fail(&work, now - 100).unwrap().at, now - 40);
    // Its recovery runs, its booking coming due and a kick and a status
    // event arriving meanwhile, for two seconds and more.
    let work = queue.take(now).unwrap().expect("its recovery is due");
    kick(&queue, "o/r");
    queue.status("o/r", 7).unwrap();
    let ending = {
        let queue = Arc::clone(&queue);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(2500));
            queue.finish(&work, &[], now).unwrap();
        })
    };
    let slept = Instant::now();
    let mut wakes = 0;
    while queue.take(crate::flow::jobs::now()).unwrap().is_none() {
        assert!(slept.elapsed() < Duration::from_secs(10), "never woken");
        queue.wait();
        wakes += 1;
    }
    assert!(
        slept.elapsed() >= Duration::from_secs(2),
        "{:?}",
        slept.elapsed()
    );
    assert!(
        wakes <= 3,
        "the sleeper woke {wakes} times for a running repository"
    );
    ending.join().unwrap();
}

/// A running pass's due recovery and booking are its own: neither runs it
/// again after it ends.
#[test]
fn a_running_pass_is_not_run_again_for_what_it_started_with() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    kick(&queue, "o/r");
    let work = queue.take(1).unwrap().unwrap();
    assert_eq!(queue.fail(&work, 1).unwrap().at, 61);
    let work = queue.take(61).unwrap().expect("its recovery");
    // Another repository's pass is taken while its recovery runs.
    kick(&queue, "a/x");
    let other = queue.take(62).unwrap().unwrap();
    assert_eq!(other.repo, "a/x");
    queue.finish(&other, &[], 62).unwrap();
    queue.finish(&work, &[booked(100)], 61).unwrap();
    assert!(queue.take(99).unwrap().is_none(), "its recovery ran twice");

    // A booking that comes due while its pass runs is that pass's to book
    // again or not.
    let work = queue.take(100).unwrap().expect("its booking");
    queue.finish(&work, &[booked(200)], 100).unwrap();
    kick(&queue, "o/r");
    let work = queue.take(150).unwrap().unwrap();
    kick(&queue, "a/x");
    let other = queue.take(201).unwrap().unwrap();
    assert_eq!(other.repo, "a/x");
    queue.finish(&other, &[], 201).unwrap();
    queue.finish(&work, &[], 150).unwrap();
    assert!(
        queue.take(202).unwrap().is_none(),
        "a booking its pass didn't renew ran it again"
    );
}
