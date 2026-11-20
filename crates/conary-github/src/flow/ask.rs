//! Who asks the reviewer for a late review. The connector takes its ask from
//! no App, the flow's own included, so where the configuration names a door
//! for a repository the flow runs it, and the door posts the ask as whoever
//! the reviewer takes it from. Acting as that person stays in the door: the
//! flow hands it only the PR and the head. Elsewhere the flow comments as
//! itself.
use super::persist::Result;
use conary_core::op::Repo;
use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the flow waits on the door: it runs inside the pass, which
/// holds the repository's queue meanwhile.
const DEADLINE: Duration = Duration::from_secs(180);
/// The tokens the flow itself may hold in its environment. The door acts as
/// its own login, so the flow's never reaches it.
const TOKENS: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// The command that asks the reviewer for a review in the listed
/// repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewAsk {
    /// The program and its arguments, run without a shell, with the
    /// repository (`OWNER/REPO`), the PR's number and the head's commit
    /// after them. Exit 0 says the head needs no ask from the flow again;
    /// anything else says it wasn't asked, and the next pass tries again.
    #[serde(deserialize_with = "argv")]
    #[schemars(length(min = 1), extend("x-spender" = false))]
    pub command: Vec<String>,
    /// The repositories it asks in, as `OWNER/REPO`. In any other the flow
    /// posts the ask as its own comment.
    #[serde(
        deserialize_with = "crate::events::repos",
        serialize_with = "crate::events::repo_names"
    )]
    #[schemars(with = "Vec<String>", extend("x-spender" = false))]
    pub repos: Vec<Repo>,
}
fn argv<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    let argv = Vec::<String>::deserialize(d)?;
    if argv.first().is_none_or(String::is_empty) {
        return Err(serde::de::Error::custom("the review ask command is empty"));
    }
    Ok(argv)
}

impl ReviewAsk {
    /// Whether it asks in `repo`.
    pub(super) fn covers(&self, repo: &Repo) -> bool {
        let repo = repo.to_string();
        self.repos
            .iter()
            .any(|r| r.to_string().eq_ignore_ascii_case(&repo))
    }
    /// Runs the door for `head` of `repo`'s PR `pr`, and returns once it
    /// said it's done with that head. What it prints goes to the flow's
    /// standard error, beside its own log and apart from the flow's report,
    /// and its error names the program and the exit, never its arguments.
    pub(super) fn ask(&self, repo: &Repo, pr: u64, head: &str) -> Result<()> {
        let asked = format!(
            "the review ask command ({}) for {repo}#{pr}",
            self.command[0]
        );
        let mut command = Command::new(&self.command[0]);
        command
            .args(&self.command[1..])
            .args([repo.to_string(), pr.to_string(), head.to_owned()])
            .stdin(Stdio::null())
            .stdout(std::io::stderr())
            .stderr(Stdio::inherit());
        for token in TOKENS {
            command.env_remove(token);
        }
        let mut child = command.spawn().map_err(|e| format!("{asked}: {e}"))?;
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() >= DEADLINE {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{asked}: no answer in {}s", DEADLINE.as_secs()).into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if status.success() {
            return Ok(());
        }
        Err(format!("{asked}: {status}").into())
    }
}
