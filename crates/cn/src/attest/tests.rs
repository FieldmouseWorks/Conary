use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use clap::Parser;

use super::*;

const COMMIT: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
const TREE: &str = "7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e";
const BASE: &str = "baba000000000000000000000000000000000000";
const HEAD: &str = "eded000000000000000000000000000000000000";

/// The command `args` (after `cn attest`), parsed.
fn parse(args: &[&str]) -> Result<AttestCommand> {
    let cli = Cli::try_parse_from([&["cn", "attest"], args].concat())?;
    match cli.command {
        crate::Command::Attest { command } => Ok(command),
        _ => unreachable!(),
    }
}

/// A token service on localhost answering one request with `status` and
/// `body`, handing back the request's line.
fn service(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
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
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header.trim().is_empty() {
                break;
            }
        }
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        line
    });
    (url, served)
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
    let command = parse(&[
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
    ])
    .unwrap();
    let (claimed, to) = claim(&command).unwrap();
    assert!(to.sign);

    let (url, served) = service(200, r#"{"value":"a.b.c"}"#);
    let request = Request {
        url: &url,
        bearer: "request-token",
    };
    let made = make(&claimed, to, Some(request)).unwrap();
    let line = served.join().unwrap();
    let read = |p: &Path| std::fs::read_to_string(p).unwrap();
    let statement = format!("{}{}", read(&made.subject), read(&made.predicate));
    assert_eq!(made.subject.file_name().unwrap(), "conary-revision");
    assert_eq!(made.predicate_type, make::GATE_TYPE);
    assert_eq!(
        read(&made.subject),
        format!("conary revision v1\n{BASE}\n{HEAD}\n")
    );
    assert_eq!(statement, make::statement(&claimed).unwrap().text());
    let audience = make::audience(&statement);
    assert_eq!(made.audience.as_deref(), Some(&audience[..]));
    assert!(line.contains(&format!("&audience={audience} ")), "{line}");
    let attestation = made.attestation.unwrap();
    assert_eq!(attestation.file_name().unwrap(), ATTESTATION);
    assert_eq!(read(&attestation), format!("{statement}a.b.c\n"));
    let predicate: serde_json::Value = serde_json::from_str(&read(&made.predicate)).unwrap();
    assert_eq!(predicate["skipped"], json!(["agents"]));
    assert_eq!(predicate["log"]["artifact"], json!("flake-log-1"));

    // Unsigned, as a public repo's job writes it for its bundle.
    let unsigned = dir.path().join("unsigned");
    let command = parse(&[
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
        unsigned.to_str().unwrap(),
    ])
    .unwrap();
    let (claimed, to) = claim(&command).unwrap();
    let made = make(&claimed, to, None).unwrap();
    assert_eq!(read(&made.subject), format!("conary commit v1\n{COMMIT}\n"));
    assert!(made.attestation.is_none() && made.audience.is_none());
    assert!(!unsigned.join(ATTESTATION).exists());
}

// A token GitHub refuses leaves nothing behind for a later step to upload.
#[test]
fn attest_writes_nothing_when_the_token_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("negative.log");
    std::fs::write(&log, "").unwrap();
    let out = dir.path().join("out");
    let command = parse(&[
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
    ])
    .unwrap();
    let (claimed, to) = claim(&command).unwrap();
    let (url, served) = service(403, "{}");
    let request = Request {
        url: &url,
        bearer: "request-token",
    };
    let e = make(&claimed, to, Some(request)).err().unwrap().to_string();
    served.join().unwrap();
    assert!(
        e.contains("HTTP 403") && !e.contains("request-token"),
        "{e}"
    );
    assert!(!out.exists(), "{out:?}");
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
    ] {
        assert!(parse(&args).is_err(), "{args:?}");
    }
    for args in [
        with(&["commit", "HEAD", "--result", "pass", "--tree", TREE]),
        with(&["negative", BASE, HEAD, "--result", "fail"]),
        with(&["negative", BASE, HEAD, "--result", "none", "--tree", TREE]),
    ] {
        let command = parse(&args).unwrap();
        let (claimed, to) = claim(&command).unwrap();
        assert!(make(&claimed, to, None).is_err(), "{args:?}");
    }
    assert!(!Path::new(out).exists());
}
