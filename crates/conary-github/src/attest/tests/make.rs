use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

use conary_core::op::{Outcome, Repo};
use serde_json::json;

use crate::api::CheckRun;
use crate::attest::make::*;
use crate::attest::testing::{claims, key, signer, token as signed};
use crate::attest::{Subject, check};

const COMMIT: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
const TREE: &str = "7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e";
const BASE: &str = "baba000000000000000000000000000000000000";
const HEAD: &str = "eded000000000000000000000000000000000000";
const RUN: u64 = 41;

fn repo() -> Repo {
    "FieldmouseWorks/remi".parse().unwrap()
}

fn run(conclusion: &str) -> CheckRun {
    CheckRun {
        id: RUN,
        name: "attest".into(),
        status: "completed".into(),
        conclusion: Some(conclusion.into()),
        html_url: None,
        started_at: None,
        check_suite: None,
        output: None,
    }
}

fn log() -> Log {
    Log::of(b"every check passed\n", "gate-log-1")
}

/// The file a job makes for `claim`, signed for as GitHub would sign its
/// token, checked as the verifier checks one.
fn made_and_checked(claim: &Claim, subject: Subject, conclusion: &str) -> Result<Outcome, String> {
    let s = statement(claim)?.text();
    let mut c = claims("FieldmouseWorks/remi", COMMIT, RUN, &s);
    assert_eq!(c["aud"], json!(audience(&s)));
    c["event_name"] = json!("pull_request");
    let made = file(&s, &signed(&c));
    check(
        made.as_bytes(),
        &[key()],
        &repo(),
        subject,
        &run(conclusion),
        &signer(),
    )
    .map(|claimed| claimed.attested.result)
}

#[test]
fn what_a_job_makes_is_what_the_verifier_reads() {
    let pushed = Claim::Commit {
        commit: COMMIT.into(),
        result: Gate::Pass,
        tree: TREE.into(),
        skipped: Vec::new(),
        log: log(),
    };
    let push = Subject::Commit {
        commit: COMMIT,
        tree: TREE,
    };
    assert_eq!(
        made_and_checked(&pushed, push, "success"),
        Ok(Outcome::Pass)
    );
    // Its tree is the one asked about, or it's no pass of that commit's.
    let elsewhere = Subject::Commit {
        commit: COMMIT,
        tree: BASE,
    };
    assert!(
        made_and_checked(&pushed, elsewhere, "success")
            .unwrap_err()
            .contains("ran on tree")
    );
    let commit = |result, skipped: &[&str]| Claim::Commit {
        commit: COMMIT.into(),
        result,
        tree: TREE.into(),
        skipped: skipped.iter().map(|s| (*s).to_owned()).collect(),
        log: log(),
    };
    let failed = commit(Gate::Fail, &[]);
    assert!(
        made_and_checked(&failed, push, "failure")
            .unwrap_err()
            .contains("says fail")
    );
    let skipping = commit(Gate::Pass, &["vm"]);
    assert!(
        made_and_checked(&skipping, push, "success")
            .unwrap_err()
            .contains("skipped vm")
    );

    let pr = Subject::Negative {
        base: BASE,
        head: HEAD,
    };
    let negative = |result, tree: Option<&str>, tests: &[&str]| Claim::Negative {
        base: BASE.into(),
        head: HEAD.into(),
        result,
        tree: tree.map(Into::into),
        tests: tests.iter().map(|t| (*t).to_owned()).collect(),
        checks: if tests.is_empty() {
            Vec::new()
        } else {
            vec!["crates".into()]
        },
        skipped: Vec::new(),
        unrun: Vec::new(),
        log: log(),
    };
    let failed = negative(Negative::Fail, Some(TREE), &["tests/new.rs"]);
    assert_eq!(made_and_checked(&failed, pr, "success"), Ok(Outcome::Fail));
    let none = negative(Negative::None, None, &[]);
    assert_eq!(made_and_checked(&none, pr, "success"), Ok(Outcome::None));
    let passed = negative(Negative::Pass, Some(TREE), &["tests/new.rs"]);
    assert_eq!(made_and_checked(&passed, pr, "failure"), Ok(Outcome::Pass));
    // Never a push's gate, and a negative is never one.
    let push = Subject::Commit {
        commit: HEAD,
        tree: TREE,
    };
    assert!(made_and_checked(&failed, push, "success").is_err());
}

#[test]
fn a_statement_names_object_ids_and_a_negatives_tree_as_its_result_has_one() {
    let made = statement(&Claim::Revision {
        base: BASE.into(),
        head: HEAD.into(),
        result: Gate::Pass,
        tree: TREE.into(),
        skipped: vec!["agents".into()],
        log: log(),
    })
    .unwrap();
    assert_eq!(
        (made.name, made.predicate_type),
        ("conary-revision", GATE_TYPE)
    );
    let s = made.text();
    let (subject, predicate) = s.split_at(s.find('{').unwrap());
    assert_eq!(
        (subject, &predicate[..predicate.len() - 1]),
        (&made.subject[..], &made.predicate[..])
    );
    assert_eq!(subject, format!("conary revision v1\n{BASE}\n{HEAD}\n"));
    assert!(predicate.ends_with("}\n") && predicate.lines().count() == 1);
    let predicate: serde_json::Value = serde_json::from_str(predicate).unwrap();
    assert_eq!(
        predicate,
        json!({"result": "pass", "revision": {"base": BASE, "head": HEAD}, "tree": TREE,
               "skipped": ["agents"], "log": {"sha256": log().sha256, "artifact": "gate-log-1"}})
    );
    assert_eq!(log().sha256.len(), 64);

    let commit = |commit: &str, tree: &str| Claim::Commit {
        commit: commit.into(),
        result: Gate::Pass,
        tree: tree.into(),
        skipped: Vec::new(),
        log: log(),
    };
    for (c, t) in [
        (&COMMIT.to_uppercase()[..], TREE),
        (COMMIT, "main"),
        ("", TREE),
    ] {
        let why = statement(&commit(c, t)).unwrap_err();
        assert!(why.contains("isn't a lowercase object id"), "{why}");
    }
    let negative = |result, tree: Option<&str>, tests: Vec<String>| Claim::Negative {
        base: BASE.into(),
        head: HEAD.into(),
        result,
        tree: tree.map(Into::into),
        tests,
        checks: Vec::new(),
        skipped: Vec::new(),
        unrun: Vec::new(),
        log: log(),
    };
    assert!(statement(&negative(Negative::None, Some(TREE), Vec::new())).is_err());
    assert!(statement(&negative(Negative::None, None, vec!["tests/a.rs".into()])).is_err());
    assert!(statement(&negative(Negative::Fail, None, Vec::new())).is_err());
    assert!(statement(&negative(Negative::Skipped, Some("x"), Vec::new())).is_err());
    let none = statement(&negative(Negative::None, None, Vec::new())).unwrap();
    assert_eq!(
        (none.name, none.predicate_type),
        ("conary-negative", NEGATIVE_TYPE)
    );
    let none = none.predicate;
    assert!(
        none.contains(r#""result":"none""#) && none.contains(r#""tree":null"#),
        "{none}"
    );
    // The results a job names them by, and nothing else.
    for (word, gate) in [
        ("pass", Some(Gate::Pass)),
        ("fail", Some(Gate::Fail)),
        ("Pass", None),
        ("none", None),
    ] {
        assert_eq!(word.parse::<Gate>().ok(), gate, "{word}");
    }
    for (word, result) in [
        ("fail", Some(Negative::Fail)),
        ("pass", Some(Negative::Pass)),
        ("skipped", Some(Negative::Skipped)),
        ("none", Some(Negative::None)),
        ("", None),
    ] {
        assert_eq!(word.parse::<Negative>().ok(), result, "{word}");
    }
}

/// A token service on localhost answering each request with the next of
/// `answers` (status, body, and how many bytes past it its length claims,
/// for an answer cut off as it's sent), and handing back each request's line and
/// Authorization header.
fn service(
    answers: Vec<(u16, &'static str, usize)>,
) -> (String, std::thread::JoinHandle<Vec<(String, String)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/token?api-version=2.0",
        listener.local_addr().unwrap()
    );
    let served = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, body, cut) in answers {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut authorization = String::new();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                if let Some(v) = header
                    .strip_prefix("authorization: ")
                    .or(header.strip_prefix("Authorization: "))
                {
                    authorization = v.trim().to_owned();
                }
            }
            seen.push((line.trim().to_owned(), authorization));
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len() + cut
            )
            .unwrap();
        }
        seen
    });
    (url, served)
}

#[test]
fn a_token_is_asked_for_with_its_statements_audience_and_again_when_github_says_later() {
    let s = "conary commit v1\nc0\n{}\n";
    let (url, served) = service(vec![
        (503, "{}", 0),
        (200, r#"{"value":"#, 20),
        (200, r#"{"value":"a.b.c"}"#, 0),
    ]);
    let request = Request {
        url: &url,
        bearer: "request-token",
    };
    let got = token_waiting(&request, s, Duration::from_millis(1)).unwrap();
    assert_eq!(got, "a.b.c");
    let seen = served.join().unwrap();
    // Cut off mid-answer is the network, not GitHub's word: asked again.
    assert_eq!(seen.len(), 3);
    for (line, authorization) in &seen {
        assert!(
            line.starts_with(&format!(
                "GET /token?api-version=2.0&audience={} ",
                audience(s)
            )),
            "{line}"
        );
        assert_eq!(authorization, "bearer request-token");
    }
    // Refused is refused: asked once, and the bearer isn't in what it says.
    let (url, served) = service(vec![(403, "{}", 0)]);
    let request = Request {
        url: &url,
        bearer: "request-token",
    };
    let e = token_waiting(&request, s, Duration::from_millis(1)).unwrap_err();
    assert!(!e.unavailable && e.to_string().contains("HTTP 403"), "{e}");
    assert!(!e.to_string().contains("request-token"), "{e}");
    assert_eq!(served.join().unwrap().len(), 1);
    // A whole answer that isn't a token is as wrong the next time.
    let (url, served) = service(vec![(200, "not json", 0)]);
    let request = Request {
        url: &url,
        bearer: "request-token",
    };
    let e = token_waiting(&request, s, Duration::from_millis(1)).unwrap_err();
    assert!(
        !e.unavailable && e.to_string().contains("doesn't read"),
        "{e}"
    );
    assert_eq!(served.join().unwrap().len(), 1);
}
