//! The flow's supplementary transport reads, against a loopback GitHub.
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
fn snapshot() -> Value {
    json!({"data":{"repository":{"pullRequest":{
        "headRefOid":"h","baseRefName":"main","title":"t","body":"b","isDraft":false,"mergeStateStatus":"BEHIND","updatedAt":"2026-11-08T12:00:00Z",
        "timelineItems":{"pageInfo":{"hasPreviousPage":false},"nodes":[]},
        "labels":{"pageInfo":{"hasNextPage":false}},
        "reviewThreads":{"pageInfo":{"hasNextPage":false},"nodes":[{"id":"open","isResolved":false},{"id":"resolved","isResolved":true}]},
        "reviews":{"totalCount":2},"comments":{"totalCount":3},
        "commits":{"nodes":[{"commit":{"oid":"h","checkSuites":{"pageInfo":{"hasNextPage":false},"nodes":[{
            "databaseId":11,"status":"IN_PROGRESS","conclusion":null,"app":{"slug":"github-actions"},
            "workflowRun":{"file":{"path":".github/workflows/flake.yml"},"workflow":{"id":"w","name":"flake"}}
        }]}}}]}
    }}}})
}
#[test]
fn reads_mergeability_threads_and_suite_identity_without_writes() {
    let (base, requests) = github(vec![answer(snapshot())]);
    let api = Api::new(&base, Tokens::given("test-token"));
    let s = api.snapshot("o/r", 7).unwrap();
    assert_eq!(s.merge_state, "BEHIND");
    assert_eq!(s.open_threads, 1);
    assert!(s.complete && s.checks_complete);
    assert_eq!(s.suites[&11].status, "in_progress");
    assert_eq!(s.suites[&11].workflow.as_ref().unwrap().id, "w");
    let request = requests.recv().unwrap();
    assert!(request.starts_with("post /graphql "));
    let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert!(body["query"].as_str().unwrap().starts_with("query("));
    assert_eq!(
        body["variables"],
        json!({"owner":"o","name":"r","number":7})
    );
}
#[test]
fn rejects_graphql_errors_and_partial_parent_data() {
    for response in [
        json!({"data":null,"errors":[{"message":"refused"}]}),
        json!({"data":{"repository":null}}),
    ] {
        let (base, _) = github(vec![answer(response)]);
        assert!(
            Api::new(&base, Tokens::given("t"))
                .snapshot("o/r", 7)
                .is_err()
        );
    }
    let (base, _) = github(vec![answer(json!({"errors":[{"type":"RATE_LIMITED"}]}))]);
    assert!(
        Api::new(&base, Tokens::given("t"))
            .snapshot("o/r", 7)
            .unwrap_err()
            .unavailable
    );
}
#[test]
fn query_caps_labels_threads_and_comments_but_not_workflow_results() {
    for field in ["labels", "reviewThreads", "reviews", "comments"] {
        let mut v = snapshot();
        let p = &mut v["data"]["repository"]["pullRequest"];
        if matches!(field, "reviews" | "comments") {
            p[field]["totalCount"] = json!(101);
        } else {
            p[field]["pageInfo"]["hasNextPage"] = json!(true);
        }
        let (base, _) = github(vec![answer(v)]);
        assert!(
            !Api::new(&base, Tokens::given("t"))
                .snapshot("o/r", 7)
                .unwrap()
                .complete
        );
    }
    let mut v = snapshot();
    v["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["checkSuites"]["pageInfo"]
        ["hasNextPage"] = json!(true);
    let (base, _) = github(vec![answer(v)]);
    let s = Api::new(&base, Tokens::given("t"))
        .snapshot("o/r", 7)
        .unwrap();
    assert!(s.complete);
    assert!(!s.checks_complete);
}
fn commit(sha: &str) -> Value {
    json!({"sha":sha,"parents":[],"committer":null,"commit":{"message":"change: code","author":{"email":"a"},"committer":{"email":"a"},"verification":null}})
}
fn page(commits: Vec<Value>, total: usize, fork: &str) -> Value {
    json!({"merge_base_commit":{"sha":fork},"total_commits":total,"commits":commits})
}
#[test]
fn reads_comparison_commits_past_250_and_verifies_each_page() {
    let pages = (0..3)
        .map(|p| {
            let end = if p == 2 { 251 } else { (p + 1) * 100 };
            answer(page(
                (p * 100..end).map(|n| commit(&n.to_string())).collect(),
                251,
                "fork",
            ))
        })
        .collect();
    let (base, requests) = github(pages);
    let api = Api::new(&base, Tokens::given("t"));
    let h = api.compare_commits("o/r", "base", "250").unwrap();
    assert_eq!(h.commits.len(), 251);
    assert!(h.complete);
    for n in 1..=3 {
        assert!(requests.recv().unwrap().starts_with(&format!(
            "get /repos/o/r/compare/base...250?per_page=100&page={n} "
        )));
    }
    for wrong in [
        page(vec![commit("head")], 101, "other"),
        page(vec![commit("head")], 102, "fork"),
    ] {
        let first = page(
            (0..100).map(|n| commit(&n.to_string())).collect(),
            101,
            "fork",
        );
        let (base, _) = github(vec![answer(first), answer(wrong)]);
        assert!(
            Api::new(&base, Tokens::given("t"))
                .compare_commits("o/r", "base", "head")
                .is_err()
        );
    }
}
#[test]
fn permanent_short_comparisons_are_distinct_from_unreadable_ranges() {
    let (base, _) = github(vec![answer(page(vec![commit("a")], 2, "fork"))]);
    let h = Api::new(&base, Tokens::given("t"))
        .compare_commits("o/r", "base", "head")
        .unwrap();
    assert!(!h.complete);
    for p in [page(vec![commit("a")], 1, "fork"), page(vec![], 0, "fork")] {
        let (base, _) = github(vec![answer(p)]);
        assert!(
            Api::new(&base, Tokens::given("t"))
                .compare_commits("o/r", "base", "head")
                .is_err()
        );
    }
}
#[test]
fn timeout_annotation_reader_reads_past_the_first_page() {
    let mut first = vec![json!({"message":"warning"}); 100];
    first[99] = json!({"message":"another warning"});
    let (base, requests) = github(vec![
        answer(json!(first)),
        answer(json!([{"message":"has exceeded the maximum execution time"}])),
    ]);
    let notes = Api::new(&base, Tokens::given("t"))
        .annotations("o/r", 1)
        .unwrap();
    assert_eq!(notes.len(), 101);
    assert!(notes[100].contains("maximum execution time"));
    assert!(requests.recv().unwrap().contains("page=1"));
    assert!(requests.recv().unwrap().contains("page=2"));
}
