//! `cn attest`, run as a CI job runs it.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

use conary_github::attest::make::audience;
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
