//! Executable plugins; the wire contract lives in `docs/plugins.md`.
mod execution;
mod runner;
pub use execution::{Choice, Context, Executed, Execution, Provenance};
pub use runner::{Failure, Limits, Plugin};

use serde::{Deserialize, Serialize};

/// The first contract: the landing flow's four kinds, no declared offers.
pub const CONTRACT_V1: &str = "conary-plugin v1";
pub const CONTRACT_V2: &str = "conary-plugin v2";
/// Adds execution choices, context boundaries and actual execution provenance.
pub const CONTRACT: &str = "conary-plugin v3";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub engine: String,
    pub session: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Review,
    Fix,
    Judge,
    Proof,
    /// A general task (v2 and later).
    Job,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub contract: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<Execution>,
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
        #[serde(default)]
        reviewers: Vec<Identity>,
    },
    Proof {
        repo: String,
        tree: String,
        checks: Vec<String>,
    },
    /// v2: run a general task. `model` and `effort`, when set, must be
    /// choices the plugin's `describe` offered.
    Job {
        repo: String,
        workdir: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<Executed>,
    #[serde(flatten)]
    pub answer: Answer,
}

impl Response {
    pub fn provenance(&self) -> Option<Provenance> {
        self.execution.clone().map(|execution| Provenance {
            by: self.by.clone(),
            execution,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Answer {
    Describe {
        versions: Vec<String>,
        /// The legacy kinds only: a v1 runner decodes this list into an enum
        /// without `job`, so a newer kind here would make it reject the whole
        /// response.
        kinds: Vec<Kind>,
        /// v2: kinds beyond v1's (`job`), which only a v2 runner reads.
        #[serde(default)]
        v2_kinds: Vec<Kind>,
        /// v2: models and their efforts. Empty for a program.
        #[serde(default)]
        models: Vec<Model>,
        /// v2: required when the plugin speaks v2.
        #[serde(default)]
        capabilities: Option<Capabilities>,
        /// v2: usage meters the engine can report.
        #[serde(default)]
        meters: Vec<String>,
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
    Job {
        summary: String,
        next: Next,
    },
}

/// A model an engine offers, with the efforts it accepts and the limits the
/// engine knows. Ids and efforts are nonblank and unique.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub efforts: Vec<String>,
    #[serde(default)]
    pub context_tokens: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sandbox {
    /// The engine runs without one.
    None,
    /// The engine brings its own.
    Brings,
    /// The deployment must provide one.
    Needs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub sandbox: Sandbox,
    pub network: bool,
    /// Devices it needs, such as `kvm`; nonblank and unique.
    pub devices: Vec<String>,
    pub mcp: bool,
}

/// How a job ends, mirroring a job runner's `await`, `ask` and `done`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Next {
    Await {
        url: String,
    },
    Ask {
        question: String,
        default: String,
        one_way: bool,
    },
    Done,
}

impl Next {
    pub fn valid(&self) -> bool {
        let ok = |s: &str| !s.trim().is_empty();
        match self {
            Self::Await { url } => ok(url),
            Self::Ask {
                question, default, ..
            } => ok(question) && ok(default),
            Self::Done => true,
        }
    }
}

fn unique_nonblank<'a>(items: impl IntoIterator<Item = &'a str>) -> bool {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .all(|s| !s.trim().is_empty() && seen.insert(s))
}

/// What a v2 plugin declares. Checked once, at describe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offers {
    pub models: Vec<Model>,
    pub capabilities: Capabilities,
    pub meters: Vec<String>,
}

impl Offers {
    pub fn valid(&self, kinds: &[Kind]) -> bool {
        unique_nonblank(self.models.iter().map(|m| &*m.id))
            && self
                .models
                .iter()
                .all(|m| unique_nonblank(m.efforts.iter().map(String::as_str)))
            && unique_nonblank(self.capabilities.devices.iter().map(String::as_str))
            && unique_nonblank(self.meters.iter().map(String::as_str))
            // A proof is a program: a model never runs the checks.
            && !(kinds.contains(&Kind::Proof) && !self.models.is_empty())
    }

    /// Whether work may ask for this model and effort.
    pub fn allows(&self, model: Option<&str>, effort: Option<&str>) -> bool {
        match (model, effort) {
            (None, None) => true,
            (None, Some(_)) => false,
            (Some(m), e) => self
                .models
                .iter()
                .find(|x| x.id == m)
                .is_some_and(|x| e.is_none_or(|e| x.efforts.iter().any(|f| f == e))),
        }
    }
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
            Self::Job { .. } => Some(Kind::Job),
        }
    }
}

impl Request {
    /// Negotiation is always sent in the v1 envelope, so any plugin can
    /// answer it and name the versions it speaks.
    pub fn describe() -> Self {
        Self {
            contract: CONTRACT_V1.into(),
            execution: None,
            task: Task::Describe,
        }
    }

    /// Work is sent in the negotiated version.
    pub fn work(contract: &str, task: Task) -> Self {
        Self {
            contract: contract.into(),
            execution: None,
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
            reviewers: vec![],
        },
        Task::Proof {
            repo: repo.clone(),
            tree: base,
            checks: vec!["fixture-pass".into(), "fixture-fail".into()],
        },
        Task::Job {
            repo,
            workdir: ".".into(),
            prompt: "fixture: finish".into(),
            model: None,
            effort: None,
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
    fn offers_are_checked_and_proof_is_a_program() {
        let model = |id: &str, efforts: &[&str]| Model {
            id: id.into(),
            efforts: efforts.iter().map(|e| (*e).into()).collect(),
            context_tokens: None,
            max_output_tokens: None,
        };
        let offers = |models| Offers {
            models,
            capabilities: Capabilities {
                sandbox: Sandbox::None,
                network: false,
                devices: vec![],
                mcp: false,
            },
            meters: vec![],
        };
        let o = offers(vec![model("m", &["low", "high"])]);
        assert!(o.valid(&[Kind::Job]));
        assert!(!o.valid(&[Kind::Job, Kind::Proof]));
        assert!(offers(vec![]).valid(&[Kind::Proof]));
        assert!(!offers(vec![model("m", &[]), model("m", &[])]).valid(&[Kind::Job]));
        assert!(!offers(vec![model("m", &["a", "a"])]).valid(&[Kind::Job]));
        assert!(o.allows(Some("m"), Some("low")));
        assert!(o.allows(None, None));
        assert!(!o.allows(Some("m"), Some("max")));
        assert!(!o.allows(Some("x"), None));
        assert!(!o.allows(None, Some("low")));
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
