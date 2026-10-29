//! Where the API's tokens come from: one token for every repository, or a
//! command that prints one for the repository it's asked about. A GitHub
//! App's tokens are the second kind: each is for one installation, one
//! owner's repositories, and the ledger reads one owner's and writes its
//! anchor in another's.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::api::ApiError;

/// How long a token the command printed is used before it's asked again. A
/// token it prints must stay good at least this long.
pub const KEPT: Duration = Duration::from_secs(5 * 60);

/// The command's exit status when GitHub can't be asked just now, as cn's own.
const UNAVAILABLE: i32 = 75;

pub struct Tokens(Source);

enum Source {
    Given(String),
    Command {
        argv: Vec<String>,
        kept_for: Duration,
        kept: Mutex<HashMap<String, (String, Instant)>>,
    },
}

impl Tokens {
    /// One token for every repository.
    pub fn given(token: impl Into<String>) -> Self {
        Self(Source::Given(token.into()))
    }

    /// `command` is a program and its arguments, split on whitespace and run
    /// without a shell, with the repository (OWNER/NAME) added as its last
    /// argument. It prints the token, and exits 75 when GitHub can't be asked
    /// just now; its own messages go to stderr, which is the caller's.
    pub fn command(command: &str) -> Result<Self, ApiError> {
        Self::kept_for(command, KEPT)
    }

    fn kept_for(command: &str, kept_for: Duration) -> Result<Self, ApiError> {
        let argv: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
        if argv.is_empty() {
            return Err(ApiError::new("the token command is empty"));
        }
        Ok(Self(Source::Command {
            argv,
            kept_for,
            kept: Mutex::new(HashMap::new()),
        }))
    }

    /// The token for `repo` (OWNER/NAME).
    pub fn of(&self, repo: &str) -> Result<String, ApiError> {
        match &self.0 {
            Source::Given(token) => Ok(token.clone()),
            Source::Command {
                argv,
                kept_for,
                kept,
            } => {
                let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((token, at)) = kept.get(repo)
                    && at.elapsed() < *kept_for
                {
                    return Ok(token.clone());
                }
                let token = run(argv, repo)?;
                kept.insert(repo.to_owned(), (token.clone(), Instant::now()));
                Ok(token)
            }
        }
    }
}

/// What the command printed for `repo`: one token, nothing else. Errors name
/// the program and the repository, never the program's arguments (a
/// credential can be one) or what it printed.
fn run(argv: &[String], repo: &str) -> Result<String, ApiError> {
    let asked = format!("the token command ({}) for {repo}", argv[0]);
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .arg(repo)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| ApiError::new(format!("{asked}: {e}")))?;
    match out.status.code() {
        Some(0) => {}
        Some(UNAVAILABLE) => return Err(ApiError::unavailable(format!("{asked}: exit 75"))),
        _ => return Err(ApiError::new(format!("{asked}: {}", out.status))),
    }
    let token = String::from_utf8(out.stdout).unwrap_or_default();
    let token = token.trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return Err(ApiError::new(format!("{asked}: printed no token")));
    }
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that prints a new token each time it's run (a count kept in
    /// `dir`) for the repository it's given, after `then`.
    fn counting(dir: &std::path::Path, then: &str) -> String {
        let script = dir.join("token");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n{then}\nn=$(($(cat {count} 2>/dev/null || echo 0) + 1))\necho $n > {count}\necho \"t$n-$1\"\n",
                count = dir.join("count").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        script.display().to_string()
    }

    #[test]
    fn a_given_token_is_every_repository_s() {
        let tokens = Tokens::given("t");
        assert_eq!(tokens.of("a/r").unwrap(), "t");
        assert_eq!(tokens.of("b/r").unwrap(), "t");
    }

    #[test]
    fn the_command_is_asked_for_each_repository_and_its_token_kept() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::command(&format!("{} extra", counting(dir.path(), ""))).unwrap();
        // Its arguments, then the repository.
        assert_eq!(tokens.of("a/r").unwrap(), "t1-extra");
        assert_eq!(tokens.of("a/r").unwrap(), "t1-extra");
        assert_eq!(tokens.of("b/r").unwrap(), "t2-extra");
        let tokens = Tokens::command(&counting(dir.path(), "")).unwrap();
        assert_eq!(tokens.of("b/r").unwrap(), "t3-b/r");
        assert_eq!(tokens.of("a/r").unwrap(), "t4-a/r");
        assert_eq!(tokens.of("b/r").unwrap(), "t3-b/r");
    }

    #[test]
    fn a_kept_token_is_asked_for_again_once_its_time_is_up() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::kept_for(&counting(dir.path(), ""), Duration::ZERO).unwrap();
        assert_eq!(tokens.of("a/r").unwrap(), "t1-a/r");
        assert_eq!(tokens.of("a/r").unwrap(), "t2-a/r");
    }

    #[test]
    fn exit_75_is_github_unavailable_and_any_other_failure_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = Tokens::command(&counting(dir.path(), "exit 75"))
            .unwrap()
            .of("a/r")
            .unwrap_err();
        assert!(err.unavailable, "{err}");
        for then in ["exit 1", "echo ghs_secret; exit 2"] {
            let err = Tokens::command(&counting(dir.path(), then))
                .unwrap()
                .of("a/r")
                .unwrap_err();
            assert!(!err.unavailable, "{then}: {err}");
            assert!(!err.to_string().contains("ghs_secret"), "{then}: {err}");
        }
        let err = Tokens::command("/nonexistent/token")
            .unwrap()
            .of("a/r")
            .unwrap_err();
        assert!(!err.unavailable, "{err}");
        // Nothing was kept from a failure.
        let tokens = Tokens::command(&counting(dir.path(), "[ -e $0.ok ] || exit 75")).unwrap();
        assert!(tokens.of("a/r").unwrap_err().unavailable);
        std::fs::write(format!("{}.ok", dir.path().join("token").display()), "").unwrap();
        assert_eq!(tokens.of("a/r").unwrap(), "t1-a/r");
    }

    #[test]
    fn no_failure_quotes_the_command_s_arguments() {
        // A credential can be one of them: `env KEY=... helper`.
        let secret = "ghs_argument";
        for then in ["", "exit 75", "exit 1", "exit 0"] {
            let dir = tempfile::tempdir().unwrap();
            let program = if then.is_empty() {
                "/nonexistent/token".to_owned()
            } else {
                counting(dir.path(), then)
            };
            let err = Tokens::command(&format!("{program} {secret}"))
                .unwrap()
                .of("a/r")
                .unwrap_err()
                .to_string();
            assert!(!err.contains(secret), "{then:?}: {err}");
            assert!(
                err.contains(&program) && err.contains("a/r"),
                "{then:?}: {err}"
            );
        }
    }

    #[test]
    fn anything_but_one_token_is_refused_without_quoting_it() {
        for printed in ["", "   ", "two words", "two\nlines"] {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("token");
            std::fs::write(&script, format!("#!/bin/sh\nprintf '{printed}'\n")).unwrap();
            std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            let err = Tokens::command(&script.display().to_string())
                .unwrap()
                .of("a/r")
                .unwrap_err();
            assert!(!err.unavailable, "{printed:?}: {err}");
            assert!(
                err.to_string().contains("printed no token"),
                "{printed:?}: {err}"
            );
            assert!(!err.to_string().contains("words"), "{err}");
        }
    }

    #[test]
    fn an_empty_command_is_refused() {
        for command in ["", "  "] {
            assert!(Tokens::command(command).is_err(), "{command:?}");
        }
    }
}
