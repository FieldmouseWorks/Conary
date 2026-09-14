// apps/conary/tests/cli_repository_sync/fixture.rs

#![cfg(test)]

use conary_core::db::models::Repository;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug)]
pub struct Mode {
    pub tty: bool,
    pub no_color: bool,
}

pub const MODES: [Mode; 4] = [
    Mode {
        tty: false,
        no_color: false,
    },
    Mode {
        tty: false,
        no_color: true,
    },
    Mode {
        tty: true,
        no_color: false,
    },
    Mode {
        tty: true,
        no_color: true,
    },
];

pub struct Capture {
    pub code: i32,
    pub text: String,
    pub stdout: String,
    pub stderr: String,
}

impl Capture {
    pub fn rows(&self, tag: &str, name: &str) -> usize {
        self.text
            .lines()
            .filter(|line| {
                line.strip_prefix(tag)
                    .is_some_and(|value| value.trim_start() == name)
            })
            .count()
    }
}

pub fn run(mode: Mode, db: &Path, args: &[&str]) -> Capture {
    let mut command = if mode.tty {
        let arguments = (0..args.len())
            .map(|index| format!("\"$CONARY_SYNC_ARG_{index}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let mut command = Command::new("script");
        command.args([
            "-qec",
            &format!("exec \"$CONARY_SYNC_EXE\" {arguments} --db-path \"$CONARY_SYNC_DB\""),
            "/dev/null",
        ]);
        command.env("CONARY_SYNC_EXE", env!("CARGO_BIN_EXE_conary"));
        command.env("CONARY_SYNC_DB", db);
        for (index, argument) in args.iter().enumerate() {
            command.env(format!("CONARY_SYNC_ARG_{index}"), argument);
        }
        command
    } else {
        let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
        command.args(args).arg("--db-path").arg(db);
        command
    };
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("CONARY_TEST_") {
            command.env_remove(name);
        }
    }
    command
        .env("TERM", "xterm")
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE");
    if mode.no_color {
        command.env("NO_COLOR", "1");
    }
    let output = command
        .output()
        .expect("PTY capture requires util-linux script");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    if mode.tty {
        assert!(stderr.is_empty(), "{stderr}");
    }
    let raw = format!("{stdout}{stderr}");
    assert_eq!(
        raw.contains('\x1b'),
        mode.tty && !mode.no_color,
        "{mode:?}: {raw:?}"
    );
    assert!(!raw.contains("0/0"), "{raw:?}");
    if let Some((_, durable)) = raw.split_once("Repository synchronization:") {
        assert!(
            !durable.contains("\x1b[2K") && !durable.contains("\x1b[1A"),
            "redraw after durable result: {durable:?}"
        );
        assert!(!durable.replace("\r\n", "\n").contains('\r'), "{durable:?}");
    }
    Capture {
        code: output.status.code().unwrap(),
        text: console::strip_ansi_codes(&raw).replace("\r\n", "\n"),
        stdout,
        stderr,
    }
}

pub fn success(db: &Path, args: &[&str]) -> Capture {
    let capture = run(MODES[1], db, args);
    assert_eq!(capture.code, 0, "{:?}: {}", args, capture.text);
    capture
}

pub fn add(db: &Path, server: &Server, name: &str, route: &str) {
    success(
        db,
        &[
            "repo",
            "add",
            name,
            &format!("{}{route}", server.url),
            "--package-format",
            "json",
            "--yes",
        ],
    );
}

pub fn stale(db: &Path, name: &str) {
    let conn = conary_core::db::open(db).unwrap();
    let mut repo = Repository::find_by_name(&conn, name).unwrap().unwrap();
    repo.last_checked_at = Some("2000-01-01T00:00:00Z".into());
    repo.update(&conn).unwrap();
}

pub fn snapshot(db: &Path, name: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let conn = conary_core::db::open(db).unwrap();
    let repo = Repository::find_by_name(&conn, name).unwrap().unwrap();
    let mut stmt = conn
        .prepare("SELECT version FROM repository_packages WHERE repository_id = ?1 ORDER BY name")
        .unwrap();
    let versions = stmt
        .query_map([repo.id.unwrap()], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    (repo.last_checked_at, repo.last_published_at, versions)
}

/// Decode exact shell arguments before executing the printed action.
pub fn execute_recovery(action: &str, expected: &[&str]) -> Capture {
    assert!(action.starts_with("conary "), "{action}");
    let decoded = Command::new("sh")
        .args([
            "-c",
            &format!("conary() {{ printf '%s\\0' \"$@\"; }}; {action}"),
        ])
        .output()
        .unwrap();
    assert!(
        decoded.status.success() && decoded.stderr.is_empty(),
        "{decoded:?}"
    );
    let args: Vec<_> = decoded
        .stdout
        .strip_suffix(&[0])
        .unwrap()
        .split(|byte| *byte == 0)
        .map(|arg| std::str::from_utf8(arg).unwrap())
        .collect();
    assert_eq!(args, expected);
    let action = action.replacen("conary", "\"$CONARY_SYNC_EXE\"", 1);
    let output = Command::new("sh")
        .args(["-c", &action])
        .env("CONARY_SYNC_EXE", env!("CARGO_BIN_EXE_conary"))
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stdout}\n{stderr}");
    Capture {
        code: 0,
        text: format!("{stdout}{stderr}"),
        stdout,
        stderr,
    }
}

type Responses = Arc<Mutex<BTreeMap<String, (u16, Vec<u8>)>>>;

pub struct Server {
    pub url: String,
    responses: Responses,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let responses: Responses = Arc::default();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (replies, seen, stopping) = (
            Arc::clone(&responses),
            Arc::clone(&requests),
            Arc::clone(&stop),
        );
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                            .unwrap();
                        let mut request = String::new();
                        BufReader::new(&mut stream).read_line(&mut request).unwrap();
                        let path = request.split_whitespace().nth(1).unwrap().to_owned();
                        seen.lock().unwrap().push(path.clone());
                        let (status, body) = replies
                            .lock()
                            .unwrap()
                            .get(&path)
                            .cloned()
                            .unwrap_or((404, vec![]));
                        // Ensure real terminal captures exercise steady progress redraws.
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                        stream.write_all(&body).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5))
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
        });
        Self {
            url,
            responses,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    pub fn metadata(&self, route: &str, version: &str) {
        let body = serde_json::json!({"name":"fixture", "version":"1", "packages":[{
            "name":"fixture-package", "version":version, "release":"1", "version_scheme":"rpm",
            "checksum":"a".repeat(64), "size":17, "download_url":"https://example.invalid/fixture.rpm"
        }]}).to_string().into_bytes();
        self.responses
            .lock()
            .unwrap()
            .insert(format!("{route}/metadata.json"), (200, body));
    }

    pub fn fail(&self, route: &str, status: u16) {
        self.responses
            .lock()
            .unwrap()
            .insert(format!("{route}/metadata.json"), (status, vec![]));
    }

    pub fn take_requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
