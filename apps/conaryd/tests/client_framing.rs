// apps/conaryd/tests/client_framing.rs

use conaryd::daemon::{DaemonEvent, client::DaemonClient};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn server(
    responses: Vec<Vec<u8>>,
    hold_open: bool,
) -> (
    tempfile::TempDir,
    DaemonClient,
    mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("framing.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (release, wait) = mpsc::channel();
    let client = DaemonClient::with_socket_path(path).with_timeout(Duration::from_millis(200));
    let task = std::thread::spawn(move || {
        for response in responses {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing client request");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                assert_ne!(request.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            let _ = stream.write_all(&response);
            if hold_open {
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
            }
        }
    });
    (root, client, release, task)
}

fn details() -> String {
    serde_json::json!({"id":"job-1", "kind":"install", "status":"completed",
        "spec":{"message":"snowman ☃"}, "created_at":"2026-09-10T00:00:00Z"})
    .to_string()
}

fn fixed(media: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn chunked(media: &str, body: &[u8], size: usize) -> Vec<u8> {
    let mut response =
        format!("HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nTransfer-Encoding: chunked\r\n\r\n")
            .into_bytes();
    for part in body.chunks(size) {
        response.extend_from_slice(format!("{:x};example=\"a;\\\"b\"\r\n", part.len()).as_bytes());
        response.extend_from_slice(part);
        response.extend_from_slice(b"\r\n");
    }
    response.extend_from_slice(b"0\r\nX-Fixture: finished\r\n\r\n");
    response
}

fn close_delimited(media: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 200 OK\r\nContent-Type: {media}\r\n\r\n").into_bytes();
    response.extend_from_slice(body);
    response
}

#[test]
fn json_is_decoded_across_wire_chunks_and_utf8_boundaries() {
    let body = details();
    let mut responses = vec![
        fixed("application/json", body.as_bytes()),
        close_delimited("application/json", body.as_bytes()),
    ];
    responses
        .extend([1, 2, 7, 4096].map(|size| chunked("application/json", body.as_bytes(), size)));
    for response in responses {
        let (_root, client, _release, task) = server(vec![response], false);
        let result = client.get_transaction("job-1");
        task.join().unwrap();
        assert_eq!(result.unwrap().spec["message"], "snowman ☃");
    }
}

#[test]
fn framed_json_completes_without_waiting_for_socket_close() {
    let body = details();
    for response in [
        fixed("application/json", body.as_bytes()),
        chunked("application/json", body.as_bytes(), 3),
    ] {
        let (_root, client, release, task) = server(vec![response], true);
        let result = client.get_transaction("job-1");
        release.send(()).unwrap();
        task.join().unwrap();
        assert_eq!(result.unwrap().status, "completed");
    }
}

#[test]
fn declared_length_truncation_is_not_accepted_as_complete_json() {
    let body = details();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len() + 1
    );
    let (_root, client, _release, task) = server(vec![response.into_bytes()], false);
    let result = client.get_transaction("job-1");
    task.join().unwrap();
    assert!(
        result.is_err(),
        "valid JSON did not satisfy its declared HTTP length"
    );
}

#[test]
fn sse_events_receive_decoded_content_for_every_framing_mode() {
    let events = b"data: {\"type\":\"job_started\",\"job_id\":\"job-1\"}\n\ndata: {\"type\":\"job_completed\",\"job_id\":\"job-1\",\"duration_ms\":1}\n\n";
    for response in [
        fixed("text/event-stream", events),
        close_delimited("text/event-stream", events),
        chunked("text/event-stream", events, 1),
        chunked("text/event-stream", events, 7),
    ] {
        let (_root, client, _release, task) = server(
            vec![response, fixed("application/json", details().as_bytes())],
            false,
        );
        let mut seen = Vec::new();
        let result = client.wait_for_job("job-1", |event| seen.push(event));
        task.join().unwrap();
        assert_eq!(result.unwrap().status, "completed");
        assert!(matches!(
            seen.as_slice(),
            [
                DaemonEvent::JobStarted { .. },
                DaemonEvent::JobCompleted { .. }
            ]
        ));
    }
}

#[test]
fn ambiguous_framing_fails_before_sse_callback() {
    let response = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n0\r\n\r\n".to_vec();
    let (_root, client, _release, task) = server(vec![response], false);
    let mut events = 0;
    let result = client.wait_for_job("job-1", |_| events += 1);
    task.join().unwrap();
    assert!(result.is_err());
    assert_eq!(events, 0);
}

#[test]
fn malformed_or_truncated_chunk_frames_are_rejected() {
    for wire in [
        "z\r\n{}\r\n0\r\n\r\n",
        "2\r\n{",
        "2\r\n{}XX0\r\n\r\n",
        "2\r\n{}\r\n",
        "0\r\nContent-Type: text/plain\r\n\r\n",
        "0\r\nX-Fixture: unfinished",
    ] {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{wire}"
        );
        let (_root, client, _release, task) = server(vec![response.into_bytes()], false);
        let error = client.get_transaction("job-1").unwrap_err().to_string();
        task.join().unwrap();
        assert!(error.len() < 256, "{error}");
    }
}

#[test]
fn invalid_chunk_metadata_cannot_wrap_an_otherwise_valid_json_result() {
    let body = details();
    for wire in [
        format!("{:x} \r\n{body}\r\n0\r\n\r\n", body.len()),
        format!("{:x};flag \r\n{body}\r\n0\r\n\r\n", body.len()),
        format!("{:x}\r\n{body}\r\n0\r\nX-Fixture: yes\n\n", body.len()),
        format!("{:x}\r\n{body}\r\n0\r\n\n", body.len()),
    ] {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{wire}"
        );
        let (_root, client, _release, task) = server(vec![response.into_bytes()], false);
        let result = client.get_transaction("job-1");
        task.join().unwrap();
        assert!(result.is_err(), "accepted malformed chunk metadata");
    }
}
