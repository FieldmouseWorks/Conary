//! The REST client against a stand-in GitHub on a local port: what it asks,
//! and how it reads each answer.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use super::*;

/// Serves `answers` in turn, one per connection, and sends back each
/// request's head (its request line and headers, lower-cased). A request's
/// body is read before the answer goes out. An answer the client stops
/// reading, past its size limit, is let go.
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
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .map_or(0, |n| n.trim().parse().unwrap());
            reader.read_exact(&mut vec![0; length]).unwrap();
            _ = conn.write_all(answer.as_bytes());
            _ = tx.send(head);
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
    error_with(status, headers, "{}")
}

fn error_with(status: &str, headers: &str, body: &str) -> ApiError {
    let answer = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
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
    // A secondary limit with neither header: its message says so.
    let secondary = r#"{"message":"You have exceeded a secondary rate limit. Please wait a few minutes before you try again."}"#;
    assert!(
        error_with(
            "403 Forbidden",
            "X-RateLimit-Remaining: 4990\r\n",
            secondary
        )
        .unavailable
    );
    // Not allowed, or not a request GitHub takes: asking again won't help.
    let refused = r#"{"message":"Resource not accessible by personal access token"}"#;
    assert!(!error_with("403 Forbidden", "X-RateLimit-Remaining: 4990\r\n", refused).unavailable);
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

#[test]
fn a_request_that_can_never_be_made_is_not_github_unavailable() {
    // A root that isn't a URL, and a token no header can carry: asking again
    // fails the same way.
    let err = Api::new("not a url", "t".into())
        .pull("o/r", 7)
        .unwrap_err();
    assert!(!err.unavailable, "{err}");
    let (base, _heads) = github(vec![]);
    let err = Api::new(&base, "t\nx".into()).pull("o/r", 7).unwrap_err();
    assert!(!err.unavailable, "{err}");
}

/// What a read and a write each make of `answer`.
fn both(answer: String) -> [ApiError; 2] {
    let answer: &'static str = answer.leak();
    let (base, _heads) = github(vec![answer, answer]);
    let api = Api::new(&base, "t".into());
    let read = api.pull("o/r", 7).unwrap_err();
    let write = api
        .send::<serde_json::Value>(false, "/repos/o/r/git/trees", &serde_json::json!({}))
        .unwrap_err();
    [read, write]
}

/// An answer whose body ends, connection closed, before its length says.
fn cut_off(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len() + 100
    )
}

fn whole(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn an_answer_cut_off_is_github_unavailable() {
    // An answer, or a 403's message that would tell a secondary limit from a
    // refusal, lost partway: the network's doing, so ask again.
    for answer in [
        cut_off("200 OK", "", "{\"number\":"),
        cut_off("403 Forbidden", "", "{\"message\":\"You have exc"),
        cut_off("403 Forbidden", "X-RateLimit-Remaining: 4990\r\n", ""),
    ] {
        for err in both(answer.clone()) {
            assert!(err.unavailable, "{answer:?}: {err}");
        }
    }
}

#[test]
fn an_answer_too_big_or_not_json_stays_failed() {
    // A refusal whose message is past what's read of it, an answer past the
    // client's size limit, or one that isn't what was asked for: the same
    // next time.
    let long_refusal = format!(r#"{{"message":"{}"}}"#, "x".repeat(MAX_MESSAGE as usize));
    let huge = format!(r#"{{"x":"{}"}}"#, "x".repeat(11 * 1024 * 1024));
    for answer in [
        whole("403 Forbidden", &long_refusal),
        whole("200 OK", &huge),
        whole("200 OK", "not json"),
    ] {
        for err in both(answer) {
            assert!(!err.unavailable, "{err}");
        }
    }
}

/// What a read and a write each make of a proxy that answers a tunnel with
/// `answer`.
fn through_proxy(answer: &'static str) -> [ApiError; 2] {
    let (proxy, heads) = github(vec![answer, answer]);
    let api = Api::through(
        "https://127.0.0.1:1",
        "t".into(),
        Some(ureq::Proxy::new(&proxy).unwrap()),
    );
    let read = api.pull("o/r", 7).unwrap_err();
    assert!(heads.recv().unwrap().starts_with("connect "));
    let write = api
        .send::<serde_json::Value>(false, "/repos/o/r/git/trees", &serde_json::json!({}))
        .unwrap_err();
    assert!(heads.recv().unwrap().starts_with("connect "));
    [read, write]
}

#[test]
fn a_proxy_that_refuses_is_not_github_unavailable() {
    // Credentials missing or wrong, or the proxy's policy: asking again gets
    // the same refusal.
    for answer in [
        "HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
    ] {
        for err in through_proxy(answer) {
            assert!(!err.unavailable, "{answer:?}: {err}");
        }
    }
}

#[test]
fn a_proxy_in_trouble_or_gone_is_github_unavailable() {
    // The proxy can't reach GitHub, is overloaded, or drops the connection
    // without an answer.
    for answer in [
        "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n",
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
        "",
    ] {
        for err in through_proxy(answer) {
            assert!(err.unavailable, "{answer:?}: {err}");
        }
    }
}
