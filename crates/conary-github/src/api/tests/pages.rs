//! Paged reads against a loopback GitHub: each goes to the end of its data.
use super::*;
use crate::flow::Read;
use serde_json::{Value, json};
use std::io::Read as _;

fn answer(v: Value) -> &'static str {
    let body = v.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .leak()
}

/// The JSON body a request carried.
fn body(request: &str) -> Value {
    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
}

/// A list of more than fifty pages, the most main read before failing it,
/// read to its last, short page.
#[test]
fn a_list_of_more_than_fifty_pages_is_read_to_its_end() {
    let full = || {
        (0..PER_PAGE)
            .map(|n| json!({"number": n}))
            .collect::<Vec<_>>()
    };
    let mut pages: Vec<_> = (0..51).map(|_| answer(json!(full()))).collect();
    pages.push(answer(json!([{"number": 7}])));
    let (base, requests) = github(pages);
    let got = Api::new(&base, Tokens::given("t"))
        .commit_pulls("o/r", "h")
        .unwrap();
    assert_eq!(got.len(), 51 * PER_PAGE + 1);
    let asked: Vec<String> = requests.try_iter().collect();
    assert_eq!(asked.len(), 52);
    assert!(asked[51].starts_with("get /repos/o/r/commits/h/pulls?per_page=100&page=52 "));
}

fn suite(id: usize) -> Value {
    json!({"databaseId": id, "createdAt": "2026-11-08T10:00:00Z", "status": "COMPLETED",
        "conclusion": "SUCCESS", "app": {"slug": "github-actions"}, "workflowRun": null})
}

fn snapshot(suites: Vec<Value>, cursor: &str) -> Value {
    let mut v = json!({"data":{"repository":{"defaultBranchRef":null,"pullRequest":{
        "headRefOid":"h","baseRefName":"main","title":"t","body":"b","isDraft":false,"mergeStateStatus":"CLEAN","updatedAt":"2026-11-08T12:00:00Z",
        "lastEditedAt":null,
        "timelineItems":{"pageInfo":{"hasPreviousPage":false},"nodes":[]},
        "retargets":{"nodes":[]},
        "labels":{"pageInfo":{"hasNextPage":false}},
        "reviewThreads":{"pageInfo":{"hasNextPage":false},"nodes":[]},
        "reviews":{"totalCount":0},"comments":{"totalCount":0},
        "commits":{"nodes":[{"commit":{"oid":"h","checkSuites":{
            "pageInfo":{"hasNextPage":true,"endCursor":cursor},"nodes":suites}}}]}
    }}}});
    v["data"]["repository"]["pullRequest"]["renames"] = json!({"nodes": []});
    v
}

/// More check suites than one query holds: the rest are read a page at a
/// time, on the head the first page was of, to the last.
#[test]
fn a_heads_check_suites_are_read_past_the_first_page() {
    let first = snapshot((0..PER_PAGE).map(suite).collect(), "c1");
    let rest = json!({"data":{"repository":{"object":{"checkSuites":{
        "pageInfo":{"hasNextPage":false,"endCursor":"c2"},
        "nodes":(PER_PAGE..PER_PAGE + 50).map(suite).collect::<Vec<_>>()}}}}});
    let (base, requests) = github(vec![answer(first), answer(rest)]);
    let s = Api::new(&base, Tokens::given("t"))
        .snapshot("o/r", 7)
        .unwrap();
    assert_eq!(s.suites.len(), PER_PAGE + 50);
    let _ = requests.recv().unwrap();
    let second = body(&requests.recv().unwrap());
    assert_eq!(
        second["variables"],
        json!({"owner":"o","name":"r","head":"h","after":"c1"})
    );
}

/// A paged read past its own deadline fails, as GitHub unavailable, and
/// asks for no page past it.
#[test]
fn a_paged_read_past_its_deadline_fails() {
    let within = std::time::Duration::from_millis(50);
    let mut asked = 0;
    let err = paged("a list", within, |_, left| {
        asked += 1;
        assert!(left <= within);
        thread::sleep(within + std::time::Duration::from_millis(10));
        Ok(Page {
            items: vec![()],
            more: true,
        })
    })
    .unwrap_err();
    assert!(err.unavailable, "{err}");
    assert_eq!(asked, 1);
}

/// A page that stalls is cut off at what's left of its read's deadline, not
/// at a call's own minute.
#[test]
fn a_page_that_stalls_is_cut_off_at_its_reads_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let full = answer(json!(
        (0..PER_PAGE)
            .map(|n| json!({"number": n}))
            .collect::<Vec<_>>()
    ));
    thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut head = [0; 1024];
        _ = conn.read(&mut head);
        conn.write_all(full.as_bytes()).unwrap();
        // The second page: asked, and never answered.
        let (mut conn, _) = listener.accept().unwrap();
        _ = conn.read(&mut head);
        thread::sleep(std::time::Duration::from_secs(30));
    });
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut api = Api::new(&base, Tokens::given("t"));
        api.read_for = std::time::Duration::from_millis(500);
        _ = tx.send(api.commit_pulls("o/r", "h"));
    });
    let got = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the read is cut off at its deadline");
    assert!(got.unwrap_err().unavailable);
}

/// Check suites' pages that say there's more but don't move on, with no
/// cursor or the one before again, fail the read rather than read the same
/// page until the deadline.
#[test]
fn check_suites_pages_that_do_not_advance_fail_the_read() {
    let stuck = |cursor: Value| {
        json!({"data":{"repository":{"object":{"checkSuites":{
            "pageInfo":{"hasNextPage":true,"endCursor":cursor},"nodes":[suite(PER_PAGE)]}}}}})
    };
    for second in [stuck(json!(null)), stuck(json!("c1"))] {
        let first = snapshot((0..PER_PAGE).map(suite).collect(), "c1");
        let (base, _) = github(vec![answer(first), answer(second)]);
        let err = Api::new(&base, Tokens::given("t"))
            .snapshot("o/r", 7)
            .unwrap_err();
        assert!(err.to_string().contains("did not advance"), "{err}");
    }
}
