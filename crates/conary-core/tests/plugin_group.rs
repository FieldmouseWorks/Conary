//! A plugin the runner ends takes what it started with it: a descendant that
//! writes a marker a second later never gets to, whichever path ends it.
use conary_core::plugin::{Limits, Plugin};
use std::process::Command;
use std::time::{Duration, Instant};

/// Runs a plugin that starts the descendant, then `rest`, under `deadline`,
/// and expects the runner to end it with `why`.
fn ended(why: &str, rest: &str, deadline: u64) {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let cancel = dir.path().join("cancel");
    let plugin = dir.path().join("plugin");
    let script = format!(
        "#!/bin/sh\nMARKER='{}'\nCANCEL='{}'\nsh -c 'sleep 1; touch \"$0\"' \"$MARKER\" {rest}\n",
        marker.display(),
        cancel.display(),
    );
    // Written by a child, so no writer of the script is open here for a
    // parallel spawn to inherit (ETXTBSY).
    let wrote = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"printf %s "$1" > "$0" && chmod 755 "$0""#)
        .arg(&plugin)
        .arg(&script)
        .status()
        .unwrap();
    assert!(wrote.success());
    let limits = Limits {
        deadline: Duration::from_millis(deadline),
        response_bytes: 1000,
    };
    let failure = Plugin::describe_cancellable(&plugin, limits, Some(&cancel))
        .err()
        .expect("the runner ends the plugin");
    assert!(failure.0.contains(why), "{failure}");
    let until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < until {
        assert!(!marker.exists(), "a descendant outlived its plugin");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_plugin_past_its_deadline_takes_its_descendants_with_it() {
    ended("exceeded deadline", "", 300);
}

#[test]
fn a_cancelled_plugin_takes_its_descendants_with_it() {
    ended("cancelled", r#"& touch "$CANCEL"; wait"#, 10_000);
}

#[test]
fn a_plugin_answering_too_much_takes_its_descendants_with_it() {
    ended("too large", "& head -c 100000 /dev/zero; wait", 10_000);
}
