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
    let mut v = json!({"data":{"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base-tip","statusCheckRollup":{"state":"FAILURE"}}},"pullRequest":{
        "headRefOid":"h","baseRefName":"main","title":"t","body":"b","isDraft":false,"mergeStateStatus":"BEHIND","updatedAt":"2026-11-08T12:00:00Z",
        "lastEditedAt":"2026-11-08T11:00:00Z",
        "timelineItems":{"pageInfo":{"hasPreviousPage":false},"nodes":[]},
        "retargets":{"nodes":[{"createdAt":"2026-11-08T11:30:00Z"}]},
        "labels":{"pageInfo":{"hasNextPage":false}},
        "reviewThreads":{"pageInfo":{"hasNextPage":false},"nodes":[
            {"id":"open","isResolved":false,"comments":{"pageInfo":{"hasNextPage":false},"nodes":[
                {"author":{"login":"person"},"body":"a finding","pullRequestReview":{"databaseId":5}},
                {"author":null,"body":"an answer","pullRequestReview":null}]}},
            {"id":"resolved","isResolved":true,"comments":{"pageInfo":{"hasNextPage":false},"nodes":[]}}]},
        "reviews":{"totalCount":2},"comments":{"totalCount":3},
        "commits":{"nodes":[{"commit":{"oid":"h","checkSuites":{"pageInfo":{"hasNextPage":false},"nodes":[{
            "databaseId":11,"createdAt":"2026-11-08T10:05:00Z","status":"IN_PROGRESS","conclusion":null,"app":{"slug":"github-actions"},
            "workflowRun":{"file":{"path":".github/workflows/flake.yml"},"workflow":{"id":"w","name":"flake"}}
        },{
            "databaseId":12,"createdAt":"2026-11-08T10:00:00Z","status":"QUEUED","conclusion":null,"app":{"slug":"other-ci"},
            "workflowRun":null
        }]}}}]}
    }}}});
    // Kept apart: one more field nests the literal past `json!`'s limit.
    v["data"]["repository"]["pullRequest"]["renames"] = json!({"nodes": []});
    v
}
/// A rename is an edit of the claim a review read, though GitHub's
/// `lastEditedAt` dates only the body: the later of the two counts.
#[test]
fn a_title_rename_counts_as_an_edit() {
    let at = |t: &str| t.parse::<jiff::Timestamp>().unwrap().as_second() as u64;
    for (edited, renamed, expected) in [
        (
            json!("2026-11-08T11:00:00Z"),
            json!([{"createdAt":"2026-11-08T11:45:00Z"}]),
            "2026-11-08T11:45:00Z",
        ),
        (
            json!("2026-11-08T11:00:00Z"),
            json!([{"createdAt":"2026-11-08T10:30:00Z"}]),
            "2026-11-08T11:00:00Z",
        ),
        (
            json!(null),
            json!([{"createdAt":"2026-11-08T11:45:00Z"}]),
            "2026-11-08T11:45:00Z",
        ),
    ] {
        let mut v = snapshot();
        let p = &mut v["data"]["repository"]["pullRequest"];
        p["lastEditedAt"] = edited;
        p["renames"]["nodes"] = renamed;
        let (base, _requests) = github(vec![answer(v)]);
        let s = Api::new(&base, Tokens::given("t"))
            .snapshot("o/r", 7)
            .unwrap();
        assert_eq!(s.since.edited, Some(at(expected)), "{expected}");
    }
}
#[test]
fn reads_mergeability_threads_and_suite_identity_without_writes() {
    let (base, requests) = github(vec![answer(snapshot())]);
    let api = Api::new(&base, Tokens::given("test-token"));
    let s = api.snapshot("o/r", 7).unwrap();
    assert_eq!(s.merge_state, "BEHIND");
    let observed = s.default_branch.as_ref().unwrap();
    assert_eq!(observed.name, "main");
    assert_eq!(observed.head, "base-tip");
    assert_eq!(observed.ci, crate::flow::Ci::Red);
    assert_eq!(s.open_threads, 1);
    // Every thread with its comments as they read now: what a judge reads.
    let open = &s.threads["open"];
    assert!(!open.resolved);
    assert_eq!(
        open.comments,
        [
            crate::flow::ThreadComment {
                author: Some("person".into()),
                body: "a finding".into(),
            },
            crate::flow::ThreadComment {
                author: None,
                body: "an answer".into(),
            },
        ]
    );
    assert!(s.threads["resolved"].resolved && s.threads["resolved"].comments.is_empty());
    assert!(s.complete && s.checks_complete);
    assert_eq!(s.suites[&11].status, "in_progress");
    assert_eq!(s.suites[&11].workflow.as_ref().unwrap().id, "w");
    // When the PR last changed without a push, and its push: the earliest
    // check suite on its head, as the reviewer's word is read against them.
    let at = |t: &str| t.parse::<jiff::Timestamp>().unwrap().as_second() as u64;
    assert_eq!(
        s.since,
        crate::flow::Since {
            edited: Some(at("2026-11-08T11:00:00Z")),
            retargeted: Some(at("2026-11-08T11:30:00Z")),
            pushed: Some(at("2026-11-08T10:00:00Z")),
        }
    );
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
    // A thread with more comments than one page is not read whole.
    let mut v = snapshot();
    v["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0]["comments"]["pageInfo"]["hasNextPage"] =
        json!(true);
    let (base, _) = github(vec![answer(v)]);
    assert!(
        !Api::new(&base, Tokens::given("t"))
            .snapshot("o/r", 7)
            .unwrap()
            .complete
    );
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
/// A finding's answers and an ask go out as GitHub's own requests: a PR
/// comment, and a thread's reply and resolution as GraphQL mutations, whose
/// errors refuse.
#[test]
fn publishes_answers_and_comments_in_githubs_own_requests() {
    use crate::flow::Write;
    let body = |request: &str| -> Value {
        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
    };
    let (base, requests) = github(vec![
        answer(json!({"id":2})),
        answer(json!({"data":{}})),
        answer(json!({"data":{}})),
        answer(json!({"errors":[{"message":"not a thread"}]})),
    ]);
    let api = Api::new(&base, Tokens::given("t"));
    api.comment("o/r", 7, "elsewhere").unwrap();
    let request = requests.recv().unwrap();
    assert!(request.starts_with("post /repos/o/r/issues/7/comments "));
    assert_eq!(body(&request), json!({"body":"elsewhere"}));
    api.reply("o/r", 7, "PRRT_1", "an answer").unwrap();
    let request = body(&requests.recv().unwrap());
    assert!(request["query"].as_str().unwrap().contains(
        "addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$thread,body:$body})"
    ));
    assert_eq!(
        request["variables"],
        json!({"thread":"PRRT_1","body":"an answer"})
    );
    api.resolve("o/r", 7, "PRRT_1").unwrap();
    let request = body(&requests.recv().unwrap());
    assert!(
        request["query"]
            .as_str()
            .unwrap()
            .contains("resolveReviewThread(input:{threadId:$thread})")
    );
    assert_eq!(request["variables"], json!({"thread":"PRRT_1"}));
    assert!(api.resolve("o/r", 7, "PRRT_2").is_err());
}
