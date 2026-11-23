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
    } else if request.contains("conary-plugin v2") {
        "conary-plugin v2"
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
    let caps = r#""capabilities":{"sandbox":"brings","network":true,"devices":["kvm"],"mcp":true}"#;
    let v2 = |kinds: &str, v2_kinds: &str, models: &str, caps: &str| {
        format!(
            r#""kind":"describe","versions":["conary-plugin v1","conary-plugin v2"],"kinds":{kinds},"v2_kinds":{v2_kinds},"models":{models},{caps},"meters":["week"]"#
        )
    };
    let models = r#"[{"id":"m","efforts":["low","high"],"context_tokens":1000,"max_output_tokens":100}]"#;
    let answer = if describe && mode == "v2-valid" {
        v2(r#"["review"]"#, r#"["job"]"#, models, caps)
    } else if describe && mode == "v2-proof-model" {
        v2(r#"["proof"]"#, r#"["job"]"#, models, caps)
    } else if describe && mode == "v2-no-caps" {
        v2(r#"[]"#, r#"["job"]"#, "[]", r#""meters2":[]"#)
    } else if describe && mode == "v2-dup-model" {
        v2(r#"[]"#, r#"["job"]"#, r#"[{"id":"m","efforts":[]},{"id":"m","efforts":[]}]"#, caps)
    } else if describe && mode == "v1-job" {
        r#""kind":"describe","versions":["conary-plugin v1"],"kinds":["review"],"v2_kinds":["job"]"#.to_string()
    } else if describe && mode == "v2-job-in-kinds" {
        v2(r#"["review","job"]"#, "[]", models, caps)
    } else if describe && mode.starts_with("v2-job") {
        v2(r#"[]"#, r#"["job"]"#, models, caps)
    } else if mode == "v2-job-empty" {
        r#""kind":"job","summary":" ","next":{"step":"done"}"#.to_string()
    } else if mode == "v2-job-bad-ask" {
        r#""kind":"job","summary":"ok","next":{"step":"ask","question":"q","default":"","one_way":false}"#.to_string()
    } else if mode == "v2-job-await" {
        r#""kind":"job","summary":"ok","next":{"step":"await","url":"https://example.test/pr/1"}"#.to_string()
    } else if request.contains("\"kind\":\"job\"") {
        r#""kind":"job","summary":"ok","next":{"step":"done"}"#.to_string()
    } else if describe {
        r#""kind":"describe","versions":["conary-plugin v1"],"kinds":["review"]"#.to_string()
    } else if mode == "absolute-path" {
        r#""kind":"review","findings":[{"path":"/etc/passwd","line":1,"severity":"p1","text":"fixture finding"}]"#.to_string()
    } else if mode == "parent-path" {
        r#""kind":"review","findings":[{"path":"../../outside.rs","line":1,"severity":"p1","text":"fixture finding"}]"#.to_string()
    } else if mode == "nested-escape" {
        r#""kind":"review","findings":[{"path":"src/nested/../../../outside.rs","line":1,"severity":"p1","text":"fixture finding"}]"#.to_string()
    } else if mode == "missing" {
        r#""kind":"review""#.to_string()
    } else if mode == "undescribed" {
        r#""kind":"judge","verdict":"pass","why":"wrong kind""#.to_string()
    } else if mode == "escape" {
        r#""kind":"review","findings":[{"path":"src/../../outside.rs","line":1,"severity":"p1","text":"outside"}]"#.to_string()
    } else {
        r#""kind":"review","findings":[]"#.to_string()
    };
    print!("{{\"contract\":\"{contract}\",\"by\":{{{by}}},{answer}}}");
}
