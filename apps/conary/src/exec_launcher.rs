// apps/conary/src/exec_launcher.rs
//! Argument handling for the `conary-exec` launcher binary.
//!
//! The launcher is single-threaded and opens no database: everything it
//! refuses comes from the filesystem and the kernel (see
//! `conary_core::launch`). CLI wiring (`conary exec --from <profile>`) belongs
//! to the source-root CLI cut-over and execs this binary.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;
use conary_core::launch::{
    EXIT_LAUNCHER_FAILURE, EtcSource, LaunchError, LaunchRequest, launch, probe_in_this_process,
};

#[derive(Debug, Parser)]
#[command(
    name = "conary-exec",
    version,
    about = "Run a command from a read-only Conary launch tree as the calling user"
)]
struct Args {
    /// Read-only launch tree to run the command from.
    #[arg(long, value_name = "PATH", required_unless_present = "check")]
    tree: Option<PathBuf>,

    /// Pre-composed directory for the tmpfs /etc (default: the tree's /etc).
    #[arg(long, value_name = "DIR", requires = "tree")]
    etc: Option<PathBuf>,

    /// Probe whether this host lets the launcher create its namespaces, and
    /// print a JSON readiness report.
    #[arg(long, conflicts_with_all = ["tree", "etc", "command"])]
    check: bool,

    /// The command and its arguments, after `--`.
    #[arg(last = true, value_name = "COMMAND", required_unless_present = "check")]
    command: Vec<OsString>,
}

/// Readiness report printed by `--check`.
#[derive(Debug, serde::Serialize)]
struct Readiness {
    ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl Readiness {
    fn from_probe(result: Result<(), LaunchError>) -> Self {
        match result {
            Ok(()) => Self {
                ready: true,
                kind: None,
                message: None,
            },
            Err(error) => Self {
                ready: false,
                kind: Some(error.kind()),
                message: Some(error.to_string()),
            },
        }
    }
}

fn check() -> i32 {
    let readiness = Readiness::from_probe(probe_in_this_process());
    match serde_json::to_string(&readiness) {
        Ok(json) => crate::ui::message(&json),
        Err(error) => {
            crate::ui::error(&format!("cannot encode readiness report: {error}"));
            return EXIT_LAUNCHER_FAILURE;
        }
    }
    if readiness.ready {
        0
    } else {
        EXIT_LAUNCHER_FAILURE
    }
}

/// Run the launcher with the process arguments; returns its exit status.
pub fn run() -> i32 {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error) => {
            let code = if error.use_stderr() {
                EXIT_LAUNCHER_FAILURE
            } else {
                0
            };
            let _ = error.print();
            return code;
        }
    };
    if args.check {
        return check();
    }
    let Some(tree) = args.tree else {
        crate::ui::error("--tree is required");
        return EXIT_LAUNCHER_FAILURE;
    };
    let request = LaunchRequest {
        tree,
        etc: args.etc.map_or(EtcSource::Tree, EtcSource::Directory),
        command: args.command,
    };
    match launch(&request) {
        Ok(never) => match never {},
        Err(error) => {
            crate::ui::error(&error.to_string());
            error.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_after_double_dash_is_kept_verbatim() {
        let args = Args::try_parse_from([
            "conary-exec",
            "--tree",
            "/var/lib/conary/roots/arch/current/tree",
            "--",
            "tree",
            "--version",
        ])
        .unwrap();
        assert_eq!(
            args.command,
            vec![OsString::from("tree"), OsString::from("--version")]
        );
        assert!(!args.check);
    }

    #[test]
    fn launch_requires_a_tree_and_a_command() {
        assert!(Args::try_parse_from(["conary-exec", "--", "true"]).is_err());
        assert!(Args::try_parse_from(["conary-exec", "--tree", "/t"]).is_err());
        assert!(Args::try_parse_from(["conary-exec", "--check"]).is_ok());
        assert!(Args::try_parse_from(["conary-exec", "--check", "--tree", "/t"]).is_err());
    }

    #[test]
    fn readiness_reports_the_typed_refusal_kind() {
        let refused = Readiness::from_probe(Err(LaunchError::UserNamespacesDisabled {
            setting: conary_core::launch::UsernsSetting::MaxUserNamespaces,
        }));
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&refused).unwrap()).unwrap();
        assert_eq!(json["ready"], false);
        assert_eq!(json["kind"], "user_namespaces_disabled");

        let ready = serde_json::to_value(Readiness::from_probe(Ok(()))).unwrap();
        assert_eq!(ready, serde_json::json!({ "ready": true }));
    }
}
