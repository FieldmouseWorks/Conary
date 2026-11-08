// Dependency-free executable used by plugin.rs. Compiled once by the tests.
use std::io::{Read, Write};
fn main() {
    let mode = std::env::current_exe()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut request = String::new();
    std::io::stdin().read_to_string(&mut request).unwrap();
    let describe = request.contains("\"kind\":\"describe\"");
    if mode == "deadline" {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    if mode == "exit" {
        std::process::exit(7);
    }
    if mode == "malformed" {
        print!("{{");
        return;
    }
    if mode == "large" {
        std::io::stdout().write_all(&vec![b' '; 16_384]).unwrap();
        return;
    }
    let contract = if mode == "version" {
        "conary-plugin v0"
    } else {
        "conary-plugin v1"
    };
    let by = if mode == "cross-author" {
        r#""engine":"another-engine","session":"fixture-author""#
    } else if mode == "unknown-session" {
        r#""engine":"conary-conformance-author","session":null"#
    } else if mode == "author" && !describe {
        r#""engine":"conary-conformance-author","session":"fixture-author""#
    } else {
        r#""engine":"conary-conformance-author","session":"independent""#
    };
    let answer = if describe {
        r#""kind":"describe","versions":["conary-plugin v1"],"kinds":["review"]"#
    } else if mode == "absolute-path" {
        r#""kind":"review","findings":[{"path":"/etc/passwd","line":1,"severity":"p1","text":"fixture finding"}]"#
    } else if mode == "parent-path" {
        r#""kind":"review","findings":[{"path":"../../outside.rs","line":1,"severity":"p1","text":"fixture finding"}]"#
    } else if mode == "nested-escape" {
        r#""kind":"review","findings":[{"path":"src/nested/../../../outside.rs","line":1,"severity":"p1","text":"fixture finding"}]"#
    } else if mode == "missing" {
        r#""kind":"review""#
    } else if mode == "undescribed" {
        r#""kind":"judge","verdict":"pass","why":"wrong kind""#
    } else {
        r#""kind":"review","findings":[]"#
    };
    print!("{{\"contract\":\"{contract}\",\"by\":{{{by}}},{answer}}}");
}
