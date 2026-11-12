//! `cn attest`, run as a CI job runs it.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

use conary_github::attest::make::{Log, audience};
use serde_json::{Value, json};

const COMMIT: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
const TREE: &str = "7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e";
const BASE: &str = "baba000000000000000000000000000000000000";
const HEAD: &str = "eded000000000000000000000000000000000000";
const BEARER: &str = "request-token";

/// `cn attest ARGS`, asking for its token at `url` when one is given, and
/// with no token service at all otherwise.
fn attest(args: &[&str], url: Option<&str>) -> Output {
    let mut cn = Command::new(env!("CARGO_BIN_EXE_cn"));
    cn.arg("attest").args(args);
    cn.env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN");
    if let Some(url) = url {
        cn.env("ACTIONS_ID_TOKEN_REQUEST_URL", url)
            .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", BEARER);
    }
    cn.output().unwrap()
}

/// A token service on localhost answering one request with `status` and
/// `body`, handing back the request's line and Authorization header.
fn service(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/token?api-version=2.0",
        listener.local_addr().unwrap()
    );
    let served = std::thread::spawn(move || {
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
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("authorization")
            {
                authorization = value.trim().to_owned();
            }
        }
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        (line, authorization)
    });
    (url, served)
}

/// A token service answering its requests in turn with `answers`.
fn service_seq(
    answers: Vec<(u16, &'static str)>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    service_seq_with(answers, |_| {})
}

fn service_seq_with(
    answers: Vec<(u16, &'static str)>,
    mut before_answer: impl FnMut(usize) + Send + 'static,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/token?api-version=2.0",
        listener.local_addr().unwrap()
    );
    let served = std::thread::spawn(move || {
        let mut lines = Vec::new();
        for (index, (status, body)) in answers.into_iter().enumerate() {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
            }
            lines.push(line);
            before_answer(index);
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        lines
    });
    (url, served)
}

fn read(path: &Value) -> String {
    std::fs::read_to_string(path.as_str().unwrap()).unwrap()
}

// What a job keeps is the statement the token signs: the subject file, then
// predicate.json's one line, and the signed file is that and the token.
#[test]
fn attest_writes_the_statement_a_token_signs_and_the_token_last() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("flake.log");
    std::fs::write(&log, "every check passed\n").unwrap();
    let out = dir.path().join("out");
    let (log, out) = (log.to_str().unwrap(), out.to_str().unwrap());
    let (url, served) = service(200, r#"{"value":"a.b.c"}"#);
    let run = attest(
        &[
            "revision",
            BASE,
            HEAD,
            "--result",
            "pass",
            "--tree",
            TREE,
            "--skipped",
            "agents",
            "--log",
            log,
            "--log-artifact",
            "flake-log-1",
            "--out",
            out,
            "--sign",
        ],
        Some(&url),
    );
    assert!(run.status.success(), "{run:?}");
    let (line, authorization) = served.join().unwrap();
    let made: Value = serde_json::from_slice(&run.stdout).unwrap();
    let statement = format!("{}{}", read(&made["subject"]), read(&made["predicate"]));
    assert!(
        made["subject"]
            .as_str()
            .unwrap()
            .ends_with("/conary-revision")
    );
    assert_eq!(
        made["predicate_type"],
        json!("https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#v1")
    );
    assert_eq!(
        read(&made["subject"]),
        format!("conary revision v1\n{BASE}\n{HEAD}\n")
    );
    let predicate: Value = serde_json::from_str(&read(&made["predicate"])).unwrap();
    assert_eq!(predicate["skipped"], json!(["agents"]));
    assert_eq!(predicate["tree"], json!(TREE));
    assert_eq!(predicate["log"]["artifact"], json!("flake-log-1"));
    assert_eq!(read(&made["predicate"]).lines().count(), 1);

    let audience = audience(&statement);
    assert_eq!(made["audience"], json!(audience));
    assert!(line.contains(&format!("&audience={audience} ")), "{line}");
    assert_eq!(authorization, format!("bearer {BEARER}"));
    assert!(
        made["attestation"]
            .as_str()
            .unwrap()
            .ends_with("/conary-attestation")
    );
    assert_eq!(read(&made["attestation"]), format!("{statement}a.b.c\n"));
    let shown = String::from_utf8_lossy(&run.stdout) + String::from_utf8_lossy(&run.stderr);
    assert!(!shown.contains(BEARER), "{shown}");

    // Unsigned, as a public repo's job writes it for its bundle.
    let unsigned = dir.path().join("unsigned");
    let unsigned = unsigned.to_str().unwrap();
    let run = attest(
        &[
            "commit",
            COMMIT,
            "--result",
            "fail",
            "--tree",
            TREE,
            "--log",
            log,
            "--log-artifact",
            "flake-log-2",
            "--out",
            unsigned,
        ],
        None,
    );
    assert!(run.status.success(), "{run:?}");
    let made: Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(
        read(&made["subject"]),
        format!("conary commit v1\n{COMMIT}\n")
    );
    assert!(made["attestation"].is_null() && made["audience"].is_null());
    assert!(!Path::new(unsigned).join("conary-attestation").exists());
}

// A token GitHub refuses, or one the job can't ask for, leaves nothing
// behind for a later step to upload, and never shows the bearer.
#[test]
fn attest_writes_nothing_when_it_cant_sign() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("negative.log");
    std::fs::write(&log, "").unwrap();
    let out = dir.path().join("out");
    let args = [
        "negative",
        BASE,
        HEAD,
        "--result",
        "fail",
        "--tree",
        TREE,
        "--test",
        "tests/new.rs",
        "--check",
        "crates",
        "--log",
        log.to_str().unwrap(),
        "--log-artifact",
        "negative-log-1",
        "--out",
        out.to_str().unwrap(),
        "--sign",
    ];
    let (url, served) = service(403, "{}");
    let run = attest(&args, Some(&url));
    served.join().unwrap();
    let said = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success() && said.contains("HTTP 403"), "{said}");
    assert!(!said.contains(BEARER), "{said}");
    assert!(!out.exists());

    let run = attest(&args, None);
    let said = String::from_utf8_lossy(&run.stderr);
    assert!(
        !run.status.success() && said.contains("id-token: write"),
        "{said}"
    );
    assert!(!out.exists());
}

// What the format refuses, cn refuses: results it doesn't name, ids that
// aren't object ids, a negative's tree that doesn't fit its result.
#[test]
fn attest_refuses_what_the_format_has_no_place_for() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    std::fs::write(&log, "").unwrap();
    let out = dir.path().join("out");
    let (log, out) = (log.to_str().unwrap(), out.to_str().unwrap());
    let with = |args: &[&'static str]| -> Vec<&str> {
        [
            args,
            &["--log", log, "--log-artifact", "l", "--out", out][..],
        ]
        .concat()
    };
    for args in [
        with(&["commit", COMMIT, "--result", "passed", "--tree", TREE]),
        with(&["commit", COMMIT, "--result", "pass"]),
        with(&["negative", BASE, HEAD, "--result", "maybe"]),
        vec!["commit", COMMIT, "--result", "pass", "--tree", TREE],
        with(&["commit", "HEAD", "--result", "pass", "--tree", TREE]),
        with(&["negative", BASE, HEAD, "--result", "fail"]),
        with(&["negative", BASE, HEAD, "--result", "none", "--tree", TREE]),
    ] {
        let run = attest(&args, None);
        assert!(!run.status.success(), "{args:?}");
    }
    assert!(!Path::new(out).exists());
}

// Burrow's statements, as its workflow wrote them before cn made them: the
// subject, then the predicate in jq's key order on one line. cn must write
// the same bytes for the same inputs.
const FAIL_COMMIT_STATEMENT: &str = concat!(
    "conary commit v1\nc0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0\n",
    r#"{"result":"fail","revision":null,"tree":"7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e","skipped":[],"log":{"sha256":"642efd8bb55acff8e8b531a81b63cbc9d2b62acb652ec106891dca6ef1959a07","artifact":"flake-log-1"}}"#,
    "\n"
);

#[test]
fn a_failing_commit_claim_is_burrows_statement_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("flake.log");
    std::fs::write(&log, "every check passed\n").unwrap();
    let out = dir.path().join("out");
    let run = attest(
        &[
            "commit",
            COMMIT,
            "--result",
            "fail",
            "--tree",
            TREE,
            "--log",
            log.to_str().unwrap(),
            "--log-artifact",
            "flake-log-1",
            "--out",
            out.to_str().unwrap(),
        ],
        None,
    );
    assert!(run.status.success(), "{run:?}");
    let made: Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(
        format!("{}{}", read(&made["subject"]), read(&made["predicate"])),
        FAIL_COMMIT_STATEMENT
    );
}

// A gate split across legs is attested from each leg's log: cn puts them
// together as burrow's gate job did (`leg K of N:` and then the leg's log),
// names that in the predicate and keeps it for the job to upload.
#[test]
fn a_combined_leg_log_is_what_the_predicate_names_and_what_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (one, two) = (dir.path().join("one"), dir.path().join("two"));
    std::fs::write(&one, "built a\n").unwrap();
    std::fs::write(&two, "built b\n").unwrap();
    let out = dir.path().join("out");
    let run = attest(
        &[
            "revision",
            BASE,
            HEAD,
            "--result",
            "pass",
            "--tree",
            TREE,
            "--skipped",
            "agents",
            "--leg-log",
            one.to_str().unwrap(),
            "--leg-log",
            two.to_str().unwrap(),
            "--log-artifact",
            "flake-log-1",
            "--out",
            out.to_str().unwrap(),
        ],
        None,
    );
    assert!(run.status.success(), "{run:?}");
    let made: Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(
        read(&made["log"]),
        "leg 1 of 2:\nbuilt a\nleg 2 of 2:\nbuilt b\n"
    );
    assert_eq!(
        format!("{}{}", read(&made["subject"]), read(&made["predicate"])),
        format!(
            "conary revision v1\n{BASE}\n{HEAD}\n{}\n",
            format_args!(
                r#"{{"result":"pass","revision":{{"base":"{BASE}","head":"{HEAD}"}},"tree":"{TREE}","skipped":["agents"],"log":{{"sha256":"40cebbe8158ea4c343a603a9121053d03fdd69f711d73f43345809ec04310a0a","artifact":"flake-log-1"}}}}"#
            )
        )
    );
    // One log or the legs', not both.
    let run = attest(
        &[
            "commit",
            COMMIT,
            "--result",
            "pass",
            "--tree",
            TREE,
            "--log",
            one.to_str().unwrap(),
            "--leg-log",
            two.to_str().unwrap(),
            "--log-artifact",
            "l",
            "--out",
            dir.path().join("o2").to_str().unwrap(),
        ],
        None,
    );
    assert!(!run.status.success());
}

fn negative_from(found: &str, extra: &[&str]) -> (Output, Option<Value>, tempfile::TempDir) {
    negative_from_at(found, extra, None)
}

fn negative_from_at(
    found: &str,
    extra: &[&str],
    url: Option<&str>,
) -> (Output, Option<Value>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("negative.json");
    std::fs::write(&file, found).unwrap();
    let log = dir.path().join("negative.log");
    std::fs::write(&log, "built t: it failed\n").unwrap();
    let out = dir.path().join("out");
    let mut args = vec![
        "negative",
        BASE,
        HEAD,
        "--from",
        file.to_str().unwrap(),
        "--log",
        log.to_str().unwrap(),
        "--log-artifact",
        "negative-log-1",
        "--out",
        out.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    let run = attest(&args, url);
    let made = serde_json::from_slice(&run.stdout).ok();
    (run, made, dir)
}

// burrow's negative.json is taken as it is, in the key order burrow's jq
// gave the predicate, with the revision from the arguments.
#[test]
fn negative_json_is_the_statement_burrow_made_of_it() {
    let (run, made, _dir) = negative_from(
        &format!(
            r#"{{"result":"fail","tree":"{TREE}","tests":["tests/a.txt"],"checks":["t"],"skipped":[],"unrun":["tests/b.txt"]}}"#
        ),
        &[],
    );
    assert!(run.status.success(), "{run:?}");
    let made = made.unwrap();
    assert_eq!(
        format!("{}{}", read(&made["subject"]), read(&made["predicate"])),
        format!(
            "conary negative v1\n{BASE}\n{HEAD}\n{}\n",
            format_args!(
                r#"{{"result":"fail","revision":{{"base":"{BASE}","head":"{HEAD}"}},"tree":"{TREE}","tests":["tests/a.txt"],"checks":["t"],"skipped":[],"unrun":["tests/b.txt"],"log":{{"sha256":"9a1e0801bbb8cb2932ef91ed59175c946e9197d3f21b6846f1ca7e03db1cef30","artifact":"negative-log-1"}}}}"#
            )
        )
    );
    assert_eq!(
        made["predicate_type"],
        json!(
            "https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#negative-v1"
        )
    );

    // A change that needs no negative has a null tree and empty lists.
    let (run, made, _dir) = negative_from(
        r#"{"result":"none","tree":null,"tests":[],"checks":[],"skipped":[],"unrun":[]}"#,
        &[],
    );
    assert!(run.status.success(), "{run:?}");
    assert!(read(&made.unwrap()["predicate"]).contains(r#""tree":null,"tests":[]"#));
}

// What the format has no place for in negative.json is refused, and nothing
// is written.
#[test]
fn negative_json_is_refused_when_it_doesnt_fit_the_format() {
    // The same shape that fits is taken, so the refusals below are the
    // format's and not a bad command line.
    let (run, _, _dir) = negative_from(
        r#"{"result":"none","tree":null,"tests":[],"checks":[],"skipped":[],"unrun":[]}"#,
        &[],
    );
    assert!(run.status.success(), "{run:?}");
    for found in [
        // An invented key.
        r#"{"result":"fail","tree":"7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e","tests":[],"checks":[],"skipped":[],"unrun":[],"extra":1}"#,
        // A result the format doesn't name.
        r#"{"result":"maybe","tree":null,"tests":[],"checks":[],"skipped":[],"unrun":[]}"#,
        // A tree that isn't an object id.
        r#"{"result":"fail","tree":"abc","tests":[],"checks":[],"skipped":[],"unrun":[]}"#,
        // None with a tree.
        r#"{"result":"none","tree":"7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e","tests":[],"checks":[],"skipped":[],"unrun":[]}"#,
        // A list missing.
        r#"{"result":"none","tree":null,"tests":[],"checks":[],"skipped":[]}"#,
        "not json",
    ] {
        let (run, _, dir) = negative_from(found, &[]);
        assert!(!run.status.success(), "{found}");
        assert!(!dir.path().join("out").exists(), "{found}");
    }
    // Flags beside the file would be two sources for one fact.
    let (run, _, _dir) = negative_from(
        r#"{"result":"none","tree":null,"tests":[],"checks":[],"skipped":[],"unrun":[]}"#,
        &["--result", "fail"],
    );
    assert!(!run.status.success());
}

// GitHub's token service answers 503 now and then: cn asks again, and signs
// the same statement the unsigned run writes.
#[test]
fn signing_asks_again_when_the_token_service_says_try_later() {
    let (url, served) = service_seq(vec![
        (503, "{}"),
        (503, "{}"),
        (200, r#"{"value":"a.b.c"}"#),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("n.json");
    std::fs::write(
        &file,
        format!(
            r#"{{"result":"fail","tree":"{TREE}","tests":["t"],"checks":["c"],"skipped":[],"unrun":[]}}"#
        ),
    )
    .unwrap();
    let log = dir.path().join("l");
    std::fs::write(&log, "x").unwrap();
    let out = dir.path().join("out");
    let signed = attest(
        &[
            "negative",
            BASE,
            HEAD,
            "--from",
            file.to_str().unwrap(),
            "--log",
            log.to_str().unwrap(),
            "--log-artifact",
            "negative-log-1",
            "--out",
            out.to_str().unwrap(),
            "--sign",
        ],
        Some(&url),
    );
    assert!(signed.status.success(), "{signed:?}");
    assert_eq!(served.join().unwrap().len(), 3);
    let signed: Value = serde_json::from_slice(&signed.stdout).unwrap();
    let statement = format!("{}{}", read(&signed["subject"]), read(&signed["predicate"]));
    assert_eq!(read(&signed["attestation"]), format!("{statement}a.b.c\n"));
    assert_eq!(signed["audience"], json!(audience(&statement)));
}

// The token server changes a source only after the statement has been made.
// Both the initial request and a retry must retain that original snapshot.
#[test]
fn signing_keeps_the_log_snapshot_across_source_changes_and_retries() {
    for command in ["commit", "revision", "negative"] {
        for change in ["append", "replace", "remove"] {
            for retry in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let one = dir.path().join("one");
                let two = dir.path().join("two");
                std::fs::write(&one, b"first\n").unwrap();
                std::fs::write(&two, b"second\x00\xff").unwrap();
                let original = b"leg 1 of 2:\nfirst\nleg 2 of 2:\nsecond\x00\xff";
                let source = one.clone();
                let answers = if retry {
                    vec![(503, "{}"), (200, r#"{"value":"a.b.c"}"#)]
                } else {
                    vec![(200, r#"{"value":"a.b.c"}"#)]
                };
                let (url, served) = service_seq_with(answers, move |index| {
                    if index != usize::from(retry) {
                        return;
                    }
                    match change {
                        "append" => std::fs::OpenOptions::new()
                            .append(true)
                            .open(&source)
                            .unwrap()
                            .write_all(b"appended")
                            .unwrap(),
                        "replace" => {
                            std::fs::remove_file(&source).unwrap();
                            std::fs::write(&source, b"replacement").unwrap();
                        }
                        "remove" => std::fs::remove_file(&source).unwrap(),
                        _ => unreachable!(),
                    }
                });
                let out = dir.path().join("out");
                let mut args = if command == "commit" {
                    vec![command, COMMIT]
                } else {
                    vec![command, BASE, HEAD]
                };
                args.extend([
                    "--result",
                    "fail",
                    "--tree",
                    TREE,
                    "--leg-log",
                    one.to_str().unwrap(),
                    "--leg-log",
                    two.to_str().unwrap(),
                    "--log-artifact",
                    "logs",
                    "--out",
                    out.to_str().unwrap(),
                    "--sign",
                ]);
                let run = attest(&args, Some(&url));
                let requests = served.join().unwrap();
                assert!(
                    run.status.success(),
                    "{command} {change} retry={retry}: {run:?}"
                );
                let made: Value = serde_json::from_slice(&run.stdout).unwrap();
                let kept = std::fs::read(made["log"].as_str().unwrap()).unwrap();
                assert_eq!(kept, original, "{command} {change} retry={retry}");
                let predicate: Value = serde_json::from_str(&read(&made["predicate"])).unwrap();
                assert_eq!(
                    predicate["log"],
                    serde_json::to_value(Log::of(&kept, "logs")).unwrap()
                );
                let statement = format!("{}{}", read(&made["subject"]), read(&made["predicate"]));
                assert_eq!(read(&made["attestation"]), format!("{statement}a.b.c\n"));
                for request in requests {
                    assert!(request.contains(&format!("&audience={} ", audience(&statement))));
                }
            }
        }
    }
}

#[test]
fn negative_json_requires_exactly_the_named_members_and_types_before_any_side_effect() {
    let valid = json!({
        "result": "none", "tree": null, "tests": [], "checks": [], "skipped": [], "unrun": []
    });
    let mut invalid = vec![
        r#"["none",null,[],[],[],[]]"#.to_owned(),
        r#"{"result":{"none":null},"tree":null,"tests":[],"checks":[],"skipped":[],"unrun":[]}"#
            .to_owned(),
        "null".to_owned(),
    ];
    for field in ["result", "tree", "tests", "checks", "skipped", "unrun"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(field);
        invalid.push(missing.to_string());
        let mut duplicate = valid.to_string();
        duplicate.pop();
        duplicate.push_str(&format!(",\"{field}\":{}}}", valid[field]));
        invalid.push(duplicate);
        let wrong_types = match field {
            "result" => vec![
                json!(null),
                json!(0),
                json!([]),
                json!({"none": null}),
                json!("maybe"),
            ],
            "tree" => vec![json!(0), json!([]), json!({}), json!(true)],
            _ => vec![
                json!(null),
                json!("list"),
                json!({}),
                json!([0]),
                json!([null]),
                json!([[]]),
                json!([{}]),
            ],
        };
        for wrong in wrong_types {
            let mut value = valid.clone();
            value[field] = wrong;
            invalid.push(value.to_string());
        }
    }
    let mut extra = valid.clone();
    extra["extra"] = json!(null);
    invalid.push(extra.to_string());
    // The shape is valid but the result/tree pair is inconsistent.
    for result in ["pass", "fail", "skipped"] {
        let mut value = valid.clone();
        value["result"] = json!(result);
        invalid.push(value.to_string());
    }
    let mut value = valid.clone();
    value["tree"] = json!(TREE);
    invalid.push(value.to_string());
    value["result"] = json!("fail");
    value["tree"] = json!("not-a-tree");
    invalid.push(value.to_string());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "http://{}/token?api-version=2.0",
        listener.local_addr().unwrap()
    );
    for found in invalid {
        for sign in [false, true] {
            let extra = if sign { &["--sign"][..] } else { &[][..] };
            let (run, _, dir) = negative_from_at(&found, extra, Some(&url));
            assert!(!run.status.success(), "sign={sign}: {found}");
            assert!(!dir.path().join("out").exists(), "sign={sign}: {found}");
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "token requested for sign={sign}: {found}"
            );
        }
    }
}
