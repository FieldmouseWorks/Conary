use super::*;
use conary_github::IngestError;
use conary_github::anchor::AnchorError;
use conary_github::undo::UndoError;

// Every query, and only a query, is asked at a time; asked so, it
// reads the ledger as it stood then.
#[test]
fn at_asks_any_query_and_only_a_query_as_the_ledger_stood_then() {
    use conary_core::op::{Actor, ActorKind, Body};
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().to_str().unwrap();
    let parse = |args: &[&str]| Cli::try_parse_from([&["cn", "--ledger", ledger], args].concat());
    for args in [
        &["at", "200", "log", "--repo", "o/r"][..],
        &["at", "200", "why", "f:1", "-C", "/"],
        &["at", "200", "train", "show", "o/r"],
        &["log"],
        &["why", "f:1"],
        &["train", "show", "o/r"],
    ] {
        assert!(parse(args).is_ok(), "{args:?}");
    }
    for args in [
        &["at", "200", "undo", "o/r#1"][..],
        &["at", "200", "sync", "o/r"],
        &["at", "200", "train", "step", "o/r"],
        &["at", "200", "at", "100", "log"],
        &["at", "yesterday", "log"],
        &["at", "200"],
    ] {
        assert!(parse(args).is_err(), "{args:?}");
    }

    let key = log::load_or_create_key(&log::key_path(dir.path())).unwrap();
    let lock = log::Lock::take(dir.path()).unwrap();
    log::save_anchor(&lock, None, "c0").unwrap();
    let mut store = conary_core::Store::open(lock, key, None).unwrap();
    for (pr, at) in [(1, 100), (2, 300)] {
        let author = Actor {
            kind: ActorKind::Person,
            login: "someone".into(),
            session: None,
        };
        let change = ChangeId::new().unwrap();
        let repo = "o/r".parse().unwrap();
        store
            .record_at(
                Body::Observe {
                    change,
                    repo,
                    pr,
                    author,
                },
                at,
            )
            .unwrap();
    }
    let cli = parse(&["at", "200", "log"]).unwrap();
    let prs = |at| -> Vec<u64> { read(&cli, at).unwrap().changes().map(|c| c.pr).collect() };
    assert_eq!(prs(Some(200)), [1]);
    assert_eq!(prs(None), [1, 2]);
}

// A policy file is checked as `cn class` reads it: one this cn doesn't
// know a key of is refused, so a machine's build of it fails.
#[test]
fn policy_checks_a_file_as_class_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let check = |body: &str| {
        let file = dir.path().join("policy.json");
        std::fs::write(&file, body).unwrap();
        run(&Cli::try_parse_from(["cn", "policy", file.to_str().unwrap()]).unwrap())
    };
    assert!(check(r#"{"r2": ["^secrets/"], "hidden": ["^docs/private/$"]}"#).is_ok());
    for bad in [r#"{"hiddn": []}"#, r#"{"r2": ["("]}"#, "not json"] {
        assert!(check(bad).is_err(), "{bad}");
    }
}

#[test]
fn help_never_shows_the_token_command() {
    use clap::CommandFactory;
    let cli = Cli::command();
    let args: Vec<_> = cli
        .get_subcommands()
        .flat_map(|c| c.get_arguments())
        .filter(|a| a.get_id() == "token_command")
        .collect();
    assert!(!args.is_empty());
    assert!(args.iter().all(|a| a.is_hide_env_values_set()));
}

// Which runners were started fresh is the machine's to say, for every
// cn it runs: a cn that doesn't count by it yet ignores it, where it
// would refuse a flag it doesn't know.
#[test]
fn the_fresh_runners_are_a_machine_setting() {
    use clap::CommandFactory;
    let cli = Cli::command();
    let step = cli
        .find_subcommand("train")
        .and_then(|t| t.find_subcommand("step"))
        .expect("cn train step");
    let fresh = step
        .get_arguments()
        .find(|a| a.get_long() == Some("fresh-runners"))
        .expect("--fresh-runners");
    assert_eq!(
        fresh.get_env().and_then(|e| e.to_str()),
        Some("CONARY_FRESH_RUNNERS")
    );
}

#[test]
fn the_token_command_then_gh_token_then_github_token_and_an_empty_one_is_none() {
    let token = |command, gh, github| {
        choose_tokens(command, gh, github)
            .map(|t| t.of("o/r").unwrap())
            .map_err(|e| e.to_string())
    };
    assert_eq!(
        token(Some("printf c-%s"), Some("g"), None),
        Ok("c-o/r".into())
    );
    assert_eq!(token(Some(" "), Some("g"), Some("h")), Ok("g".into()));
    assert_eq!(token(None, Some(""), Some("h")), Ok("h".into()));
    assert_eq!(token(None, Some("g"), Some("h")), Ok("g".into()));
    for (gh, github) in [(None, None), (Some(""), Some(" "))] {
        assert!(
            token(None, gh, github)
                .unwrap_err()
                .contains("set GH_TOKEN")
        );
    }
}

#[test]
fn notes_are_written_only_for_repos_the_ledger_keeps() {
    use conary_core::op::{Actor, ActorKind, Body};
    let dir = tempfile::tempdir().unwrap();
    let lock = log::Lock::take(dir.path()).unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let mut store = conary_core::Store::open(lock, key, None).unwrap();
    let kept: Repo = "o/kept".parse().unwrap();
    let change = ChangeId::new().unwrap();
    store
        .record(Body::Observe {
            change,
            repo: kept.clone(),
            pr: 5,
            author: Actor {
                kind: ActorKind::Agent,
                login: "a".into(),
                session: None,
            },
        })
        .unwrap();
    let stranger: Repo = "o/stranger".parse().unwrap();
    let done = Undid::default();
    let ledger = store.ledger();
    assert_eq!(
        noted(ledger, &done, Some(change), Some(&kept)),
        std::slice::from_ref(&kept)
    );
    assert!(noted(ledger, &done, None, Some(&stranger)).is_empty());
    assert_eq!(noted(ledger, &done, Some(change), Some(&stranger)), [kept]);
}

#[test]
fn a_change_is_named_by_its_id_or_its_pr() {
    let ledger = Ledger::default();
    let id = ChangeId::new().unwrap();
    assert_eq!(named(&ledger, &id.to_string()).unwrap(), id);
    let refused = |said: &str| named(&ledger, said).unwrap_err().to_string();
    assert!(refused("o/r#5").contains("o/r#5 isn't in the ledger"));
    assert!(refused("o#5").contains("as OWNER/NAME"));
    assert!(refused("o/r#five").contains("as a PR"));
    assert!(refused("five").contains("as a change id"));
}

#[test]
fn a_refusal_exits_apart_from_a_run_that_stopped() {
    let boxed = |e: Box<dyn std::error::Error>| (refused(&*e), unavailable(&*e));
    let id = ChangeId::new().unwrap();
    assert_eq!(
        boxed(Stopped::from(UndoError::NotLanded(id)).into()),
        (true, false)
    );
    assert_eq!(
        boxed(Unnamed("o/r#5 isn't in the ledger".into()).into()),
        (true, false)
    );
    assert_eq!(
        boxed(Stopped::from(UndoError::Api(ApiError::new("Server Error"))).into()),
        (false, false)
    );
    assert_eq!(
        boxed(Stopped::from(UndoError::Api(ApiError::unavailable("limit"))).into()),
        (false, true)
    );
    assert_eq!(boxed("no token".into()), (false, false));
    // Refused in a repo after an undo opened in another: it stopped.
    let mut stopped = Stopped::from(UndoError::NoTrunk("main".into()));
    stopped.done.undos.push(Undone {
        change: id,
        pr: 7,
        reverts: vec![id],
        left: Vec::new(),
        how: How::Opened { conflicts: false },
        recorded: 1,
    });
    assert_eq!(boxed(stopped.into()), (false, false));
}

#[test]
fn github_unavailable_is_told_through_whatever_wraps_it() {
    let boxed = |e: Box<dyn std::error::Error>| unavailable(&*e);
    assert!(boxed(ApiError::unavailable("limit").into()));
    assert!(boxed(
        IngestError::Api(ApiError::unavailable("limit")).into()
    ));
    assert!(boxed(
        AnchorError::Api(ApiError::unavailable("limit")).into()
    ));
    assert!(!boxed(IngestError::Api(ApiError::new("refused")).into()));
    assert!(!boxed(AnchorError::Lost("gone".into()).into()));
    assert!(!boxed("no token".into()));
}
