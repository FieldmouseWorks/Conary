//! The REST client against a stand-in GitHub on a local port: what it asks,
//! and how it reads each answer.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use super::*;

/// Serves `answers` in turn, one per connection, and sends back each
/// request's head (its request line and headers, lower-cased), then a blank
/// line and its body as it came. The body is read before the answer goes
/// out. An answer the client stops reading, past its size limit, is let go.
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
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            _ = conn.write_all(answer.as_bytes());
            _ = tx.send(format!("{head}\r\n{}", String::from_utf8(body).unwrap()));
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
    let api = Api::new(&base, Tokens::given("t")).with_etags(Etags::open(dir.path()).unwrap());
    assert_eq!(api.pull("o/r", 7).unwrap().head.sha, "h");
    assert!(!heads.recv().unwrap().contains("if-none-match"));
    // The second answer has no body: what it reads is what was kept.
    assert_eq!(api.pull("o/r", 7).unwrap().head.sha, "h");
    assert!(heads.recv().unwrap().contains("if-none-match: \"v1\"\r\n"));
}

#[test]
fn without_etags_it_asks_plainly() {
    let (base, heads) = github(vec![ok("\"v1\""), ok("\"v1\"")]);
    let api = Api::new(&base, Tokens::given("t"));
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
    Api::new(&base, Tokens::given("t"))
        .pull("o/r", 7)
        .unwrap_err()
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
        Api::new(&base, Tokens::given("t"))
            .pull("o/r", 7)
            .unwrap_err()
            .unavailable
    );
}

#[test]
fn a_request_that_can_never_be_made_is_not_github_unavailable() {
    // A root that isn't a URL, and a token no header can carry: asking again
    // fails the same way.
    let err = Api::new("not a url", Tokens::given("t"))
        .pull("o/r", 7)
        .unwrap_err();
    assert!(!err.unavailable, "{err}");
    let (base, _heads) = github(vec![]);
    let err = Api::new(&base, Tokens::given("t\nx"))
        .pull("o/r", 7)
        .unwrap_err();
    assert!(!err.unavailable, "{err}");
}

/// What a read and a write each make of `answer`.
fn both(answer: String) -> [ApiError; 2] {
    let answer: &'static str = answer.leak();
    let (base, _heads) = github(vec![answer, answer]);
    let api = Api::new(&base, Tokens::given("t"));
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
        Tokens::given("t"),
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

#[test]
fn each_request_is_asked_with_its_repository_s_token() {
    let dir = tempfile::tempdir().unwrap();
    let script = crate::tokens::tests::script(dir.path(), "echo \"for-$1\" | tr / -");
    let (base, heads) = github(vec![ok("\"v1\""), ok("\"v1\"")]);
    let api = Api::new(&base, Tokens::command(&script).unwrap());
    api.pull("a/r", 7).unwrap();
    assert!(
        heads
            .recv()
            .unwrap()
            .contains("authorization: bearer for-a-r\r\n")
    );
    api.pull("b/s", 7).unwrap();
    assert!(
        heads
            .recv()
            .unwrap()
            .contains("authorization: bearer for-b-s\r\n")
    );
}

#[test]
fn reads_a_path_s_repository() {
    for (path, repo) in [
        ("/repos/o/r/pulls/7", Some("o/r")),
        ("/repos/o/r", Some("o/r")),
        ("/repos/o/r?x=1", Some("o/r")),
        ("/repos/o/r/git/refs/notes/conary", Some("o/r")),
        ("/repos/o", None),
        ("/repos//r/pulls", None),
        ("/repos/o//pulls", None),
        ("/user", None),
    ] {
        assert_eq!(repo_of(path), repo, "{path}");
    }
}

#[test]
fn the_trains_merges_say_merged_already_there_or_conflict() {
    use crate::train::{Forge, Merged};
    let made = r#"{"sha":"m","commit":{"tree":{"sha":"t"}}}"#;
    let answers: Vec<&'static str> = vec![
        whole("201 Created", made).leak(),
        whole("204 No Content", "").leak(),
        whole("409 Conflict", r#"{"message":"Merge conflict"}"#).leak(),
        whole("404 Not Found", r#"{"message":"Head does not exist"}"#).leak(),
    ];
    let (base, heads) = github(answers);
    let api = Api::new(&base, Tokens::given("t"));
    let merge = || api.merge("o/r", "conary/build/x", "h", "Merge #1");
    assert_eq!(
        merge().unwrap(),
        Merged::Commit {
            sha: "m".into(),
            tree: "t".into()
        }
    );
    assert!(heads.recv().unwrap().starts_with("post /repos/o/r/merges "));
    assert_eq!(merge().unwrap(), Merged::Already);
    assert_eq!(merge().unwrap(), Merged::Conflict);
    assert!(merge().is_err());
}

#[test]
fn the_trains_branches_are_read_made_and_deleted_by_name() {
    use crate::train::Forge;
    let refs = r#"[{"ref":"refs/heads/conary/train/a","object":{"sha":"s"}}]"#;
    let answers: Vec<&'static str> = vec![
        whole("200 OK", r#"{"object":{"sha":"s"}}"#).leak(),
        whole("404 Not Found", "{}").leak(),
        whole("200 OK", refs).leak(),
        whole("201 Created", "{}").leak(),
        whole("204 No Content", "").leak(),
        whole(
            "422 Unprocessable Entity",
            r#"{"message":"Reference does not exist"}"#,
        )
        .leak(),
        whole(
            "403 Forbidden",
            r#"{"message":"Resource not accessible by integration"}"#,
        )
        .leak(),
    ];
    let (base, heads) = github(answers);
    let api = Api::new(&base, Tokens::given("t"));
    assert_eq!(api.branch("o/r", "main").unwrap(), Some("s".into()));
    assert!(
        heads
            .recv()
            .unwrap()
            .starts_with("get /repos/o/r/git/ref/heads/main ")
    );
    assert_eq!(api.branch("o/r", "gone").unwrap(), None);
    heads.recv().unwrap();
    assert_eq!(api.branches("o/r", "conary/").unwrap(), ["conary/train/a"]);
    assert!(
        heads
            .recv()
            .unwrap()
            .starts_with("get /repos/o/r/git/matching-refs/heads/conary/ ")
    );
    api.create_branch("o/r", "conary/train/a", "s").unwrap();
    assert!(
        heads
            .recv()
            .unwrap()
            .starts_with("post /repos/o/r/git/refs ")
    );
    api.delete_branch("o/r", "conary/train/a").unwrap();
    assert!(
        heads
            .recv()
            .unwrap()
            .starts_with("delete /repos/o/r/git/refs/heads/conary/train/a ")
    );
    // Already gone is gone; refused isn't.
    api.delete_branch("o/r", "conary/train/a").unwrap();
    assert!(api.delete_branch("o/r", "conary/train/a").is_err());
}

#[test]
fn a_prs_merge_is_at_its_head_the_way_the_repo_merges_or_refused() {
    use crate::train::{Forge, PrMerge};
    let squash_only =
        r#"{"allow_squash_merge":true,"allow_merge_commit":false,"allow_rebase_merge":false}"#;
    let answers: Vec<&'static str> = vec![
        whole("200 OK", squash_only).leak(),
        whole("200 OK", r#"{"merged":true}"#).leak(),
        whole("200 OK", "{}").leak(),
        whole(
            "405 Method Not Allowed",
            r#"{"message":"Required status check \"conary/train\" is expected."}"#,
        )
        .leak(),
        whole("200 OK", squash_only).leak(),
        whole("502 Bad Gateway", "{}").leak(),
    ];
    let (base, heads) = github(answers);
    let api = Api::new(&base, Tokens::given("t"));
    assert_eq!(api.land("o/r", 7, "h").unwrap(), PrMerge::Merged);
    assert!(heads.recv().unwrap().starts_with("get /repos/o/r "));
    let merge = heads.recv().unwrap();
    assert!(
        merge.starts_with("put /repos/o/r/pulls/7/merge "),
        "{merge}"
    );
    let body: serde_json::Value =
        serde_json::from_str(merge.split("\r\n\r\n").last().unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"sha": "h", "merge_method": "squash"})
    );
    // A repo whose methods GitHub didn't say gets GitHub's default.
    assert_eq!(
        api.land("o/r", 7, "h").unwrap(),
        PrMerge::Refused("Required status check \"conary/train\" is expected.".into())
    );
    heads.recv().unwrap();
    let merge = heads.recv().unwrap();
    let body: serde_json::Value =
        serde_json::from_str(merge.split("\r\n\r\n").last().unwrap()).unwrap();
    assert_eq!(body, serde_json::json!({"sha": "h"}));
    assert!(api.land("o/r", 7, "h").unwrap_err().unavailable);
}

#[test]
fn the_trains_status_is_its_own_context() {
    use crate::train::{Forge, Mark};
    let answers: Vec<&'static str> = vec![
        whole("201 Created", "{}").leak(),
        whole("201 Created", "{}").leak(),
        whole("201 Created", "{}").leak(),
    ];
    let (base, heads) = github(answers);
    let api = Api::new(&base, Tokens::given("t"));
    let long = "x".repeat(200);
    api.mark("o/r", "h", Mark::Landing, &long, None).unwrap();
    let posted = heads.recv().unwrap();
    assert!(
        posted.starts_with("post /repos/o/r/statuses/h "),
        "{posted}"
    );
    let body: serde_json::Value =
        serde_json::from_str(posted.split("\r\n\r\n").last().unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"state": "success", "context": "conary/train", "description": "x".repeat(140)})
    );
    api.mark("o/r", "h", Mark::Red, "red", Some("https://ci/1"))
        .unwrap();
    let posted = heads.recv().unwrap();
    let body: serde_json::Value =
        serde_json::from_str(posted.split("\r\n\r\n").last().unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"state": "failure", "context": "conary/train", "description": "red", "target_url": "https://ci/1"})
    );
    api.mark("o/r", "h", Mark::Refused, "no", None).unwrap();
    let posted = heads.recv().unwrap();
    let body: serde_json::Value =
        serde_json::from_str(posted.split("\r\n\r\n").last().unwrap()).unwrap();
    assert_eq!(body["state"], "error");
}
