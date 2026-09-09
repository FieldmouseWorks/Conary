// apps/conaryd/tests/client_media_type.rs

use conaryd::daemon::{DaemonEvent, client::DaemonClient};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

fn server(
    responses: Vec<String>,
) -> (tempfile::TempDir, DaemonClient, std::thread::JoinHandle<()>) {
    server_bytes(responses.into_iter().map(String::into_bytes).collect())
}

fn server_bytes(
    responses: Vec<Vec<u8>>,
) -> (tempfile::TempDir, DaemonClient, std::thread::JoinHandle<()>) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("daemon.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let client = DaemonClient::with_socket_path(path).with_timeout(Duration::from_secs(1));
    let task = std::thread::spawn(move || {
        for response in responses {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "client did not request next response"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            // An early media-type refusal may close before consuming the body.
            let _ = stream.write_all(&response);
        }
    });
    (root, client, task)
}

fn details() -> String {
    serde_json::json!({
        "id":"job-1", "kind":"install", "status":"completed", "spec":{},
        "created_at":"2026-09-10T00:00:00Z"
    })
    .to_string()
}

fn reply(content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn valid_json_bytes_do_not_override_the_declared_media_type() {
    let (_root, client, task) = server(vec![reply("text/plain", &details())]);
    let error = client.get_transaction("job-1").unwrap_err().to_string();
    task.join().unwrap();
    assert!(error.contains("Content-Type"), "{error}");
    assert!(error.len() < 256);
}

#[test]
fn problem_json_preserves_the_daemon_error() {
    let body =
        serde_json::to_string(&conaryd::daemon::DaemonError::not_found("fixture missing")).unwrap();
    let response = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: application/problem+json; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let (_root, client, task) = server(vec![response]);
    let error = client.get_transaction("job-1").unwrap_err().to_string();
    task.join().unwrap();
    assert!(error.contains("fixture missing"), "{error}");
}

#[test]
fn wrong_sse_type_is_rejected_before_callback() {
    let body = "data: {\"type\":\"job_completed\",\"job_id\":\"job-1\",\"duration_ms\":1}\n\n";
    let (_root, client, task) = server(vec![reply("application/json", body)]);
    let mut events = 0;
    let error = client
        .wait_for_job("job-1", |_| events += 1)
        .unwrap_err()
        .to_string();
    task.join().unwrap();
    assert_eq!(events, 0);
    assert!(error.contains("Content-Type"), "{error}");
}

#[test]
fn parameterized_sse_then_json_completes_the_job() {
    let body = "data: {\"type\":\"job_completed\",\"job_id\":\"job-1\",\"duration_ms\":1}\n\n";
    let response = reply("Text/Event-Stream; charset=\"utf-8\"", body)
        .replace("Content-Type:", "cOnTeNt-TyPe:");
    let (_root, client, task) = server(vec![
        response,
        reply("Application/JSON; charset=utf-8", &details()),
    ]);
    let mut events = Vec::new();
    let result = client
        .wait_for_job("job-1", |event| events.push(event))
        .unwrap();
    task.join().unwrap();
    assert_eq!(result.status, "completed");
    assert!(matches!(
        events.as_slice(),
        [DaemonEvent::JobCompleted { .. }]
    ));
}

#[test]
fn cancellation_without_a_response_body_needs_no_media_type() {
    let (_root, client, task) = server(vec!["HTTP/1.1 204 No Content\r\n\r\n".into()]);
    client.cancel_transaction("job-1").unwrap();
    task.join().unwrap();
}

#[test]
fn malformed_header_name_is_not_normalized_into_authority() {
    let response = reply("application/json", &details()).replace("Content-Type:", "Content-Type :");
    let (_root, client, task) = server(vec![response]);
    let error = client.get_transaction("job-1").unwrap_err().to_string();
    task.join().unwrap();
    assert!(error.contains("headers"), "{error}");
}

#[test]
fn slow_sse_header_trickle_cannot_extend_the_header_deadline() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("slow.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let task = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        for byte in b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n" {
            if stream.write_all(&[*byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let client = DaemonClient::with_socket_path(path).with_timeout(Duration::from_millis(80));
    let start = Instant::now();
    let result = client.wait_for_job("job-1", |_| panic!("unverified event"));
    let elapsed = start.elapsed();
    task.join().unwrap();
    assert!(result.is_err());
    assert!(
        elapsed < Duration::from_millis(400),
        "header deadline extended to {elapsed:?}"
    );
}

#[test]
fn response_type_refusal_precedes_invalid_body_utf8() {
    for (status, fields) in [
        (200, ""),
        (200, "Content-Type: text/plain\r\n"),
        (200, "Content-Type: invalid type\r\n"),
        (
            200,
            "Content-Type: application/json\r\nContent-Type: application/json\r\n",
        ),
        (500, "Content-Type: application/json\r\n"),
    ] {
        let mut raw =
            format!("HTTP/1.1 {status} Fixture\r\n{fields}Content-Length: 2\r\n\r\n").into_bytes();
        raw.extend_from_slice(&[0xff, 0xfe]);
        let (_root, client, task) = server_bytes(vec![raw]);
        let error = client.get_transaction("job-1").unwrap_err().to_string();
        task.join().unwrap();
        assert!(error.contains("Content-Type"), "{status} {fields}: {error}");
        assert!(error.len() < 256);
    }
}
