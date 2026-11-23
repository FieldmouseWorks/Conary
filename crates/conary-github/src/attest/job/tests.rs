use std::sync::LazyLock;

use serde_json::{Value, json};

use super::*;
use crate::attest::ISSUER;
use crate::attest::testing::{SIGNER, SIGNER_SHA, signer, token, token_with};
use crate::sim::{Call, FRESH, Hub};

const AUDIENCE_ASKED: &str = "burrow-builds:v1:sha256:abab";
/// The time the tests check at, in seconds since the Unix epoch.
const NOW: i64 = 1_800_000_000;
const PUSHED: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
const JOB: u64 = 41;

/// The claims GitHub gives a default-branch push's job in a public repo,
/// through the signer workflow at its commit on its default branch, on one
/// of GitHub's own runners, asking for the door's audience.
fn claims() -> Value {
    json!({
        "iss": ISSUER,
        "aud": AUDIENCE_ASKED,
        "exp": NOW + 300,
        "nbf": NOW - 10,
        "repository": "FieldmouseWorks/burrow",
        "repository_visibility": "public",
        "event_name": "push",
        "ref": "refs/heads/main",
        "sha": PUSHED,
        "check_run_id": JOB.to_string(),
        "job_workflow_ref": format!("{SIGNER}@refs/heads/main"),
        "job_workflow_sha": SIGNER_SHA,
        "runner_environment": "github-hosted",
    })
}

/// The repos the door lists.
static REPOS: LazyLock<Vec<Repo>> = LazyLock::new(|| {
    ["FieldmouseWorks/burrow", "FieldmouseWorks/wend", "Acme/ci"]
        .map(|r| r.parse().unwrap())
        .to_vec()
});
static SIGNED_BY: LazyLock<Signer> = LazyLock::new(signer);

fn door<'a>(audience: &'a str, fresh: Option<&'a str>) -> Door<'a> {
    Door {
        audience,
        repos: &REPOS,
        signer: &SIGNED_BY,
        fresh,
    }
}

fn hub() -> Hub {
    Hub::new("FieldmouseWorks/burrow", PUSHED)
}

/// `claims` changed by `change`, checked by the machine's door, which names
/// its fresh runners as the sim's launcher does.
fn checked_on(hub: &Hub, change: impl FnOnce(&mut Value)) -> Result<Admitted, String> {
    let mut c = claims();
    change(&mut c);
    admitted(
        hub,
        hub,
        &token(&c),
        &door(AUDIENCE_ASKED, Some(FRESH)),
        NOW,
    )
    .unwrap()
}

fn checked(change: impl FnOnce(&mut Value)) -> Result<Admitted, String> {
    checked_on(&hub(), change)
}

fn refused(change: impl FnOnce(&mut Value), says: &str) {
    let why = checked(change).unwrap_err();
    assert!(why.contains(says), "{why}");
}

#[test]
fn a_default_branch_push_on_a_hosted_runner_is_admitted_without_asking_for_its_job() {
    let hub = hub();
    let got = checked_on(&hub, |_| {}).unwrap();
    assert_eq!(
        got,
        Admitted {
            repository: "FieldmouseWorks/burrow".into(),
            visibility: "public".into(),
            event: "push".into(),
            git_ref: "refs/heads/main".into(),
            sha: PUSHED.into(),
            workflow_sha: SIGNER_SHA.into(),
            runner: None,
        }
    );
    // GitHub's word that its own runner took the job is in the token.
    assert_eq!(hub.count(Call::Job), 0);
}

#[test]
fn a_train_candidate_and_a_prs_run_through_the_signers_pinned_copy_are_admitted() {
    // A train candidate's push, on one of the machine's fresh runners.
    let hub = hub();
    let got = checked_on(&hub, |c| {
        c["ref"] = json!("refs/heads/conary/train/1");
        c["runner_environment"] = json!("self-hosted");
    })
    .unwrap();
    assert_eq!(got.runner.as_deref(), Some(&*format!("{FRESH}{JOB}")));
    assert_eq!(hub.count(Call::Job), 1);
    // Another repo's PR, calling the signer workflow pinned to its commit.
    let got = checked(|c| {
        c["repository"] = json!("FieldmouseWorks/wend");
        c["event_name"] = json!("pull_request");
        c["ref"] = json!("refs/pull/7/merge");
        c["job_workflow_ref"] = json!(format!("{SIGNER}@{SIGNER_SHA}"));
    })
    .unwrap();
    assert_eq!(
        (got.repository.as_str(), got.event.as_str()),
        ("FieldmouseWorks/wend", "pull_request")
    );
}

#[test]
fn a_token_github_didnt_sign_is_refused() {
    let hub = hub();
    let other = token_with(
        &json!({"alg": "RS256", "kid": "not-githubs", "typ": "JWT"}),
        &claims(),
    );
    let why = admitted(&hub, &hub, &other, &door(AUDIENCE_ASKED, None), NOW)
        .unwrap()
        .unwrap_err();
    assert!(why.contains("isn't one GitHub publishes"), "{why}");
    // Its claims changed after GitHub signed them.
    let honest = token(&claims());
    let mut parts: Vec<&str> = honest.split('.').collect();
    let mut forged = claims();
    forged["repository"] = json!("FieldmouseWorks/elsewhere");
    let body = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        forged.to_string(),
    );
    parts[1] = &body;
    let why = admitted(
        &hub,
        &hub,
        &parts.join("."),
        &door(AUDIENCE_ASKED, None),
        NOW,
    )
    .unwrap()
    .unwrap_err();
    assert!(why.contains("signature doesn't verify"), "{why}");
    // Another issuer's, under GitHub's key.
    refused(
        |c| c["iss"] = json!("https://example.com"),
        "issuer is \"https://example.com\"",
    );
}

#[test]
fn a_signer_commit_off_its_default_branch_or_a_prs_own_copy_is_refused() {
    // The signer's default branch moves on, leaving the token's commit on
    // another branch: GitHub knows it, but not on main.
    let hub = hub();
    hub.set_signer(&[&"e".repeat(40)]);
    let why = checked_on(&hub, |c| {
        c["job_workflow_ref"] = json!(format!("{SIGNER}@refs/heads/mine"));
    })
    .unwrap_err();
    assert!(
        why.contains("which isn't on Acme/ci's default branch"),
        "{why}"
    );
    // A PR of the signer's repo, running the copy of the workflow it
    // changes: its commit is GitHub's merge, on no branch.
    let why = checked_on(&hub, |c| {
        c["repository"] = json!("Acme/ci");
        c["event_name"] = json!("pull_request");
        c["job_workflow_ref"] = json!(format!("{SIGNER}@refs/pull/3/merge"));
    })
    .unwrap_err();
    assert!(
        why.contains("which isn't on Acme/ci's default branch"),
        "{why}"
    );
    // A workflow that isn't the signer's.
    refused(
        |c| {
            c["job_workflow_ref"] =
                json!("FieldmouseWorks/burrow/.github/workflows/mine.yml@refs/heads/main")
        },
        "asked for by",
    );
    refused(
        |c| c["job_workflow_sha"] = json!("main"),
        "no signer commit",
    );
}

#[test]
fn a_runner_neither_hosted_nor_fresh_is_refused() {
    let hub = hub();
    hub.ran_on(JOB, "remi-ci-1");
    let why = checked_on(&hub, |c| c["runner_environment"] = json!("self-hosted")).unwrap_err();
    assert!(
        why.contains("it ran on remi-ci-1, which wasn't started fresh"),
        "{why}"
    );
    // Without fresh runners named, only GitHub's own count.
    let hub = self::hub();
    let mut c = claims();
    c["runner_environment"] = json!("self-hosted");
    let why = admitted(&hub, &hub, &token(&c), &door(AUDIENCE_ASKED, None), NOW)
        .unwrap()
        .unwrap_err();
    assert!(why.contains("wasn't started fresh"), "{why}");
    // A self-hosted job its token doesn't name can't be looked up.
    refused(
        |c| {
            c["runner_environment"] = json!("self-hosted");
            c.as_object_mut().unwrap().remove("check_run_id");
        },
        "names no job",
    );
}

#[test]
fn a_token_for_another_audience_or_an_attestations_is_refused() {
    refused(
        |c| c["aud"] = json!("burrow-builds:v1:sha256:cdcd"),
        "audience isn't burrow-builds:v1:sha256:abab",
    );
    refused(
        |c| c["aud"] = json!([AUDIENCE_ASKED, "another"]),
        "audience isn't",
    );
    // An attestation's token is checked only with its statement.
    let hub = hub();
    let audience = format!("{AUDIENCE}abab");
    let mut c = claims();
    c["aud"] = json!(audience);
    let why = admitted(&hub, &hub, &token(&c), &door(&audience, None), NOW)
        .unwrap()
        .unwrap_err();
    assert!(why.contains("is an attestation's audience"), "{why}");
    assert_eq!(hub.count(Call::Keys), 0);
}

#[test]
fn a_token_out_of_its_time_is_refused() {
    refused(|c| c["exp"] = json!(NOW - SKEW - 1), "expired at");
    refused(
        |c| _ = c.as_object_mut().unwrap().remove("exp"),
        "has no expiry",
    );
    refused(|c| c["nbf"] = json!(NOW + SKEW + 1), "isn't good until");
    // Within the skew either way, it holds.
    checked(|c| {
        c["exp"] = json!(NOW - SKEW);
        c["nbf"] = json!(NOW + SKEW);
    })
    .unwrap();
}

#[test]
fn a_run_of_a_repo_the_door_doesnt_list_is_refused() {
    // Anyone's repo can call the signer workflow at its pinned commit.
    let hub = hub();
    let why = checked_on(&hub, |c| c["repository"] = json!("Stranger/fork")).unwrap_err();
    assert!(
        why.contains("\"Stranger/fork\"'s, which isn't listed"),
        "{why}"
    );
    assert_eq!(hub.count(Call::Job), 0);
    // GitHub's spelling of a listed repo isn't the only one.
    checked(|c| c["repository"] = json!("fieldmouseworks/BURROW")).unwrap();
}
