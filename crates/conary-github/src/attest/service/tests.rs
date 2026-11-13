//! A service's signed statement counts only for what it says, signed with a
//! key the machine trusts for its producer and check; everything else fails
//! closed.

use serde_json::json;

use super::*;
use crate::attest::testing::{
    PRODUCER, SIGNER, author_key, producer_key, producers, producers_file, service_statement,
};
use crate::attest::{Trust, attested};
use crate::sim::{Call, Hub};

const REPO: &str = "FieldmouseWorks/remi";
const COMMIT: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";
const BASE: &str = "3333333333333333333333333333333333333333";
const CHECK: &str = "conary/review/reviewer";
const PUSH: Subject = Subject::Commit {
    commit: COMMIT,
    tree: TREE,
    definition: None,
};

fn log() -> String {
    "a".repeat(64)
}

fn repo() -> Repo {
    REPO.parse().unwrap()
}

fn run(name: &str, conclusion: &str, text: Option<String>) -> CheckRun {
    serde_json::from_value(json!({
        "id": 41, "name": name, "status": "completed", "conclusion": conclusion,
        "html_url": null, "started_at": null, "check_suite": null,
        "output": {"title": null, "summary": null, "text": text},
    }))
    .unwrap()
}

fn honest() -> Statement {
    service_statement(REPO, COMMIT, TREE, CHECK, &log())
}

/// Checks `text` as check run `CHECK`'s passing output on the commit.
fn checked(text: &str) -> Result<Attested, String> {
    check(
        text,
        &producers(&["conary/review/*"]),
        &repo(),
        PUSH,
        &run(CHECK, "success", Some(text.into())),
    )
}

/// `honest()`, changed, then signed with the trusted key.
fn signed(change: impl FnOnce(&mut Statement)) -> String {
    let mut s = honest();
    change(&mut s);
    sign(&s, &producer_key())
}

#[test]
fn a_trusted_producers_statement_counts_for_exactly_what_it_says() {
    let text = sign(&honest(), &producer_key());
    let attested = checked(&text).unwrap();
    assert_eq!(attested.result, Outcome::Pass);
    assert_eq!(attested.log, Some(log()));
    let (statement, line) = text.trim_end().rsplit_once('\n').unwrap();
    let (_, rest) = line.split_once(' ').unwrap();
    let (key, signature) = rest.split_once(' ').unwrap();
    assert_eq!(
        attested.attest,
        Attest::Service {
            statement: format!("{statement}\n"),
            key: key.into(),
            signature: signature.into(),
        }
    );
    // Its repository's spelling is GitHub's to choose.
    checked(&signed(|s| s.repository = "fieldmouseworks/REMI".into())).unwrap();
}

#[test]
fn a_statement_altered_after_signing_doesnt_count() {
    let text = sign(&honest(), &producer_key());
    for (from, to) in [
        (r#""result":"pass""#, r#""result":"fail""#),
        (
            &*format!(r#""sha256":"{}""#, log()),
            &*format!(r#""sha256":"{}""#, "b".repeat(64)),
        ),
        (COMMIT, "4444444444444444444444444444444444444444"),
        (TREE, "5555555555555555555555555555555555555555"),
        (PRODUCER, "remi-proof"),
        (CHECK, "conary/review/other"),
    ] {
        assert!(text.contains(from), "{from}");
        let why = checked(&text.replacen(from, to, 1)).unwrap_err();
        assert!(why.contains("signature doesn't verify"), "{from}: {why}");
    }
}

#[test]
fn a_signed_statement_counts_only_for_its_own_repository_revision_tree_check_and_result() {
    let other = "4444444444444444444444444444444444444444";
    for (text, says) in [
        (
            signed(|s| s.repository = "FieldmouseWorks/burrow".into()),
            "burrow's",
        ),
        (signed(|s| s.revision.head = other.into()), "about 4444"),
        (signed(|s| s.revision.base = Some(BASE.into())), "on 3333"),
        (signed(|s| s.tree = Some(other.into())), "tree 4444"),
        (signed(|s| s.tree = None), "tree none"),
        (
            signed(|s| s.check = "conary/review/other".into()),
            "check \"conary/review/other\"",
        ),
        (signed(|s| s.result = "fail".into()), "it says fail"),
        (signed(|s| s.result = "error".into()), "it says error"),
        (signed(|s| s.log.sha256 = "A".repeat(64)), "no log"),
        (signed(|s| s.log.sha256 = "a".repeat(63)), "no log"),
    ] {
        let why = checked(&text).unwrap_err();
        assert!(why.contains(says), "{says}: {why}");
    }
    // A pass its own check run doesn't agree with is no pass.
    let text = sign(&honest(), &producer_key());
    for conclusion in ["failure", "neutral", "skipped", "cancelled"] {
        let why = check(
            &text,
            &producers(&["conary/review/*"]),
            &repo(),
            PUSH,
            &run(CHECK, conclusion, Some(text.clone())),
        )
        .unwrap_err();
        assert!(why.contains(conclusion), "{conclusion}: {why}");
    }
}

#[test]
fn only_a_key_the_machine_trusts_for_the_producer_and_the_check_counts() {
    // The author's own key, however well it says the rest: unknown.
    let why = checked(&sign(&honest(), &author_key())).unwrap_err();
    assert!(why.contains("doesn't trust"), "{why}");
    // A trusted key says whose it is: another producer's name on it is a
    // claim the machine didn't make.
    let why = checked(&signed(|s| s.producer = "remi-proof".into())).unwrap_err();
    assert!(why.contains("its key is remi-reviewer's"), "{why}");
    // Trusted for its own checks alone.
    let text = sign(&honest(), &producer_key());
    for scope in [
        &["conary/proof/*"][..],
        &["conary/review/reviewerx"],
        &["conary/review"],
    ] {
        let why = check(
            &text,
            &producers(scope),
            &repo(),
            PUSH,
            &run(CHECK, "success", Some(text.clone())),
        )
        .unwrap_err();
        assert!(why.contains("isn't trusted to attest"), "{scope:?}: {why}");
    }
    check(
        &text,
        &producers(&["conary/proof/*", CHECK]),
        &repo(),
        PUSH,
        &run(CHECK, "success", Some(text.clone())),
    )
    .unwrap();
    // With no producers configured, nothing counts.
    let why = check(
        &text,
        &Producers::default(),
        &repo(),
        PUSH,
        &run(CHECK, "success", Some(text.clone())),
    )
    .unwrap_err();
    assert!(why.contains("doesn't trust"), "{why}");
}

#[test]
fn a_missing_or_malformed_signature_fails_closed() {
    let good = sign(&honest(), &producer_key());
    let statement = good.rsplitn(3, '\n').nth(2).unwrap().to_owned() + "\n";
    let line = good.trim_end().rsplit_once('\n').unwrap().1;
    let parts: Vec<&str> = line.split(' ').collect();
    let flipped = {
        let mut sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        sig[0] ^= 1;
        URL_SAFE_NO_PAD.encode(sig)
    };
    for (text, says) in [
        (statement.clone(), "signature line"),
        (
            good.trim_end().to_owned(),
            "a statement and then a signature",
        ),
        (
            format!("{statement}ed25519 {}\n", parts[1]),
            "signature line",
        ),
        (
            format!("{statement}rsa {} {}\n", parts[1], parts[2]),
            "signature line",
        ),
        (
            format!("{statement}ed25519 {} {} x\n", parts[1], parts[2]),
            "signature line",
        ),
        (
            format!("{statement}ed25519 {} {flipped}\n", parts[1]),
            "doesn't verify",
        ),
        (
            format!("{statement}ed25519 {} AAAA\n", parts[1]),
            "64 bytes",
        ),
        (
            format!("{statement}ed25519 AAAA {}\n", parts[2]),
            "32 bytes",
        ),
        (
            good.replacen(HEADER, "conary service v2", 1),
            "isn't `conary service v1`",
        ),
        (
            format!("{statement}\n{line}\n"),
            "isn't `conary service v1` and one",
        ),
        (good.clone() + &"x".repeat(MOST as usize), "past"),
    ] {
        let why = checked(&text).unwrap_err();
        assert!(why.contains(says), "{says}: {why}");
    }
    // A statement that's signed but isn't one: unknown fields and all.
    let key = producer_key();
    for predicate in [json!({"producer": PRODUCER}).to_string(), {
        let mut v = serde_json::to_value(honest()).unwrap();
        v["extra"] = json!(true);
        v.to_string()
    }] {
        let statement = format!("{HEADER}\n{predicate}\n");
        let text = format!(
            "{statement}{SCHEME} {} {}\n",
            URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
            URL_SAFE_NO_PAD.encode(key.sign(statement.as_bytes()).to_bytes())
        );
        let why = checked(&text).unwrap_err();
        assert!(why.contains("doesn't read"), "{why}");
    }
}

#[test]
fn a_negative_counts_for_its_revision_with_what_its_tests_did_on_the_old_code() {
    let negative = |result: &str, tree: Option<&str>, conclusion: &str| {
        let mut s = service_statement(REPO, COMMIT, TREE, "negative", &log());
        s.revision.base = Some(BASE.into());
        s.tree = tree.map(Into::into);
        s.result = result.into();
        let text = sign(&s, &producer_key());
        check(
            &text,
            &producers(&["negative"]),
            &repo(),
            Subject::Negative {
                base: BASE,
                head: COMMIT,
            },
            &run("negative", conclusion, Some(text.clone())),
        )
    };
    assert_eq!(
        negative("fail", Some(TREE), "success").unwrap().result,
        Outcome::Fail
    );
    assert_eq!(
        negative("none", None, "success").unwrap().result,
        Outcome::None
    );
    assert_eq!(
        negative("pass", Some(TREE), "failure").unwrap().result,
        Outcome::Pass
    );
    for (result, tree, conclusion, says) in [
        ("fail", Some(TREE), "failure", "check run failure"),
        ("pass", Some(TREE), "success", "check run success"),
        ("error", Some(TREE), "success", "isn't a negative's result"),
        ("fail", None, "success", "tree doesn't fit"),
        ("none", Some(TREE), "success", "tree doesn't fit"),
        ("fail", Some("tree"), "success", "tree doesn't fit"),
    ] {
        let why = negative(result, tree, conclusion).unwrap_err();
        assert!(why.contains(says), "{result} {tree:?}: {why}");
    }
    // A commit's statement is no negative, and a negative's no commit's.
    let text = sign(&honest(), &producer_key());
    let why = check(
        &text,
        &producers(&["conary/review/*"]),
        &repo(),
        Subject::Negative {
            base: BASE,
            head: COMMIT,
        },
        &run(CHECK, "success", Some(text.clone())),
    )
    .unwrap_err();
    assert!(
        why.contains("not 1111111111111111111111111111111111111111 on 3333"),
        "{why}"
    );
}

#[test]
fn the_machines_producer_keys_are_checked_as_they_are_read() {
    let key = producer_key();
    let other = author_key();
    let ok = producers_file(&[
        (PRODUCER, &key, &["conary/review/*"]),
        (PRODUCER, &other, &["x"]),
    ]);
    Producers::parse(&ok).unwrap();
    let encoded = URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes());
    for (file, says) in [
        (producers_file(&[(PRODUCER, &key, &[])]), "names no check"),
        (producers_file(&[(PRODUCER, &key, &[""])]), "isn't a check"),
        (producers_file(&[(PRODUCER, &key, &["*"])]), "isn't a check"),
        (
            producers_file(&[(PRODUCER, &key, &["a*b"])]),
            "isn't a check",
        ),
        (
            producers_file(&[("", &key, &["x"])]),
            "isn't a producer's name",
        ),
        (
            producers_file(&[(" a", &key, &["x"])]),
            "isn't a producer's name",
        ),
        (
            producers_file(&[("a", &key, &["x"]), ("b", &key, &["y"])]),
            "its key is a's already",
        ),
        (
            json!([{"name": "a", "key": "AAAA", "checks": ["x"]}])
                .to_string()
                .into_bytes(),
            "32 bytes",
        ),
        (
            json!([{"name": "a", "key": URL_SAFE_NO_PAD.encode([0u8; 32]), "checks": ["x"]}])
                .to_string()
                .into_bytes(),
            "weak",
        ),
        (
            json!([{"name": "a", "key": encoded, "checks": ["x"], "repos": ["o/r"]}])
                .to_string()
                .into_bytes(),
            "don't read",
        ),
        (b"{}".to_vec(), "don't read"),
    ] {
        let why = Producers::parse(&file).unwrap_err();
        assert!(why.contains(says), "{says}: {why}");
    }
    let dir = tempfile::tempdir().unwrap();
    let why = Producers::load(&dir.path().join("missing.json")).unwrap_err();
    assert!(why.contains("missing.json"), "{why}");
}

#[test]
fn the_shared_verifier_counts_a_service_statement_by_itself_and_keeps_it_while_trusted() {
    let hub = Hub::new(REPO, COMMIT);
    let text = sign(&honest(), &producer_key());
    let trust = Trust {
        signer: SIGNER.parse().unwrap(),
        fresh: None,
        producers: producers(&["conary/review/*"]),
    };
    let counted = attested(
        &hub,
        &hub,
        &repo(),
        PUSH,
        &run(CHECK, "success", Some(text.clone())),
        &trust,
    )
    .unwrap()
    .unwrap();
    // Read from its check run alone: nothing of GitHub Actions is asked.
    let actions = || [Call::Job, Call::Artifacts, Call::Artifact, Call::Keys].map(|c| hub.count(c));
    assert_eq!(actions(), [0; 4]);
    assert!(trust.holds(&counted.attest));
    // Kept, it counts while the machine trusts its key for it, and no more.
    let rotated = Trust {
        producers: producers(&["conary/proof/*"]),
        ..trust.clone()
    };
    assert!(!rotated.holds(&counted.attest));
    let gone = Trust {
        producers: Producers::default(),
        ..trust.clone()
    };
    assert!(!gone.holds(&counted.attest));
    let Attest::Service {
        statement,
        key,
        signature,
    } = counted.attest
    else {
        unreachable!()
    };
    let altered = Attest::Service {
        statement: statement.replacen("pass", "fail", 1),
        key,
        signature,
    };
    assert!(!trust.holds(&altered));
    // An output that says it's a service statement is one, and nothing
    // else: a bad one is refused, never read as CI's.
    let why = attested(
        &hub,
        &hub,
        &repo(),
        PUSH,
        &run(CHECK, "success", Some("conary service v1\n".into())),
        &trust,
    )
    .unwrap()
    .unwrap_err();
    assert!(why.contains("statement and then a signature"), "{why}");
    assert_eq!(actions(), [0; 4]);
}

#[test]
fn a_statement_counts_only_for_the_check_definition_the_machine_selects() {
    let (now, before) = ("d".repeat(64), "e".repeat(64));
    let defined = Subject::Commit {
        commit: COMMIT,
        tree: TREE,
        definition: Some(&now),
    };
    let checked_as = |text: &str, subject| {
        check(
            text,
            &producers(&["conary/review/*"]),
            &repo(),
            subject,
            &run(CHECK, "success", Some(text.into())),
        )
    };
    checked_as(&signed(|s| s.definition = Some(now.clone())), defined).unwrap();
    for (text, subject, says) in [
        (
            signed(|s| s.definition = Some(before.clone())),
            defined,
            format!("defined by {before}, and the machine defines it by {now}"),
        ),
        (
            signed(|_| {}),
            defined,
            format!("defined by nothing, and the machine defines it by {now}"),
        ),
        (
            signed(|s| s.definition = Some(now.clone())),
            PUSH,
            format!("defined by {now}, and the machine defines it by nothing"),
        ),
    ] {
        let why = checked_as(&text, subject).unwrap_err();
        assert!(why.contains(&says), "{says}: {why}");
    }
    // Signed, the definition can't be swapped for the one selected.
    let text = signed(|s| s.definition = Some(before.clone()));
    let why = checked_as(&text.replacen(&before, &now, 1), defined).unwrap_err();
    assert!(why.contains("signature doesn't verify"), "{why}");
    // Nor left out: a statement says it, null included.
    let key = producer_key();
    let mut v = serde_json::to_value(honest()).unwrap();
    v.as_object_mut().unwrap().remove("definition");
    let statement = format!("{HEADER}\n{v}\n");
    let text = format!(
        "{statement}{SCHEME} {} {}\n",
        URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
        URL_SAFE_NO_PAD.encode(key.sign(statement.as_bytes()).to_bytes())
    );
    let why = checked(&text).unwrap_err();
    assert!(why.contains("missing field `definition`"), "{why}");
    // A negative is the machine's named check, with no recipe of its own.
    let mut s = service_statement(REPO, COMMIT, TREE, "negative", &log());
    s.revision.base = Some(BASE.into());
    s.result = "fail".into();
    s.definition = Some(now.clone());
    let text = sign(&s, &producer_key());
    let why = check(
        &text,
        &producers(&["negative"]),
        &repo(),
        Subject::Negative {
            base: BASE,
            head: COMMIT,
        },
        &run("negative", "success", Some(text.clone())),
    )
    .unwrap_err();
    assert!(why.contains("defines no recipe for it"), "{why}");
}
