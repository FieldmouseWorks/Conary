use serde_json::{Value, json};

use super::testing::{KID, SIGNER_SHA, claims, file, key, statement, token, token_with, zipped};
use super::*;
use crate::api::CheckSuite;

const COMMIT: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
const TREE: &str = "7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e";
const RUN: u64 = 41;

fn repo() -> Repo {
    "FieldmouseWorks/remi".parse().unwrap()
}

fn run(conclusion: &str) -> CheckRun {
    CheckRun {
        id: RUN,
        name: "check / check".into(),
        status: "completed".into(),
        conclusion: Some(conclusion.into()),
        html_url: Some(format!(
            "https://github.com/FieldmouseWorks/remi/actions/runs/9/job/{RUN}"
        )),
        started_at: None,
        check_suite: Some(CheckSuite { id: 3 }),
    }
}

/// The job's statement and its claims, as GitHub would give them for a
/// pass on the candidate.
fn honest() -> (String, Value) {
    let s = statement(COMMIT, TREE, "pass");
    let c = claims("FieldmouseWorks/remi", COMMIT, RUN, &s);
    (s, c)
}

/// Checks a file as the candidate's passing check run's attestation.
fn checked(file: &str) -> Result<Claimed, String> {
    check(
        file.as_bytes(),
        &[key()],
        &repo(),
        COMMIT,
        TREE,
        &run("success"),
    )
}

/// The honest attestation, with `change` made to its claims first: the
/// reason it's refused, if it is.
fn with_claims(change: impl FnOnce(&mut Value)) -> Result<Claimed, String> {
    let (s, mut c) = honest();
    change(&mut c);
    checked(&file(&s, &token(&c)))
}

#[test]
fn the_jobs_own_attestation_counts_and_is_kept_as_it_was_checked() {
    let (s, c) = honest();
    let t = token(&c);
    let claimed = checked(&file(&s, &t)).unwrap();
    assert_eq!(claimed.workflow_sha, SIGNER_SHA);
    assert!(!claimed.hosted);
    assert_eq!(
        claimed.attest,
        Attest::GithubToken {
            statement: s,
            token: t
        }
    );
    // GitHub gives a token's ids as strings, but a number is the same id.
    with_claims(|c| c["check_run_id"] = json!(RUN)).unwrap();
    // An audience is a string or a list of them; one holding just this is
    // this one.
    with_claims(|c| c["aud"] = json!([c["aud"].clone()])).unwrap();
    // GitHub's spelling of the signer's repo isn't the only one.
    with_claims(|c| {
        c["job_workflow_ref"] = json!(format!(
            "fieldmouseworks/BURROW/{SIGNER_WORKFLOW}@refs/heads/main"
        ))
    })
    .unwrap();
    with_claims(|c| c["repository"] = json!("fieldmouseworks/remi")).unwrap();
    // Only GitHub's word that its own runner ran the job says it was hosted.
    assert!(
        with_claims(|c| c["runner_environment"] = json!("github-hosted"))
            .unwrap()
            .hosted
    );
    for other in [json!("self-hosted"), json!("GitHub-Hosted"), Value::Null] {
        assert!(
            !with_claims(|c| c["runner_environment"] = other)
                .unwrap()
                .hosted
        );
    }
}

#[test]
fn a_token_github_didnt_sign_doesnt_count() {
    let (s, c) = honest();
    let t = token(&c);
    // Its claims changed after it was signed.
    let (header, rest) = t.split_once('.').unwrap();
    let (_, signature) = rest.split_once('.').unwrap();
    let mut forged = c.clone();
    forged["sha"] = json!("d".repeat(40));
    let forged = format!(
        "{header}.{}.{signature}",
        URL_SAFE_NO_PAD.encode(forged.to_string())
    );
    let why = checked(&file(&s, &forged)).unwrap_err();
    assert!(why.contains("signature doesn't verify"), "{why}");
    // Signed by a key GitHub doesn't publish, or by one it names that isn't
    // an RSA key.
    for published in [
        Key {
            kid: "another".into(),
            ..key()
        },
        Key {
            kty: "EC".into(),
            ..key()
        },
    ] {
        let why = check(
            file(&s, &t).as_bytes(),
            &[published],
            &repo(),
            COMMIT,
            TREE,
            &run("success"),
        )
        .unwrap_err();
        assert!(why.contains("isn't one GitHub publishes"), "{why}");
    }
    // Signed with anything but RS256, or not at all.
    for alg in ["HS256", "none", "RS512"] {
        let t = token_with(&json!({"alg": alg, "kid": KID}), &c);
        let why = checked(&file(&s, &t)).unwrap_err();
        assert!(why.contains(&format!("signed with {alg}")), "{why}");
    }
    // Not a JWT.
    for t in ["a.b", "a.b.c.d", "%%%.%%%.%%%"] {
        assert!(checked(&file(&s, t)).is_err(), "{t}");
    }
}

#[test]
fn a_token_counts_only_for_its_own_statement_run_and_commit() {
    let refused = |change: fn(&mut Value), says: &str| {
        let why = with_claims(change).unwrap_err();
        assert!(why.contains(says), "{says}: {why}");
    };
    refused(|c| c["iss"] = json!("https://token.example"), "issuer");
    // An audience for another statement, or with another beside it.
    refused(
        |c| c["aud"] = json!(format!("{AUDIENCE}{}", "0".repeat(64))),
        "audience",
    );
    refused(
        |c| c["aud"] = json!([c["aud"].clone(), "sts.amazonaws.com"]),
        "audience",
    );
    refused(|c| c["aud"] = Value::Null, "audience");
    refused(
        |c| c["repository"] = json!("FieldmouseWorks/burrow"),
        "FieldmouseWorks/burrow's",
    );
    // Asked for by any workflow but burrow's shared CI: the repo's own,
    // which a candidate can change, or one that only starts like it.
    for workflow in [
        "FieldmouseWorks/remi/.github/workflows/flake.yml@refs/heads/conary/train/x",
        "FieldmouseWorks/burrow/.github/workflows/flake-check.yml.old@refs/heads/main",
        "FieldmouseWorks/burrow-fork/.github/workflows/flake-check.yml@refs/heads/main",
        "",
    ] {
        let why = with_claims(|c| c["job_workflow_ref"] = json!(workflow)).unwrap_err();
        assert!(why.contains("asked for by"), "{workflow}: {why}");
    }
    refused(|c| c["job_workflow_sha"] = json!("main"), "signer commit");
    refused(|c| c["check_run_id"] = json!("42"), "check run");
    refused(
        |c| {
            c.as_object_mut().unwrap().remove("check_run_id");
        },
        "check run",
    );
    refused(|c| c["sha"] = json!("d".repeat(40)), "its token is for");
}

#[test]
fn a_statement_counts_only_for_a_pass_on_the_candidates_tree_with_nothing_skipped() {
    // A statement that's changed, signed for as it is now: only what it
    // says decides.
    let says = |s: String, run: CheckRun| {
        let c = claims("FieldmouseWorks/remi", COMMIT, RUN, &s);
        check(
            file(&s, &token(&c)).as_bytes(),
            &[key()],
            &repo(),
            COMMIT,
            TREE,
            &run,
        )
    };
    let honest = statement(COMMIT, TREE, "pass");
    says(honest.clone(), run("success")).unwrap();
    let why = |s: String, run: CheckRun| says(s, run).unwrap_err();

    assert!(why(statement(&"d".repeat(40), TREE, "pass"), run("success")).contains("about"));
    assert!(why(statement(COMMIT, &"e".repeat(40), "pass"), run("success")).contains("tree"));
    // A failure attested is no pass, and a pass the check run doesn't
    // agree with doesn't count.
    assert!(why(statement(COMMIT, TREE, "fail"), run("success")).contains("says fail"));
    assert!(why(honest.clone(), run("failure")).contains("check run failure"));
    let skipped = honest.replace(r#""skipped":[]"#, r#""skipped":["agents"]"#);
    assert_ne!(skipped, honest);
    assert!(why(skipped, run("success")).contains("skipped agents"));
    let pr = honest.replace(
        r#""revision":null"#,
        r#""revision":{"base":"b","head":"h"}"#,
    );
    assert_ne!(pr, honest);
    assert!(why(pr, run("success")).contains("revision"));
    // A PR's subject, a predicate over more than one line, a line more.
    let revision = honest.replace("conary commit v1", "conary revision v1");
    assert!(why(revision, run("success")).contains("subject"));
    let pretty = format!(
        "conary commit v1\n{COMMIT}\n{}\n",
        serde_json::to_string_pretty(&json!({"result": "pass", "revision": null,
                                            "tree": TREE, "skipped": []}))
        .unwrap()
    );
    assert!(why(pretty, run("success")).contains("subject"));
    assert!(why(format!("{honest}more\n"), run("success")).contains("subject"));
}

#[test]
fn a_file_is_its_statement_and_then_its_token() {
    let (s, c) = honest();
    let t = token(&c);
    // The token's line ends the file.
    assert!(checked(&format!("{s}{t}")).is_err());
    assert!(checked(&format!("{t}\n{s}")).is_err());
    assert!(checked("").is_err());
    assert!(
        check(
            &[0xff, b'\n', b'\n'],
            &[key()],
            &repo(),
            COMMIT,
            TREE,
            &run("success")
        )
        .is_err()
    );
    // As GitHub zips its artifact, and nothing else.
    let bytes = file(&s, &t).into_bytes();
    assert_eq!(unzip(&zipped(FILE, &bytes)).unwrap(), bytes);
    assert!(
        unzip(&zipped("flake.log", &bytes))
            .unwrap_err()
            .contains("no conary-attestation")
    );
    assert!(unzip(&bytes).unwrap_err().contains("not a zip"));
    let big = vec![b'a'; MOST as usize + 1];
    assert!(unzip(&zipped(FILE, &big)).unwrap_err().contains("past"));
}
