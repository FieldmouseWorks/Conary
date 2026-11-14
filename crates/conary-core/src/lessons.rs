//! The lessons register, `docs/lessons.toml` (`docs/DESIGN.md`, Properties):
//! what the pipeline Conary replaces taught, one entry per lesson, kept until
//! Conary builds the part it bears on.
//!
//! Reading the register checks it: every field present and well formed,
//! each ID once, and every `applies` value a component the register declares.
//! That check is the gate's, and an honest `open` entry passes it. Whether a
//! milestone may exit is a separate question, [`Register::exit`]: it fails
//! while an `open` entry applies to a component that milestone ships.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Where the register lives, from the repository's root.
pub const PATH: &str = "docs/lessons.toml";

/// How far a lesson has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Not yet stated anywhere Conary holds it.
    Open,
    /// DESIGN states it.
    Designed,
    /// A test holds it.
    Built,
}

/// One lesson.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lesson {
    /// Stable, never reused or renamed.
    pub id: String,
    /// The lesson, in one line.
    pub lesson: String,
    /// The GitHub issues or PRs it was learned in.
    pub from: Vec<String>,
    /// The components it bears on, each one the register declares.
    pub applies: Vec<String>,
    pub status: Status,
    /// The DESIGN section, test or issue that carries it.
    #[serde(rename = "where")]
    pub held: Vec<String>,
}

/// The register as the file has it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    milestones: Vec<String>,
    components: BTreeMap<String, Vec<String>>,
    lessons: Vec<Lesson>,
}

/// A register that has passed its check.
#[derive(Debug, Clone)]
pub struct Register {
    milestones: Vec<String>,
    /// Each component, and the milestones that ship it.
    components: BTreeMap<String, Vec<String>>,
    lessons: Vec<Lesson>,
}

/// Why a register was refused.
#[derive(Debug)]
pub enum RegisterError {
    Read(std::io::Error),
    /// Not TOML, or not the register's shape: a field missing or unknown, a
    /// status that isn't one, a value of the wrong type.
    Parse(toml::de::Error),
    /// Well formed, but these entries break its rules.
    Invalid(Vec<String>),
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "the lessons register can't be read: {e}"),
            Self::Parse(e) => write!(f, "the lessons register doesn't parse: {e}"),
            Self::Invalid(problems) => {
                write!(f, "the lessons register breaks its rules:")?;
                for p in problems {
                    write!(f, "\n  {p}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for RegisterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(e) => Some(e),
            Self::Parse(e) => Some(e),
            Self::Invalid(_) => None,
        }
    }
}

/// A milestone the register doesn't declare, so has no coverage to check.
#[derive(Debug)]
pub struct UnknownMilestone {
    pub asked: String,
    pub declared: Vec<String>,
}

impl std::fmt::Display for UnknownMilestone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the lessons register declares no milestone {}; it declares {}",
            self.asked,
            self.declared.join(", ")
        )
    }
}

impl std::error::Error for UnknownMilestone {}

/// The lessons part of a milestone's exit check.
#[derive(Debug, Serialize)]
pub struct Exit {
    pub milestone: String,
    /// The components the milestone ships.
    pub ships: Vec<String>,
    /// Each open lesson that applies to one of them: the milestone can't
    /// exit until every one is designed or built.
    pub open: Vec<Blocker>,
}

/// An open lesson that holds a milestone's exit.
#[derive(Debug, Serialize)]
pub struct Blocker {
    pub id: String,
    /// The components it applies to that the milestone ships.
    pub applies: Vec<String>,
    /// Where it's owed.
    #[serde(rename = "where")]
    pub held: Vec<String>,
}

impl Exit {
    /// Whether no open lesson holds the milestone.
    pub fn clear(&self) -> bool {
        self.open.is_empty()
    }
}

/// A stable ID: lowercase words joined by hyphens.
static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").unwrap());

/// A link to a GitHub issue or pull request.
static SOURCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://github\.com/[A-Za-z0-9-]+/[A-Za-z0-9._-]+/(issues|pull)/[1-9][0-9]*$")
        .unwrap()
});

impl Register {
    /// Reads and checks the register at `path`.
    pub fn load(path: &Path) -> Result<Self, RegisterError> {
        Self::parse(&std::fs::read_to_string(path).map_err(RegisterError::Read)?)
    }

    /// Parses and checks a register, reporting every rule it breaks.
    pub fn parse(text: &str) -> Result<Self, RegisterError> {
        let File {
            milestones,
            components,
            lessons,
        } = toml::from_str(text).map_err(RegisterError::Parse)?;
        let mut problems = Vec::new();

        if milestones.is_empty() {
            problems.push("it declares no milestones".to_owned());
        }
        let mut declared = BTreeSet::new();
        for m in &milestones {
            if !plain(m) {
                problems.push(format!("milestone {m:?} is blank or padded"));
            }
            if !declared.insert(m.as_str()) {
                problems.push(format!("milestone {m} is declared twice"));
            }
        }
        for (name, shipped_by) in &components {
            if !plain(name) {
                problems.push(format!("component {name:?} is blank or padded"));
            }
            if shipped_by.is_empty() {
                problems.push(format!(
                    "component {name:?} is shipped by no milestone, so no exit would check its lessons"
                ));
            }
            for (m, twice) in each(shipped_by) {
                if twice {
                    problems.push(format!("component {name:?} names milestone {m} twice"));
                } else if !declared.contains(m) {
                    problems.push(format!(
                        "component {name:?} is shipped by {m:?}, which isn't a declared milestone"
                    ));
                }
            }
        }
        for m in &milestones {
            if !components.values().any(|by| by.contains(m)) {
                problems.push(format!(
                    "milestone {m} ships no component, so its exit would check nothing"
                ));
            }
        }

        let mut ids = BTreeSet::new();
        for (i, l) in lessons.iter().enumerate() {
            let at = if l.id.is_empty() {
                format!("lesson {}", i + 1)
            } else {
                format!("lesson {:?}", l.id)
            };
            if !ID.is_match(&l.id) {
                problems.push(format!(
                    "{at}: its id isn't lowercase words joined by hyphens"
                ));
            } else if !ids.insert(l.id.as_str()) {
                problems.push(format!("{at}: the id is used twice"));
            }
            if !plain(&l.lesson) || l.lesson.contains(['\n', '\r']) {
                problems.push(format!("{at}: the lesson must be one nonblank line"));
            }
            if l.from.is_empty() {
                problems.push(format!("{at}: from names no source"));
            }
            for (link, twice) in each(&l.from) {
                if twice {
                    problems.push(format!("{at}: from names {link} twice"));
                } else if !SOURCE.is_match(link) {
                    problems.push(format!(
                        "{at}: from {link:?} isn't a link to a GitHub issue or pull request"
                    ));
                }
            }
            if l.applies.is_empty() {
                problems.push(format!("{at}: applies names no component"));
            }
            for (c, twice) in each(&l.applies) {
                if twice {
                    problems.push(format!("{at}: applies names {c:?} twice"));
                } else if !components.contains_key(c) {
                    problems.push(format!(
                        "{at}: applies to {c:?}, which isn't a declared component"
                    ));
                }
            }
            if l.held.is_empty() {
                problems.push(format!("{at}: where names nothing that carries it"));
            }
            for w in &l.held {
                if !plain(w) {
                    problems.push(format!("{at}: a where entry is blank or padded"));
                }
            }
        }

        if problems.is_empty() {
            Ok(Self {
                milestones,
                components,
                lessons,
            })
        } else {
            Err(RegisterError::Invalid(problems))
        }
    }

    pub fn milestones(&self) -> &[String] {
        &self.milestones
    }

    pub fn lessons(&self) -> &[Lesson] {
        &self.lessons
    }

    /// The components `milestone` ships.
    pub fn ships(&self, milestone: &str) -> Vec<String> {
        self.components
            .iter()
            .filter(|(_, by)| by.iter().any(|m| m == milestone))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// The lessons part of `milestone`'s exit check: every open lesson that
    /// applies to a component it ships.
    pub fn exit(&self, milestone: &str) -> Result<Exit, UnknownMilestone> {
        if !self.milestones.iter().any(|m| m == milestone) {
            return Err(UnknownMilestone {
                asked: milestone.to_owned(),
                declared: self.milestones.clone(),
            });
        }
        let ships = self.ships(milestone);
        let open = self
            .lessons
            .iter()
            .filter(|l| l.status == Status::Open)
            .filter_map(|l| {
                let applies: Vec<String> = l
                    .applies
                    .iter()
                    .filter(|c| ships.contains(c))
                    .cloned()
                    .collect();
                (!applies.is_empty()).then(|| Blocker {
                    id: l.id.clone(),
                    applies,
                    held: l.held.clone(),
                })
            })
            .collect();
        Ok(Exit {
            milestone: milestone.to_owned(),
            ships,
            open,
        })
    }
}

/// Nonblank, with no space at either end.
fn plain(s: &str) -> bool {
    !s.is_empty() && s.trim() == s
}

/// Each value, and whether an earlier one was the same.
fn each(values: &[String]) -> impl Iterator<Item = (&str, bool)> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .map(move |v| (v.as_str(), !seen.insert(v.as_str())))
}
