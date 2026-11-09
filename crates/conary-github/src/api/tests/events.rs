use super::*;
use crate::events::{Kick, Queue, Receiver};
use crate::flow::{Stage, Write};
use crate::train::{Forge, Mark};
use ring::hmac;
use std::io::Write as _;
use std::sync::Arc;
fn deliver(receiver: &mut Receiver, queue: &Queue, id: u64, context: &str, state: &str) {
    let body = serde_json::to_vec(&serde_json::json!({"id":id,"repository":{"full_name":"o/r"},"context":context,"state":state,"sha":"h"})).unwrap();
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, b"s"), &body);
    let sig = format!(
        "sha256={}",
        tag.as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_eq!(
        receiver.receive(Some(&sig), &id.to_string(), "status", &body, 1, queue),
        202
    );
}
#[test]
fn status_echoes_before_and_after_write_responses_quiesce_without_hiding_external_updates() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Arc::new(Queue::open(dir.path()).unwrap());
    queue
        .kick(Kick {
            repo: "o/r".into(),
            pr: None,
        })
        .unwrap();
    let work = queue.take(1).unwrap().unwrap();
    let incoming = Arc::clone(&queue);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let mut receiver = Receiver::open(b"s".to_vec(), ["o/r".into()], dir.path()).unwrap();
        for (id, context, state) in [
            (1, "review-settled", "pending"),
            (2, "review-settled", "success"),
            (3, "review-settled", "pending"),
            (4, "conary/train", "success"),
        ] {
            let (mut conn, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            // Delivery can beat the response which tells us its ID.
            deliver(&mut receiver, &incoming, id, context, state);
            let body = format!("{{\"id\":{id}}}");
            write!(
                conn,
                "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        (dir, receiver)
    });
    let api = Api::through(&base, Tokens::given("t"), None, CALL_FOR)
        .with_event_queue(Arc::clone(&queue));
    for settled in [false, true, false] {
        api.status("o/r", "h", settled, "same decision").unwrap();
    }
    api.mark("o/r", "h", Mark::Landing, "land", None).unwrap();
    let (dir, mut receiver) = server.join().unwrap();
    queue.finish(&work, &[], 1).unwrap();
    assert!(
        queue.take(1).unwrap().is_none(),
        "own pending/success/revocation and train echoes must stop"
    );
    // An external write to our context must still trigger a safety recheck.
    deliver(&mut receiver, &queue, 100, "review-settled", "success");
    let work = queue
        .take(1)
        .unwrap()
        .expect("external permission change must be rechecked");
    queue.finish(&work, &[], 1).unwrap();
    deliver(&mut receiver, &queue, 101, "check", "failure");
    let work = queue.take(1).unwrap().unwrap();
    queue.finish(&work, &[], 1).unwrap();
    drop(api);
    drop(queue);
    let queue = Queue::open(dir.path()).unwrap();
    // Receipt retention survives restart, as do queued echoes.
    queue.status("o/r", 1).unwrap();
    assert!(queue.take(2).unwrap().is_none());
}
#[test]
fn unchanged_stage_labels_are_read_without_reposting_them() {
    let pull = serde_json::from_str::<serde_json::Value>(PULL).unwrap();
    let mut labeled = pull;
    labeled["labels"] = serde_json::json!([{"name":"waiting: CI"}]);
    let body = serde_json::to_string(&labeled).unwrap();
    let answer: &'static str = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .leak();
    let (base, asked) = github(vec![answer, answer]);
    let api = Api::through(&base, Tokens::given("t"), None, CALL_FOR);
    api.stage("o/r", 7, Stage::Ci).unwrap();
    assert!(asked.recv().unwrap().starts_with("get "));
    assert!(asked.recv().unwrap().starts_with("get "));
}
