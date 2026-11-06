use super::*;

fn repo(s: &str) -> Repo {
    s.parse().unwrap()
}

fn paths(files: &[&str]) -> Paths {
    Paths {
        files: files
            .iter()
            .map(|p| Touched {
                path: (*p).to_owned(),
                from: None,
            })
            .collect(),
        cut: false,
    }
}

const FILE: &str = r#"{
  "r2": ["(^|/)CODEOWNERS$", "(^|/)secrets?/"],
  "docs": ["^docs/", "(^|/)README\\.md$", "^AGENTS\\.md$"],
  "repos": {
    "Owner/Live": {"live": true, "r2": ["^migrations/"], "notDocs": ["^docs/DESIGN\\.md$"]},
    "owner/notes": {"docs": ["^notes/"]}
  }
}"#;

fn policy() -> Policy {
    Policy::parse(FILE.as_bytes()).unwrap()
}

#[test]
fn a_change_touching_an_r2_path_is_r2_and_says_which() {
    let got = policy().classify(&repo("o/plain"), &paths(&["src/a.rs", "secrets/x.yaml"]));
    assert_eq!(got.class, Class::R2);
    assert_eq!(got.because.as_deref(), Some("secrets/x.yaml"));
    assert!(got.review());
}

#[test]
fn a_rename_out_of_an_r2_path_is_r2() {
    let p = Paths {
        files: vec![Touched {
            path: "CODEOWNERS.bak".into(),
            from: Some("CODEOWNERS".into()),
        }],
        cut: false,
    };
    let got = policy().classify(&repo("o/plain"), &p);
    assert_eq!(
        (got.class, got.because.as_deref()),
        (Class::R2, Some("CODEOWNERS"))
    );
}

#[test]
fn a_list_cut_short_is_r2_and_never_docs() {
    let mut p = paths(&["docs/a.md"]);
    p.cut = true;
    let got = policy().classify(&repo("o/plain"), &p);
    assert_eq!(got.class, Class::R2);
    assert!(got.because.unwrap().contains("too many"));
    assert!(!got.docs);
}

#[test]
fn a_live_repo_is_r1_and_its_r2_paths_add_to_the_machines() {
    let p = policy();
    let live = repo("owner/LIVE");
    assert_eq!(p.classify(&live, &paths(&["src/a.rs"])).class, Class::R1);
    assert_eq!(
        p.classify(&live, &paths(&["migrations/1.sql"])).class,
        Class::R2
    );
    assert_eq!(p.classify(&live, &paths(&["CODEOWNERS"])).class, Class::R2);
    // Another repo doesn't get its additions.
    assert_eq!(
        p.classify(&repo("o/plain"), &paths(&["migrations/1.sql"]))
            .class,
        Class::R0
    );
}

#[test]
fn docs_only_needs_no_review() {
    let got = policy().classify(&repo("o/plain"), &paths(&["docs/a.md", "README.md"]));
    assert_eq!(got.class, Class::R0);
    assert!(got.docs && !got.review());
}

#[test]
fn one_path_that_isnt_docs_makes_it_code() {
    let got = policy().classify(&repo("o/plain"), &paths(&["docs/a.md", "src/a.rs"]));
    assert!(!got.docs && got.review());
}

#[test]
fn no_paths_is_never_docs() {
    assert!(!policy().classify(&repo("o/plain"), &paths(&[])).docs);
}

#[test]
fn rules_for_agents_and_the_gate_are_never_docs() {
    // The machine's docs name AGENTS.md; it's reviewed all the same.
    let p = Policy::parse(br#"{"docs": [".*"]}"#).unwrap();
    for path in [
        "AGENTS.md",
        "sub/CLAUDE.md",
        "AGENTS.override.md",
        ".github/workflows/x.md",
    ] {
        assert!(
            !p.classify(&repo("o/plain"), &paths(&[path])).docs,
            "{path} counted as docs"
        );
    }
    assert!(
        policy()
            .classify(&repo("o/plain"), &paths(&["docs/x.md"]))
            .docs
    );
    assert!(
        !policy()
            .classify(&repo("o/plain"), &paths(&["AGENTS.md"]))
            .docs
    );
}

#[test]
fn a_repos_not_docs_and_docs_add_to_the_machines() {
    let p = policy();
    let live = repo("owner/live");
    assert!(!p.classify(&live, &paths(&["docs/DESIGN.md"])).docs);
    assert!(p.classify(&live, &paths(&["docs/other.md"])).docs);
    assert!(p.classify(&repo("owner/notes"), &paths(&["notes/x"])).docs);
    assert!(!p.classify(&repo("o/plain"), &paths(&["notes/x"])).docs);
}

#[test]
fn a_rename_into_docs_from_code_isnt_docs() {
    let p = Paths {
        files: vec![Touched {
            path: "docs/old.rs".into(),
            from: Some("src/old.rs".into()),
        }],
        cut: false,
    };
    assert!(!policy().classify(&repo("o/plain"), &p).docs);
}

#[test]
fn a_docs_change_on_an_r2_path_still_needs_review() {
    let p = Policy::parse(br#"{"r2": ["^docs/decisions/"], "docs": ["^docs/"]}"#).unwrap();
    let got = p.classify(&repo("o/x"), &paths(&["docs/decisions/1.md"]));
    assert_eq!(got.class, Class::R2);
    assert!(got.docs && got.review());
}

#[test]
fn a_repos_live_overrides_the_machines() {
    let p = Policy::parse(br#"{"live": true, "repos": {"o/quiet": {"live": false}}}"#).unwrap();
    assert_eq!(p.classify(&repo("o/x"), &paths(&["a"])).class, Class::R1);
    assert_eq!(
        p.classify(&repo("o/quiet"), &paths(&["a"])).class,
        Class::R0
    );
}

#[test]
fn unknown_keys_bad_patterns_and_repos_named_twice_are_refused() {
    for bad in [
        r#"{"oneWay": []}"#,
        r#"{"repos": {"o/x": {"reviewers": 1}}}"#,
        r#"{"r2": ["("]}"#,
        r#"{"repos": {"o/x": {"docs": ["["]}}}"#,
        r#"{"repos": {"o/x": {}, "O/X": {}}}"#,
        // The same key twice, as written or escaped: a map would keep the
        // last, and drop the first's R2 paths.
        r#"{"repos": {"o/x": {"r2": ["^docs/"]}, "o/x": {"docs": ["^docs/"]}}}"#,
        r#"{"repos": {"o/x": {"r2": ["^docs/"]}, "o\/x": {}}}"#,
        r#"{"repos": {"nobody": {}}}"#,
        r#"{"repos": {"o/x": {"repos": {}}}}"#,
        "not json",
    ] {
        assert!(Policy::parse(bad.as_bytes()).is_err(), "took {bad}");
    }
}

#[test]
fn the_digest_is_the_files_and_changes_with_it() {
    let a = Policy::parse(b"{}").unwrap();
    let b = Policy::parse(b"{ }").unwrap();
    assert_eq!(a.digest(), blake3::hash(b"{}").to_hex().as_str());
    assert_ne!(a.digest(), b.digest());
}
