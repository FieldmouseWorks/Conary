//! The REST client against a stand-in GitHub on a local port: what it asks,
//! and how it reads each answer.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use super::*;

/// Serves `answers` in turn, one per connection, and sends back each
/// request's head (its request line and headers, lower-cased).
fn github(answers: Vec<&'static str>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for answer in answers {
            let (mut conn, _) = listener.accept().unwrap();
            let mut head = String::new();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                head.push_str(&line.to_lowercase());
            }
            conn.write_all(answer.as_bytes()).unwrap();
            tx.send(head).unwrap();
        }
    });
    (base, rx)
}

const PULL: &str = r#"{"number":7,"state":"open","title":"t","body":null,"user":{"login":"a","type":"User"},"merged":false,"merged_at":null,"merge_commit_sha":null,"base":{"sha":"b","ref":"main","repo":null},"head":{"sha":"h","ref":"x","repo":null}}"#;

fn ok(etag: &str) -> &'static str {
    format!(
        "HTTP/1.1 200 OK\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PULL}",
        PULL.len()
    )
    .leak()
}

const NOT_MODIFIED: &str = "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n";

#[test]
fn asks_whether_what_it_kept_is_current_and_reads_it_on_a_304() {
    let dir = tempfile::tempdir().unwrap();
    let (base, heads) = github(vec![ok("\"v1\""), NOT_MODIFIED]);
    let api = Api::new(&base, "t".into()).with_etags(Etags::open(dir.path()).unwrap());
    assert_eq!(api.pull("o/r", 7).unwrap().head.sha, "h");
    assert!(!heads.recv().unwrap().contains("if-none-match"));
    // The second answer has no body: what it reads is what was kept.
    assert_eq!(api.pull("o/r", 7).unwrap().head.sha, "h");
    assert!(heads.recv().unwrap().contains("if-none-match: \"v1\"\r\n"));
}

#[test]
fn without_etags_it_asks_plainly() {
    let (base, heads) = github(vec![ok("\"v1\""), ok("\"v1\"")]);
    let api = Api::new(&base, "t".into());
    api.pull("o/r", 7).unwrap();
    api.pull("o/r", 7).unwrap();
    assert!(!heads.recv().unwrap().contains("if-none-match"));
    assert!(!heads.recv().unwrap().contains("if-none-match"));
}

fn error(status: &str, headers: &str) -> ApiError {
    let answer =
        format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 2\r\nConnection: close\r\n\r\n{{}}");
    let (base, _heads) = github(vec![answer.leak()]);
    Api::new(&base, "t".into()).pull("o/r", 7).unwrap_err()
}

#[test]
fn tells_github_unavailable_from_a_request_refused() {
    // The primary rate limit spent, a secondary one, too many requests,
    // GitHub's own trouble: ask again later.
    assert!(error("403 Forbidden", "X-RateLimit-Remaining: 0\r\n").unavailable);
    assert!(error("403 Forbidden", "Retry-After: 60\r\n").unavailable);
    assert!(error("429 Too Many Requests", "").unavailable);
    assert!(error("502 Bad Gateway", "").unavailable);
    // Not allowed, or not a request GitHub takes: asking again won't help.
    assert!(!error("403 Forbidden", "X-RateLimit-Remaining: 4999\r\n").unavailable);
    assert!(!error("401 Unauthorized", "").unavailable);
    assert!(!error("422 Unprocessable Entity", "").unavailable);
}

#[test]
fn no_answer_at_all_is_github_unavailable() {
    // A port nothing listens on.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(
        Api::new(&base, "t".into())
            .pull("o/r", 7)
            .unwrap_err()
            .unavailable
    );
}
