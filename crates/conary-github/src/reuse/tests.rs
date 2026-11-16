//! The lookup against the simulated GitHub, with a generic machine's
//! configuration: a repository `Acme/app` whose gate is the signer
//! workflow `Acme/ci/.github/workflows/gate.yml`.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::attest::testing::{
    self, SIGNER_SHA, claims, file, gate_statement, producer_key, producers, service_statement,
    token,
};
use crate::sim::{Call, FRESH, Hub};

const REPO: &str = "Acme/app";
const SIGNER_REPO: &str = "Acme/ci";
const WORKFLOW: &str = ".github/workflows/gate.yml";
const TRUNK: &str = "1111111111111111111111111111111111111111";
/// PR #7's head: its own patch on trunk.
const HEAD: &str = "00000000000000000000000000000000000000a7";
/// The push PR #7 landed as: another commit, holding what its head does.
const PUSH: &str = "00000000000000000000000000000000000000c1";
/// GitHub's merge of PR #7, which its runs check out and its token names.
const MERGE: &str = "00000000000000000000000000000000000000e7";
/// The check run's name: the caller's job, then burrow's.
const CHECK: &str = "flake / check";
const NOW: i64 = 1_800_000_000;

fn repo() -> Repo {
    REPO.parse().unwrap()
}

fn config() -> Config {
    Config {
        repos: vec![repo()],
        trust: Trust {
            signer: testing::signer(),
            fresh: Some(FRESH.into()),
            producers: attest::service::Producers::default(),
        },
        server: "https://github.com".into(),
    }
}

/// GitHub with PR #7 open at [`HEAD`] and landed as [`PUSH`], on trunk.
fn github() -> Hub {
    let hub = Hub::new(REPO, TRUNK);
    hub.pr(REPO, 7, HEAD);
    hub.change(PUSH, TRUNK, HEAD);
    hub.set_branch(REPO, "main", PUSH);
    hub.link(REPO, PUSH, &[7]);
    assert_ne!(PUSH, HEAD);
    assert_eq!(hub.tree_at(PUSH), hub.tree_at(HEAD));
    hub
}

/// The gate burrow's workflow names at `at`, a commit of the signer's
/// repo.
fn gate(hub: &Hub, at: &str) -> Value {
    json!({"inputs": {"build-skip": ["agents"], "eval-apart": true, "install-nix": true,
                      "keep-builds": false, "legs": 4, "runner": "ubuntu-latest"},
           "source": "tree",
           "workflow": {"commit": at, "file": WORKFLOW, "repository": SIGNER_REPO,
                        "tree": hub.tree_at(at)}})
}

/// Where a gate ran: on `commit`'s check run, as a push's (`base` none)
/// or a PR revision's on its merge base `base`, checking `tree`.
struct Ran<'a> {
    base: Option<&'a str>,
    commit: &'a str,
    tree: &'a str,
}

/// `gate` ran as `ran` says, passed and was attested by its job: its check
/// run's id. `claim` changes its token's claims first, and `statement` its
/// statement after signing.
fn passes_with(
    hub: &Hub,
    ran: Ran,
    gate: &Value,
    skipped: &[&str],
    claim: impl FnOnce(&mut Value),
    statement: impl FnOnce(String) -> String,
) -> u64 {
    let Ran { base, commit, tree } = ran;
    let run = hub.check_run(commit, CHECK, "success", None);
    let s = gate_statement(base, commit, tree, gate, skipped);
    let sha = if base.is_some() { MERGE } else { commit };
    let mut c = claims(REPO, sha, run, &s);
    c["job_workflow_sha"] = gate["workflow"]["commit"].clone();
    c["job_workflow_ref"] = json!(format!("{}@refs/heads/main", testing::SIGNER));
    claim(&mut c);
    hub.attest(run, &file(&statement(s), &token(&c)));
    run
}

/// PR #7's gate passed on its merge with trunk, attested.
fn pr_passes(hub: &Hub, gate: &Value) -> u64 {
    pr_passes_skipping(hub, gate, &["agents"])
}

fn pr_passes_skipping(hub: &Hub, gate: &Value, skipped: &[&str]) -> u64 {
    let tree = hub.tree_at(HEAD);
    passes_with(
        hub,
        Ran {
            base: Some(TRUNK),
            commit: HEAD,
            tree: &tree,
        },
        gate,
        skipped,
        |_| {},
        |s| s,
    )
}

fn hit(run: u64) -> Answer {
    Answer::Hit {
        run: format!(
            "https://github.com/{REPO}/actions/runs/{}/job/{run}",
            run * 10
        ),
    }
}

/// A request, as burrow's runner makes one: one line of JSON, keys sorted.
fn request(commit: &str, tree: &str, gate: &Value) -> Vec<u8> {
    let body = json!({"commit": commit, "contract": CONTRACT, "gate": gate,
                      "repository": REPO, "tree": tree});
    serde_json::to_vec(&body).unwrap()
}

/// The asking job's token's claims: the push's job, in the workflow its
/// gate names.
fn asker(body: &[u8], gate: &Value) -> Value {
    json!({
        "iss": attest::ISSUER,
        "aud": format!("{AUDIENCE}{}", attest::hex(&Sha256::digest(body))),
        "repository": REPO,
        "sha": PUSH,
        "ref": "refs/heads/main",
        "job_workflow_ref": format!(
            "{}/{}@refs/heads/main",
            gate["workflow"]["repository"].as_str().unwrap(),
            gate["workflow"]["file"].as_str().unwrap()
        ),
        "job_workflow_sha": gate["workflow"]["commit"],
        "iat": NOW - 5,
        "nbf": NOW - 5,
        "exp": NOW + 300,
    })
}

/// What the lookup answers `body`, asked with `claims`' token.
fn reply(hub: &Hub, ledger: Result<&Ledger, String>, body: &[u8], claims: &Value) -> Reply {
    let bearer = format!("Bearer {}", token(claims));
    answer(hub, hub, ledger, &config(), NOW, Some(&bearer), body)
}

/// What the lookup says of [`PUSH`] under `gate`, asked honestly.
fn ask_with(hub: &Hub, ledger: &Ledger, gate: &Value) -> Answer {
    let body = request(PUSH, &hub.tree_at(PUSH), gate);
    let r = reply(hub, Ok(ledger), &body, &asker(&body, gate));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["contract"], CONTRACT);
    assert_eq!(r.body["query"], attest::hex(&Sha256::digest(&body)));
    let why = || r.body["why"].as_str().unwrap().to_owned();
    match r.body["evidence"].as_str().unwrap() {
        "hit" => Answer::Hit {
            run: r.body["run"].as_str().unwrap().into(),
        },
        "miss" => Answer::Miss { why: why() },
        "unknown" => Answer::Unknown { why: why() },
        other => panic!("{other}"),
    }
}

fn ask(hub: &Hub, gate: &Value) -> Answer {
    ask_with(hub, &Ledger::default(), gate)
}

fn is_miss(a: &Answer) -> bool {
    matches!(a, Answer::Miss { .. })
}

fn is_unknown(a: &Answer) -> bool {
    matches!(a, Answer::Unknown { .. })
}

#[test]
fn a_prs_attested_pass_on_the_tree_a_push_lands_is_a_hit() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    let run = pr_passes(&hub, &g);
    assert_eq!(ask(&hub, &g), hit(run));
}

#[test]
fn the_pushs_own_earlier_attested_pass_is_a_hit() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    let tree = hub.tree_at(PUSH);
    let run = passes_with(
        &hub,
        Ran {
            base: None,
            commit: PUSH,
            tree: &tree,
        },
        &g,
        &["agents"],
        |_| {},
        |s| s,
    );
    assert_eq!(ask(&hub, &g), hit(run));
}

#[test]
fn a_train_candidates_attested_pass_on_the_tree_is_a_hit() {
    use crate::train::{self, Now};
    use conary_core::Policy;
    use conary_core::store::Store;

    let hub = Hub::new(REPO, TRUNK);
    let head = "0000000000000000000000000000000000000008";
    hub.pr(REPO, 8, head);
    let dir = tempfile::tempdir().unwrap();
    let lock = conary_core::log::Lock::take(dir.path()).unwrap();
    let mut store =
        Store::open(lock, ed25519_dalek::SigningKey::from_bytes(&[5; 32]), None).unwrap();
    let steward = crate::ingest::Config {
        reviewers: vec![crate::sim::STEWARD.into()],
        own_contexts: vec!["review-settled".into()],
    };
    let policy = Policy::parse(br#"{"r2": []}"#).unwrap();
    train::join(
        &mut store,
        &hub,
        &hub,
        &steward,
        &repo(),
        8,
        head,
        &[],
        &policy,
        None,
    )
    .unwrap();
    let settings = train::Settings {
        size: 8,
        require: vec![CHECK.into()],
        trust: config().trust,
    };
    let stepped = train::step(&mut store, &hub, &hub, &steward, &repo(), &settings).unwrap();
    let Now::Gating { candidate, .. } = stepped.now else {
        panic!("not gating: {:?}", stepped.now);
    };
    let ledger = store.ledger();
    let c = ledger.candidate(candidate).unwrap();
    let (commit, tree) = (c.commit.to_string(), c.tree.to_string());

    // The PR lands as a commit of the candidate's tree that GitHub links
    // to no PR, so only the train knows where its pass is.
    let push = "00000000000000000000000000000000000000c8";
    hub.change(push, TRUNK, head);
    assert_eq!(hub.tree_at(push), tree);
    assert_ne!(push, commit);
    let g = gate(&hub, SIGNER_SHA);
    let run = passes_with(
        &hub,
        Ran {
            base: None,
            commit: &commit,
            tree: &tree,
        },
        &g,
        &["agents"],
        |_| {},
        |s| s,
    );

    let body = request(push, &tree, &g);
    let mut claims = asker(&body, &g);
    claims["sha"] = json!(push);
    let r = reply(&hub, Ok(ledger), &body, &claims);
    assert_eq!(r.body["evidence"], "hit", "{}", r.body);
    assert_eq!(r.body["run"], json!(hit(run).run()));
    // Without the ledger's candidates, nothing says where to look.
    let r = reply(&hub, Ok(&Ledger::default()), &body, &claims);
    assert_eq!(r.body["evidence"], "miss", "{}", r.body);
}

impl Answer {
    fn run(&self) -> &str {
        match self {
            Answer::Hit { run } => run,
            other => panic!("not a hit: {other:?}"),
        }
    }
}

#[test]
fn a_pass_on_another_tree_is_a_miss() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    // Trunk moved before the PR's last run: it checked another merge.
    let other = crate::sim::tree_of(&[HEAD, "a-later-change"]);
    passes_with(
        &hub,
        Ran {
            base: Some(TRUNK),
            commit: HEAD,
            tree: &other,
        },
        &g,
        &["agents"],
        |_| {},
        |s| s,
    );
    let a = ask(&hub, &g);
    assert!(is_miss(&a), "{a:?}");
}

#[test]
fn a_changed_workflow_is_a_miss_and_the_same_files_at_another_commit_a_hit() {
    let hub = github();
    let old = gate(&hub, SIGNER_SHA);
    let run = pr_passes(&hub, &old);
    // The workflow changed on its default branch.
    let changed = "5151515151515151515151515151515151515152";
    hub.change(changed, SIGNER_SHA, "a-new-step");
    hub.set_branch(SIGNER_REPO, "main", changed);
    let a = ask(&hub, &gate(&hub, changed));
    assert!(is_miss(&a), "{a:?}");
    // A later commit of the same files: the same gate.
    let same = "5151515151515151515151515151515151515153";
    hub.take_out_as(same, changed, "a-new-step");
    hub.set_branch(SIGNER_REPO, "main", same);
    assert_eq!(hub.tree_at(same), hub.tree_at(SIGNER_SHA));
    assert_eq!(ask(&hub, &gate(&hub, same)), hit(run));
}

#[test]
fn a_gate_with_other_inputs_is_a_miss() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    pr_passes(&hub, &g);
    for (path, value) in [
        ("/inputs/legs", json!(8)),
        ("/inputs/runner", json!("self-hosted")),
        ("/inputs/build-skip", json!([])),
        ("/inputs/eval-apart", json!(false)),
        ("/inputs/install-nix", json!(false)),
        ("/inputs/keep-builds", json!(true)),
        ("/workflow/file", json!(".github/workflows/other.yml")),
        ("/workflow/repository", json!("Acme/other-ci")),
    ] {
        let mut other = g.clone();
        *other.pointer_mut(path).unwrap() = value;
        let a = ask(&hub, &other);
        assert!(is_miss(&a), "{path}: {a:?}");
    }
    let mut more = g.clone();
    more["inputs"]["extra"] = json!(1);
    assert!(is_miss(&ask(&hub, &more)));
    // A gate whose verdict reads more than the tree can't be matched by it.
    let mut by_commit = g.clone();
    by_commit["source"] = json!("commit");
    let a = ask(&hub, &by_commit);
    assert!(is_miss(&a), "{a:?}");
    assert_eq!(ask(&hub, &g), hit(hub.checks_on(HEAD)[0].id));
}

#[test]
fn a_pass_that_skipped_what_this_gate_builds_is_a_miss() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    pr_passes_skipping(&hub, &g, &["agents", "vm-tests"]);
    let a = ask(&hub, &g);
    assert!(is_miss(&a), "{a:?}");
    // Skipping less than the gate does is building more.
    let run = pr_passes_skipping(&hub, &g, &[]);
    assert_eq!(ask(&hub, &g), hit(run));
}

#[test]
fn unattested_forged_and_reused_passes_are_misses() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    let tree = hub.tree_at(HEAD);
    // A status, and a passed check run nothing attested.
    hub.post_status(
        PUSH,
        json!({"context": CHECK, "state": "success", "id": 1, "created_at": "2026-11-14T00:00:00Z"}),
    );
    hub.check_run(HEAD, CHECK, "success", None);
    // A push's run that reused a pass, saying so: it attests nothing.
    hub.check_run(
        PUSH,
        CHECK,
        "success",
        Some("gate-reuse: this gate already passed on this tree, attested, in https://github.com/Acme/app/actions/runs/1"),
    );
    // Its statement changed after it was signed: another tree, or a gate.
    passes_with(
        &hub,
        Ran {
            base: Some(TRUNK),
            commit: HEAD,
            tree: &tree,
        },
        &g,
        &["agents"],
        |_| {},
        |s| s.replace("\"tree\":\"", "\"tree\":\"0"),
    );
    passes_with(
        &hub,
        Ran {
            base: Some(TRUNK),
            commit: HEAD,
            tree: &tree,
        },
        &g,
        &["agents"],
        |_| {},
        |s| s.replace("\"legs\":4", "\"legs\":5"),
    );
    // A token that isn't GitHub's signature.
    let run = hub.check_run(HEAD, CHECK, "success", None);
    let s = gate_statement(Some(TRUNK), HEAD, &tree, &g, &["agents"]);
    let forged = token(&claims(REPO, MERGE, run, &s));
    let forged = format!("{}AAAA", &forged[..forged.len() - 4]);
    hub.attest(run, &file(&s, &forged));
    // An attested pass from before workflows named their gate.
    let run = hub.check_run(HEAD, CHECK, "success", None);
    let s = testing::statement(HEAD, &tree, "pass");
    hub.attest(run, &file(&s, &token(&claims(REPO, HEAD, run, &s))));
    // A failure, attested, on a check run that says it passed.
    let run = hub.check_run(HEAD, CHECK, "success", None);
    let s =
        gate_statement(Some(TRUNK), HEAD, &tree, &g, &["agents"]).replace("\"pass\"", "\"fail\"");
    let mut c = claims(REPO, MERGE, run, &s);
    c["job_workflow_sha"] = json!(SIGNER_SHA);
    hub.attest(run, &file(&s, &token(&c)));
    // A pass attested on a check run that failed.
    let run = hub.check_run(HEAD, CHECK, "failure", None);
    let s = gate_statement(Some(TRUNK), HEAD, &tree, &g, &["agents"]);
    hub.attest(run, &file(&s, &token(&claims(REPO, MERGE, run, &s))));

    let a = ask(&hub, &g);
    assert!(is_miss(&a), "{a:?}");
}

#[test]
fn passes_untrusted_producers_attest_are_misses() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    let tree = hub.tree_at(HEAD);
    let pass = |claim: &dyn Fn(&mut Value)| {
        passes_with(
            &hub,
            Ran {
                base: Some(TRUNK),
                commit: HEAD,
                tree: &tree,
            },
            &g,
            &["agents"],
            claim,
            |s| s,
        )
    };
    // The repository's own workflow, not the machine's signer.
    pass(&|c| c["job_workflow_ref"] = json!("Acme/app/.github/workflows/gate.yml@refs/heads/main"));
    // Another repository's job.
    pass(&|c| c["repository"] = json!("Acme/elsewhere"));
    // Another issuer.
    pass(&|c| c["iss"] = json!("https://example.com"));
    // The signer at a commit off its default branch: a branch's copy.
    let branch = "5151515151515151515151515151515151515159";
    hub.change(branch, SIGNER_SHA, "a-branch-edit");
    let off = gate(&hub, branch);
    passes_with(
        &hub,
        Ran {
            base: Some(TRUNK),
            commit: HEAD,
            tree: &tree,
        },
        &off,
        &["agents"],
        |_| {},
        |s| s,
    );
    // A gate naming another workflow commit than its token's, both on the
    // signer's default branch.
    let later = "5151515151515151515151515151515151515158";
    hub.set_signer(&[SIGNER_SHA, later]);
    pass(&|c| c["job_workflow_sha"] = json!(later));
    // A runner that wasn't started fresh for its job.
    let shared = pass(&|_| {});
    hub.ran_on(shared, "shared-runner");
    // A service's statement, by a producer the machine trusts: only CI
    // says a gate passed on a tree.
    let statement = service_statement(REPO, HEAD, &tree, CHECK, &"0".repeat(64));
    let text = attest::service::sign(&statement, &producer_key());
    hub.check_run(HEAD, CHECK, "success", Some(&text));

    let mut trusting = config();
    trusting.trust.producers = producers(&[CHECK]);
    assert_ne!(trusting.trust.producers, config().trust.producers);
    let body = request(PUSH, &tree, &g);
    let bearer = format!("Bearer {}", token(&asker(&body, &g)));
    for config in [config(), trusting] {
        let r = answer(
            &hub,
            &hub,
            Ok(&Ledger::default()),
            &config,
            NOW,
            Some(&bearer),
            &body,
        );
        assert_eq!(r.body["evidence"], "miss", "{}", r.body);
    }
}

#[test]
fn expired_or_unreadable_evidence_never_hits() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    let run = pr_passes(&hub, &g);
    // Past its retention, it's gone: no evidence, so a miss.
    hub.expire_artifacts(run);
    let a = ask(&hub, &g);
    assert!(is_miss(&a), "{a:?}");

    let hub = github();
    let run = pr_passes(&hub, &g);
    assert_eq!(ask(&hub, &g), hit(run));
    for call in [
        Call::CommitPulls,
        Call::Pull,
        Call::CheckRuns,
        Call::Job,
        Call::Artifacts,
        Call::Artifact,
        Call::Tree,
        Call::Branch,
    ] {
        hub.fail(call);
        let a = ask(&hub, &g);
        assert!(is_unknown(&a), "{call:?}: {a:?}");
        hub.heal(call);
    }
    // The ledger can't be read.
    let body = request(PUSH, &hub.tree_at(PUSH), &g);
    let r = reply(
        &hub,
        Err("it doesn't verify".into()),
        &body,
        &asker(&body, &g),
    );
    assert_eq!(r.body["evidence"], "unknown", "{}", r.body);
    // GitHub's keys can't be read: the requester can't be told apart.
    hub.fail(Call::Keys);
    let r = reply(&hub, Ok(&Ledger::default()), &body, &asker(&body, &g));
    assert_eq!(r.status, 503);
    assert!(r.body.get("evidence").is_none());
    hub.heal(Call::Keys);
    assert_eq!(ask(&hub, &g), hit(run));
}

#[test]
fn a_push_whose_tree_isnt_the_one_asked_about_is_unknown() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    pr_passes(&hub, &g);
    // The request says the push holds the PR's tree; GitHub says otherwise.
    let other = "00000000000000000000000000000000000000c2";
    hub.change(other, PUSH, "more");
    let body = request(other, &hub.tree_at(HEAD), &g);
    let mut claims = asker(&body, &g);
    claims["sha"] = json!(other);
    let r = reply(&hub, Ok(&Ledger::default()), &body, &claims);
    assert_eq!(r.body["evidence"], "unknown", "{}", r.body);
    // Its gate's workflow tree isn't its commit's.
    let mut lying = g.clone();
    lying["workflow"]["tree"] = json!(hub.tree_at(PUSH));
    let a = ask(&hub, &lying);
    assert!(is_unknown(&a), "{a:?}");
}

#[test]
fn only_the_asking_job_itself_is_answered() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    pr_passes(&hub, &g);
    let body = request(PUSH, &hub.tree_at(PUSH), &g);
    let honest = asker(&body, &g);
    let status = |claims: &Value| reply(&hub, Ok(&Ledger::default()), &body, claims).status;
    assert_eq!(status(&honest), 200);
    let changed = |f: &dyn Fn(&mut Value)| {
        let mut c = honest.clone();
        f(&mut c);
        status(&c)
    };
    // Another request's token, and one asked for with no audience.
    let other = request(PUSH, &hub.tree_at(PUSH), &gate(&hub, TRUNK));
    assert_eq!(
        changed(&|c| c["aud"] = asker(&other, &g)["aud"].clone()),
        401
    );
    assert_eq!(
        changed(&|c| _ = c.as_object_mut().unwrap().remove("aud")),
        401
    );
    assert_eq!(changed(&|c| c["iss"] = json!("https://example.com")), 401);
    assert_eq!(changed(&|c| c["repository"] = json!("Acme/elsewhere")), 401);
    assert_eq!(changed(&|c| c["sha"] = json!(HEAD)), 401);
    assert_eq!(
        changed(&|c| c["job_workflow_ref"] =
            json!("Acme/app/.github/workflows/gate.yml@refs/heads/main")),
        401
    );
    assert_eq!(changed(&|c| c["job_workflow_sha"] = json!(TRUNK)), 401);
    assert_eq!(changed(&|c| c["exp"] = json!(NOW - 120)), 401);
    assert_eq!(
        changed(&|c| _ = c.as_object_mut().unwrap().remove("exp")),
        401
    );
    assert_eq!(changed(&|c| c["nbf"] = json!(NOW + 120)), 401);
    // No token, another scheme, a token that isn't GitHub's.
    let none = answer(
        &hub,
        &hub,
        Ok(&Ledger::default()),
        &config(),
        NOW,
        None,
        &body,
    );
    assert_eq!(none.status, 401);
    let t = token(&honest);
    for auth in [
        format!("Basic {t}"),
        format!("Bearer {}x", t),
        "Bearer ".into(),
    ] {
        let r = answer(
            &hub,
            &hub,
            Ok(&Ledger::default()),
            &config(),
            NOW,
            Some(&auth),
            &body,
        );
        assert_eq!(r.status, 401, "{auth}");
    }
}

#[test]
fn a_request_that_doesnt_read_or_isnt_this_machines_is_refused() {
    let hub = github();
    let g = gate(&hub, SIGNER_SHA);
    let tree = hub.tree_at(PUSH);
    let status = |body: &[u8]| {
        let bearer = format!("Bearer {}", token(&asker(body, &g)));
        answer(
            &hub,
            &hub,
            Ok(&Ledger::default()),
            &config(),
            NOW,
            Some(&bearer),
            body,
        )
        .status
    };
    let with = |f: &dyn Fn(&mut Value)| {
        let mut v: Value = serde_json::from_slice(&request(PUSH, &tree, &g)).unwrap();
        f(&mut v);
        status(&serde_json::to_vec(&v).unwrap())
    };
    assert_eq!(with(&|_| {}), 200);
    assert_eq!(
        with(&|v| v["contract"] = json!("burrow-gate-reuse/v2")),
        400
    );
    assert_eq!(with(&|v| v["extra"] = json!(1)), 400);
    assert_eq!(with(&|v| v["tree"] = json!("main")), 400);
    assert_eq!(with(&|v| v["commit"] = json!(PUSH.to_uppercase())), 400);
    assert_eq!(with(&|v| v["gate"] = json!([])), 400);
    assert_eq!(
        with(&|v| _ = v["gate"].as_object_mut().unwrap().remove("workflow")),
        400
    );
    assert_eq!(with(&|v| v["gate"]["workflow"]["tree"] = json!(null)), 400);
    assert_eq!(
        with(&|v| v["gate"]["inputs"]["build-skip"] = json!("agents")),
        400
    );
    assert_eq!(with(&|v| v["repository"] = json!("Acme/other")), 403);
    assert_eq!(with(&|v| v["repository"] = json!("not a repo")), 400);
    assert_eq!(status(b"not json"), 400);
    assert_eq!(status(&vec![b' '; MAX_BODY + 1]), 400);
}

#[test]
fn the_machines_settings_read_into_a_config() {
    let settings: Settings = serde_json::from_value(json!({
        "listen": "127.0.0.1:8091",
        "repos": [REPO],
        "token_command": "/usr/local/bin/conary-token",
        "signer": testing::SIGNER,
    }))
    .unwrap();
    let c = settings.config().unwrap();
    assert_eq!(c.repos, [repo()]);
    assert_eq!(c.trust.signer, testing::signer());
    assert_eq!(c.trust.fresh, None);
    assert_eq!(c.trust.producers, attest::service::Producers::default());
    assert_eq!(c.server, "https://github.com");
    assert_eq!(settings.api, "https://api.github.com");
    let unknown = json!({"listen": "127.0.0.1:1", "repos": [], "token_command": "x",
                         "signer": testing::SIGNER, "producers": "/etc/keys"});
    assert!(serde_json::from_value::<Settings>(unknown).is_err());
    let mut bad = settings.clone();
    bad.signer = "Acme/ci/gate.yml".into();
    assert!(bad.config().is_err());
}
