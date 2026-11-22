//! Paged reads against a loopback GitHub: each goes to the end of its data.
use super::*;
use crate::flow::Read;
use serde_json::{Value, json};

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
