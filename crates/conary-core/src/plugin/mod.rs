//! Executable plugins; the wire contract lives in `docs/plugins.md`.
mod runner;
pub use runner::{Failure, Limits, Plugin};

use serde::{Deserialize, Serialize};

pub const CONTRACT: &str = "conary-plugin v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub engine: String,
    pub session: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Review,
    Fix,
    Judge,
    Proof,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub contract: String,
    #[serde(flatten)]
    pub task: Task,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Task {
    Describe,
    Review {
        repo: String,
        pr: u64,
        base: String,
        head: String,
        author: Identity,
    },
    Fix {
        repo: String,
        head: String,
        problems: Vec<Problem>,
    },
    Judge {
        repo: String,
        head: String,
        against: String,
        author: Identity,
    },
    Proof {
        repo: String,
        tree: String,
        checks: Vec<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Problem {
    Finding { finding: Finding },
    RedCheck { check: String, text: String },
    Conflict { base: String, text: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub path: String,
    pub line: u64,
    pub severity: Severity,
    pub text: String,
}

impl Finding {
    /// A finding names a file inside the repository by a relative path, a
    /// one-based line and a nonblank explanation.
    pub fn valid(&self) -> bool {
        inside_repository(&self.path) && self.line != 0 && !self.text.trim().is_empty()
    }
}

/// Whether `path` is relative and, read lexically, names something below the
/// repository root: never absolute, never the root itself, and no `..` that
/// climbs above it, however many components it passes through.
fn inside_repository(path: &str) -> bool {
    if path.trim().is_empty() || path.contains(['\\', '\0']) {
        return false;
    }
    let mut depth = 0usize;
    for component in std::path::Path::new(path).components() {
        match component {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return false,
        }
    }
    depth > 0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub contract: String,
    pub by: Identity,
    #[serde(flatten)]
    pub answer: Answer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Answer {
    Describe {
        versions: Vec<String>,
        kinds: Vec<Kind>,
    },
    Review {
        findings: Vec<Finding>,
    },
    Fix {
        result: FixResult,
    },
    Judge {
        verdict: Judgment,
        why: String,
    },
    Proof {
        results: Vec<CheckResult>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FixResult {
    Pushed { head: String },
    Unable { why: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Judgment {
    Pass,
    Fail,
    Owner,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub check: String,
    pub verdict: Verdict,
    pub why: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
}

impl Task {
    pub fn kind(&self) -> Option<Kind> {
        match self {
            Self::Describe => None,
            Self::Review { .. } => Some(Kind::Review),
            Self::Fix { .. } => Some(Kind::Fix),
            Self::Judge { .. } => Some(Kind::Judge),
            Self::Proof { .. } => Some(Kind::Proof),
        }
    }
}

impl Request {
    pub fn new(task: Task) -> Self {
        Self {
            contract: CONTRACT.into(),
            task,
        }
    }
}

/// Side-effect-free fixtures. Plugins must recognize this reserved repository
/// and report deterministic answers without contacting a remote or running code.
pub fn fixtures() -> Vec<Task> {
    let repo = "conary-test/plugin-conformance".to_owned();
    let base = "1111111111111111111111111111111111111111".to_owned();
    let head = "2222222222222222222222222222222222222222".to_owned();
    let author = Identity {
        engine: "conary-conformance-author".into(),
        session: Some("fixture-author".into()),
    };
    vec![
        Task::Review {
            repo: repo.clone(),
            pr: 1,
            base: base.clone(),
            head: head.clone(),
            author: author.clone(),
        },
        Task::Fix {
            repo: repo.clone(),
            head: head.clone(),
            problems: vec![Problem::RedCheck {
                check: "fixture".into(),
                text: "No writable repository is supplied".into(),
            }],
        },
        Task::Judge {
            repo: repo.clone(),
            head,
            against: "fixture: accept".into(),
            author,
        },
        Task::Proof {
            repo,
            tree: base,
            checks: vec!["fixture-pass".into(), "fixture-fail".into()],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(path: &str) -> Finding {
        Finding {
            path: path.into(),
            line: 1,
            severity: Severity::P2,
            text: "defect".into(),
        }
    }

    #[test]
    fn findings_stay_inside_the_repository() {
        for path in ["src/lib.rs", "./src/lib.rs", "src/../lib.rs", "a/b/../../c"] {
            assert!(finding(path).valid(), "{path}");
        }
        for path in [
            "",
            " ",
            "/etc/passwd",
            "//etc/passwd",
            "..",
            "../outside.rs",
            "../../outside.rs",
            "src/../../outside.rs",
            "a/b/../../../outside.rs",
            "./../outside.rs",
            ".",
            "src/..",
            "..\\outside.rs",
            "src/\0lib.rs",
        ] {
            assert!(!finding(path).valid(), "{path}");
        }
    }

    #[test]
    fn findings_need_a_line_and_text() {
        assert!(
            !Finding {
                line: 0,
                ..finding("src/lib.rs")
            }
            .valid()
        );
        assert!(
            !Finding {
                text: " ".into(),
                ..finding("src/lib.rs")
            }
            .valid()
        );
    }
}
