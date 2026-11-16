//! The configuration's published schema (`flow::schema`), and the options it
//! says are live taking effect at the next decision that reads them.
use conary_github::events::{Live, Queue, Receiver, Runtime};
use conary_github::flow::schema::{self, From};
use conary_github::flow::{Config, Settings};
use ring::hmac;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// Settings with one more field, as a change adding one would leave them.
mod grown {
    use super::Settings;
    use serde::{Deserialize, Serialize};

    /// A field with no entry: no description, and no say on spending.
    #[derive(Default, Serialize, Deserialize, schemars::JsonSchema)]
    #[serde(default)]
    pub struct Unsaid {
        #[serde(flatten)]
        pub settings: Settings,
        pub drain: bool,
    }

    /// A described spender that starts on.
    #[derive(Serialize, Deserialize, schemars::JsonSchema)]
    #[serde(default)]
    pub struct OnByDefault {
        #[serde(flatten)]
        pub settings: Settings,
        /// How many drain jobs may run at once.
        #[schemars(extend("x-spender" = true))]
        pub drain: u32,
    }
    impl Default for OnByDefault {
        fn default() -> Self {
            Self {
                settings: Settings::default(),
                drain: 2,
            }
        }
    }

    /// The same spender, off until a machine turns it on.
    #[derive(Default, Serialize, Deserialize, schemars::JsonSchema)]
    #[serde(default)]
    pub struct OffByDefault {
        #[serde(flatten)]
        pub settings: Settings,
        /// How many drain jobs may run at once.
        #[schemars(extend("x-spender" = true))]
        pub drain: u32,
    }

    /// One that needs a restart and doesn't say why.
    #[derive(Default, Serialize, Deserialize, schemars::JsonSchema)]
    #[serde(default)]
    pub struct Unexplained {
        #[serde(flatten)]
        pub settings: Settings,
        /// Where the drain listens.
        #[schemars(extend("x-spender" = false, "x-live" = false))]
        pub drain_listen: String,
    }
}

#[test]
fn every_option_has_an_entry_and_one_without_fails() {
    let flow = schema::flow().unwrap();
    let events = schema::events().unwrap();
    let settings = &flow["$defs"]["Settings"]["properties"];
    assert_eq!(
        settings.as_object().unwrap().len(),
        serde_json::to_value(Settings::default())
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        "every field of Settings is in the schema",
    );
    for (name, option) in settings.as_object().unwrap() {
        assert!(option["description"].is_string(), "{name}");
        assert!(option["x-spender"].is_boolean(), "{name}");
        assert!(option["x-live"].is_boolean(), "{name}");
        assert!(
            !option["description"].as_str().unwrap().contains('\n'),
            "{name}: one line"
        );
    }
    assert_eq!(
        settings["proof_slots"]["default"],
        json!(0),
        "the default is the loader's"
    );
    assert!(events["properties"]["listen"]["x-needs"].is_string());

    let unsaid = schema::of::<grown::Unsaid>().unwrap_err().to_string();
    assert!(unsaid.contains("drain has no description"), "{unsaid}");
    assert!(
        unsaid.contains("drain doesn't say whether it starts paid work"),
        "{unsaid}"
    );
    let unexplained = schema::of::<grown::Unexplained>().unwrap_err().to_string();
    assert!(
        unexplained.contains("drain_listen isn't live, so it must say what a change needs"),
        "{unexplained}"
    );
}

#[test]
fn every_spender_defaults_off_and_a_new_one_that_starts_on_fails() {
    let mut spenders = Vec::new();
    let flow = schema::flow().unwrap();
    let events = schema::events().unwrap();
    for schema in [&flow, &events] {
        let structs = std::iter::once(("", schema))
            .chain(
                schema["$defs"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, def)| (name.as_str(), def)),
            )
            .filter_map(|(name, s)| Some((name, s["properties"].as_object()?)));
        for (owner, properties) in structs {
            for (name, option) in properties {
                if option["x-spender"] == json!(true) {
                    assert!(
                        [json!(false), json!(0), json!(null), json!({})]
                            .contains(&option["default"]),
                        "{owner}.{name} defaults to {}",
                        option["default"]
                    );
                    spenders.push(format!("{owner}.{name}"));
                }
            }
        }
    }
    spenders.sort();
    assert_eq!(
        spenders,
        [
            "Plugins.fix",
            "Plugins.judge",
            "Plugins.proof",
            "Plugins.review",
            "Settings.proof_slots",
            "Settings.proofs",
            "Settings.renovate_sweep",
            "Settings.train",
        ]
    );

    let on = schema::of::<grown::OnByDefault>().unwrap_err().to_string();
    assert!(
        on.contains("drain starts paid work, so it must default off or zero"),
        "{on}"
    );
    let off = schema::of::<grown::OffByDefault>().unwrap();
    assert_eq!(off["properties"]["drain"]["default"], json!(0));
}

fn sign(body: &[u8]) -> String {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, b"s"), body);
    let hex: String = tag.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}
fn events(dir: &Path, change: impl FnOnce(&mut Value)) {
    let mut config = json!({
        "listen": "127.0.0.1:8090",
        "secret_path": dir.join("secret"),
        "repos": ["o/s"],
        "data": dir.join("data"),
        "flow_config": dir.join("flow.json"),
        "policy": dir.join("policy.json"),
        "token_command": "true",
        "anchor_repo": null,
        "worker": "/nonexistent",
        "api": "http://127.0.0.1:1",
    });
    change(&mut config);
    std::fs::write(dir.join("events.json"), config.to_string()).unwrap();
}

/// conaryd took its repositories once, as it started. Now a change to the
/// list, which the schema says is live, is acted on at the next delivery
/// and the next pass, as a change to the flow's own options is.
#[test]
fn a_live_option_changed_between_passes_is_acted_on() {
    let dir = tempfile::tempdir().unwrap();
    events(dir.path(), |_| {});
    let live = Arc::new(Live::open(&dir.path().join("events.json")).unwrap());
    let queue = Arc::new(Queue::open(&live.started().data).unwrap());
    let listed = Arc::clone(&live);
    let mut receiver = Receiver::open(
        b"s".to_vec(),
        Box::new(move || listed.repos()),
        &live.started().data,
    )
    .unwrap();
    let body = br#"{"repository":{"full_name":"o/r"},"pull_request":{"number":3}}"#;
    let deliver = |receiver: &mut Receiver, id: &str| {
        receiver.receive(Some(&sign(body)), id, "pull_request", body, 1, &queue)
    };
    assert_eq!(deliver(&mut receiver, "1"), 202);
    assert!(queue.take(1).unwrap().is_none(), "o/r isn't listed yet");
    events(dir.path(), |c| c["repos"] = json!(["o/s", "o/r"]));
    assert_eq!(deliver(&mut receiver, "2"), 202);
    let work = queue.take(1).unwrap().expect("the next delivery kicks o/r");
    assert_eq!(work.repo, "o/r");

    // Its pass reads the flow's options as they are now.
    let ledger = dir.path().join("ledger");
    conary_core::log::load_or_create_key(&conary_core::log::key_path(&ledger)).unwrap();
    std::fs::write(dir.path().join("policy.json"), b"{}").unwrap();
    std::fs::write(
        dir.path().join("flow.json"),
        json!({"settings": {"train": true}}).to_string(),
    )
    .unwrap();
    let trained = live.work(&ledger, &work, &queue).unwrap_err().to_string();
    assert!(trained.contains("require anchor_repo"), "{trained}");
    std::fs::write(
        dir.path().join("flow.json"),
        json!({"settings": {"train": false}}).to_string(),
    )
    .unwrap();
    let untrained = live.work(&ledger, &work, &queue).unwrap_err().to_string();
    assert!(!untrained.contains("anchor_repo"), "{untrained}");

    // Taken off the list, its mailbox retires without a pass.
    events(dir.path(), |c| c["repos"] = json!(["o/s"]));
    assert!(live.work(&ledger, &work, &queue).unwrap().is_empty());
}

/// Each option of conaryd's events configuration changes what a running
/// daemon reads at its next pass exactly when the schema says it's live.
/// One the schema says isn't keeps the value the daemon started with, and
/// waits, named, for a restart.
#[test]
fn only_a_restart_only_option_keeps_its_started_value() {
    let dir = tempfile::tempdir().unwrap();
    events(dir.path(), |_| {});
    let live = Live::open(&dir.path().join("events.json")).unwrap();
    let started = serde_json::to_value(live.started()).unwrap();
    let schema = schema::events().unwrap();
    let options = schema["properties"].as_object().unwrap();
    for (name, option) in options {
        let other = match name.as_str() {
            "listen" => json!("127.0.0.1:1"),
            "repos" => json!(["x/y"]),
            "anchor_repo" => json!("x/y"),
            "token_command" => json!("other"),
            "api" => json!("http://other"),
            "secret_path" | "data" | "flow_config" | "policy" | "worker" => json!("/other"),
            _ => panic!("{name}: give it another value here"),
        };
        events(dir.path(), |c| c[name] = other.clone());
        let read = serde_json::to_value(live.read().unwrap()).unwrap();
        if option["x-live"] == json!(true) {
            assert_eq!(read[name], other, "{name} is live");
            assert!(live.waiting().is_empty(), "{name}");
        } else {
            assert_eq!(read[name], started[name], "{name} waits for a restart");
            assert_eq!(live.waiting(), std::slice::from_ref(name));
        }
    }
    let restart: Vec<_> = options
        .iter()
        .filter(|(_, o)| o["x-live"] == json!(false))
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(restart, ["data", "listen", "secret_path"]);
}

#[test]
fn settings_list_each_value_its_source_and_the_exceptions_apart() {
    let dir = tempfile::tempdir().unwrap();
    events(dir.path(), |c| {
        c.as_object_mut().unwrap().remove("api");
    });
    let listing =
        schema::effective::<Runtime>(&std::fs::read(dir.path().join("events.json")).unwrap())
            .unwrap();
    let exceptions: Vec<_> = listing.not_live.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(exceptions, ["data", "listen", "secret_path"]);
    assert!(listing.not_live.iter().all(|o| {
        o.needs
            .as_deref()
            .unwrap()
            .starts_with("a restart of conaryd")
    }));
    let api = listing.options.iter().find(|o| o.name == "api").unwrap();
    assert_eq!(
        (api.value.clone(), api.from),
        (json!("https://api.github.com"), From::Default)
    );
    assert!(
        listing
            .options
            .iter()
            .all(|o| o.name == "api" || o.from == From::File && o.needs.is_none())
    );

    let key = "0".repeat(64);
    let flow =
        json!({"settings": {"proof_slots": 2, "judge_key": key}, "plugins": {"review": "/r"}});
    let listing = schema::effective::<Config>(flow.to_string().as_bytes()).unwrap();
    assert!(listing.not_live.is_empty());
    let get = |name: &str| {
        let o = listing.options.iter().find(|o| o.name == name).unwrap();
        (o.value.clone(), o.from)
    };
    assert_eq!(get("settings.proof_slots"), (json!(2), From::File));
    assert_eq!(get("settings.train"), (json!(false), From::Default));
    assert_eq!(get("plugins.review"), (json!("/r"), From::File));
    assert_eq!(get("plugins.fix"), (json!(null), From::Default));
    assert_eq!(get("evidence"), (json!(null), From::Default));
    assert_eq!(get("settings.judge_key"), (json!("[redacted]"), From::File));
    assert!(!serde_json::to_string(&listing).unwrap().contains(&key));
    // The loader refuses what it doesn't know, and so does the listing.
    assert!(schema::effective::<Config>(br#"{"settings": {"drain": 1}}"#).is_err());
}
