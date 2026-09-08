// apps/conary/tests/cli_progress.rs
//! Real terminal and pipe captures of the package progress adapters.

use conary::commands::progress::{AdoptProgress, InstallProgress, RemoveProgress, UpdateProgress};
use std::process::Command;
use std::time::Duration;

#[test]
fn progress_capture_child() {
    let Ok(scenario) = std::env::var("CONARY_PROGRESS_CAPTURE") else {
        return;
    };
    match scenario.as_str() {
        "single" => {
            let progress = InstallProgress::single("Installing");
            progress.set_status("Extracting fixture");
            std::thread::sleep(Duration::from_millis(250));
            progress.clear();
        }
        "zero" => {
            let progress = UpdateProgress::new(0);
            progress.set_status("Checking fixture");
            std::thread::sleep(Duration::from_millis(250));
            progress.clear();
        }
        "remove_error" => {
            let progress = RemoveProgress::new("fixture");
            std::thread::sleep(Duration::from_millis(250));
            drop(progress);
        }
        "adopt" => {
            let progress = AdoptProgress::single("Adopting");
            std::thread::sleep(Duration::from_millis(250));
            progress.finish("Adopted fixture");
        }
        _ => panic!("unknown capture scenario"),
    }
    println!("CAPTURE_COMPLETE");
    std::thread::sleep(Duration::from_millis(250));
}

fn capture(scenario: &str, tty: bool, no_color: bool) -> String {
    let exe = std::env::current_exe().unwrap();
    let mut command = if tty {
        let mut command = Command::new("script");
        // The executable path is passed as a positional shell argument, never shell code.
        command.args([
            "-qec",
            "exec \"$CONARY_PROGRESS_EXE\" --exact progress_capture_child --nocapture",
            "/dev/null",
        ]);
        command.env("CONARY_PROGRESS_EXE", &exe);
        command
    } else {
        let mut command = Command::new(exe);
        command.args(["--exact", "progress_capture_child", "--nocapture"]);
        command
    };
    command
        .env("CONARY_PROGRESS_CAPTURE", scenario)
        .env("TERM", "xterm");
    command.env_remove("NO_COLOR").env_remove("CLICOLOR_FORCE");
    if no_color {
        command.env("NO_COLOR", "1");
    }
    let output = command
        .output()
        .expect("capture progress (requires util-linux script)");
    assert!(output.status.success(), "{output:?}");
    let mut text = String::from_utf8(output.stdout).unwrap();
    text.push_str(&String::from_utf8(output.stderr).unwrap());
    text
}

#[test]
fn terminal_progress_has_no_phantom_bars_or_duplicate_completion() {
    for scenario in ["single", "zero", "remove_error", "adopt"] {
        let output = capture(scenario, true, false);
        assert!(
            output.contains('\x1b'),
            "no TTY rendering exercised: {output:?}"
        );
        assert!(!output.contains("0/0"), "{scenario}: {output:?}");
        assert!(
            !output.contains("duplicate completion"),
            "{scenario}: {output:?}"
        );
        let (_, after) = output.split_once("CAPTURE_COMPLETE").unwrap();
        assert!(
            !after.contains('\x1b'),
            "redraw after completion: {output:?}"
        );
    }
}

#[test]
fn pipes_and_no_color_terminals_have_no_live_redraw() {
    for (tty, no_color) in [(false, false), (false, true), (true, true)] {
        for scenario in ["single", "zero", "remove_error", "adopt"] {
            let output = capture(scenario, tty, no_color);
            assert!(!output.contains('\x1b'), "{scenario}: {output:?}");
            assert!(!output.contains("0/0"), "{scenario}: {output:?}");
            assert!(
                !output.contains("duplicate completion"),
                "{scenario}: {output:?}"
            );
        }
    }
}
