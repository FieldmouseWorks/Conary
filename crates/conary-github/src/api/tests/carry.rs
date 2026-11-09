use super::*;
use base64::Engine;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
fn reply(v: Value) -> &'static str {
    let body = v.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .leak()
}
fn id(b: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", b.len()).as_bytes());
    h.update(b);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
fn tree(content: &[u8]) -> Value {
    json!({"truncated":false,"tree":[{"path":"same.txt","mode":"100644","type":"blob","sha":id(content)}]})
}
fn blob(content: &[u8]) -> Value {
    json!({"sha":id(content),"encoding":"base64","content":base64::engine::general_purpose::STANDARD.encode(content)})
}
#[test]
fn carried_same_file_edits_use_native_merge_and_only_read_github() {
    let ancestor = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n";
    let onto = b"TOP\none\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n";
    let reviewed = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\nPR\nnine\n";
    let merged = b"TOP\none\ntwo\nthree\nfour\nfive\nsix\nseven\nPR\nnine\n";
    for (actual, expected) in [
        (&merged[..], true),
        (&b"unreviewed resolution\n"[..], false),
    ] {
        let replies = vec![
            json!({"merge_base_commit":{"sha":"fork"},"files":[]}),
            json!({"merge_base_commit":{"sha":"left"},"files":[]}),
            tree(ancestor),
            tree(onto),
            tree(reviewed),
            tree(actual),
            blob(ancestor),
            blob(onto),
            blob(reviewed),
        ];
        let (root, requests) = github(replies.into_iter().map(reply).collect());
        let api = Api::new(&root, Tokens::given("test"));
        assert_eq!(api.carries("o/r", "main", "head", "old").unwrap(), expected);
        let requests: Vec<_> = requests.try_iter().collect();
        assert_eq!(requests.len(), 9);
        assert!(requests.iter().all(|r| r.starts_with("get ")));
    }
}
#[test]
fn a_cut_tree_cannot_carry_evidence() {
    let (root, _) = github(vec![
        reply(json!({"merge_base_commit":{"sha":"fork"},"files":[]})),
        reply(json!({"merge_base_commit":{"sha":"left"},"files":[]})),
        reply(json!({"truncated":true,"tree":[]})),
    ]);
    assert!(
        Api::new(&root, Tokens::given("test"))
            .carries("o/r", "main", "head", "old")
            .is_err()
    );
}
