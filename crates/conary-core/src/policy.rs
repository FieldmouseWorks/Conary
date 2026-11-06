//! Class policy (`docs/DESIGN.md` section 4.4): what a change's class is, and
//! whether it's documentation only, from the repo and the paths it touches.
//! The policy is configuration the machine gives Conary, never something a
//! change's author sets: an author can only raise a class.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::op::Repo;

/// How much landing a change risks (`docs/DESIGN.md` section 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Class {
    /// Landing changes nothing outside the repo. A revert undoes it fully.
    R0,
    /// Landing goes live, but a revert undoes it.
    R1,
    /// Landing can't be fully undone, or is the owner's to decide.
    R2,
}

impl std::fmt::Display for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::R0 => "R0",
            Self::R1 => "R1",
            Self::R2 => "R2",
        })
    }
}

/// One file a change touches, and where it started when it's a rename or a
/// copy: a rename out of an R2 path is R2 as much as a rename into one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touched {
    pub path: String,
    pub from: Option<String>,
}

/// The paths a change touches, as far as they could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub files: Vec<Touched>,
    /// Whether the list may be cut short: GitHub lists only so many files
    /// of a comparison, and says nothing of the rest.
    pub cut: bool,
}

/// What the policy says of one change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classified {
    pub class: Class,
    /// What made it R2: the first path that did, or why its paths couldn't
    /// all be read.
    pub because: Option<String>,
    /// Every path it touches is documentation, as the policy names it.
    pub docs: bool,
}

impl Classified {
    /// Whether it needs a review round before it lands. Documentation needs
    /// green CI and nothing more, unless it's R2 (`docs/DESIGN.md` section
    /// 4.4).
    pub fn review(&self) -> bool {
        self.class == Class::R2 || !self.docs
    }
}

/// Never documentation, whatever a policy says: a repo's rules for agents,
/// and `.github/`, which holds the gate itself.
const NEVER_DOCS: &[&str] = &[
    r"(^|/)AGENTS(\.override)?\.md$",
    r"(^|/)CLAUDE(\.local)?\.md$",
    r"^\.github/",
];

/// One level of the policy file: the machine's, or one repo's additions to
/// it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Rules {
    /// Path regexes that make a change R2.
    #[serde(default)]
    r2: Vec<String>,
    /// Path regexes naming documentation.
    #[serde(default)]
    docs: Vec<String>,
    /// Path regexes that are never documentation, even where `docs` says so:
    /// a design doc, a page the repo publishes.
    #[serde(default)]
    not_docs: Vec<String>,
    /// Whether landing goes live: R1 rather than R0.
    live: Option<bool>,
}

/// The file: the machine's rules, and each repo's additions.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct File {
    #[serde(default)]
    r2: Vec<String>,
    #[serde(default)]
    docs: Vec<String>,
    #[serde(default)]
    not_docs: Vec<String>,
    live: Option<bool>,
    #[serde(default)]
    repos: Entries,
}

/// A JSON object's entries in order, a key that comes twice kept twice: a
/// map would keep only the last, and a repo's restrictions with it.
#[derive(Debug, Default)]
struct Entries(Vec<(String, Rules)>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = Entries;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object of repos, as owner/name")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Entries, A::Error> {
                let mut all = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    all.push(entry);
                }
                Ok(Entries(all))
            }
        }
        d.deserialize_map(Visit)
    }
}

/// Compiled rules: one level's.
#[derive(Debug)]
struct Compiled {
    r2: Vec<Regex>,
    docs: Vec<Regex>,
    not_docs: Vec<Regex>,
    live: Option<bool>,
}

impl Compiled {
    fn new(rules: Rules, at: &str) -> Result<Self, PolicyError> {
        let compile = |list: Vec<String>, key: &str| {
            list.into_iter()
                .map(|p| Regex::new(&p).map_err(|e| PolicyError(format!("{at}{key}: {p:?}: {e}"))))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Self {
            r2: compile(rules.r2, "r2")?,
            docs: compile(rules.docs, "docs")?,
            not_docs: compile(rules.not_docs, "notDocs")?,
            live: rules.live,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyError(pub String);

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "policy: {}", self.0)
    }
}

impl std::error::Error for PolicyError {}

/// The machine's class policy, as one JSON file says it:
///
/// ```json
/// {"r2": ["(^|/)CODEOWNERS$"], "docs": [], "notDocs": [], "live": false,
///  "repos": {"owner/name": {"r2": ["^migrations/"], "docs": ["^docs/"], "live": true}}}
/// ```
///
/// A repo's lists add to the machine's, never replace them, so no repo can
/// take a path off R2. Its `live` replaces the machine's. A key it doesn't
/// know is refused, a typo and a newer Conary's alike, rather than doing
/// nothing.
#[derive(Debug)]
pub struct Policy {
    machine: Compiled,
    repos: Vec<(Repo, Compiled)>,
    never_docs: Vec<Regex>,
    digest: String,
}

impl Policy {
    pub fn parse(bytes: &[u8]) -> Result<Self, PolicyError> {
        let file: File = serde_json::from_slice(bytes).map_err(|e| PolicyError(e.to_string()))?;
        let mut repos = Vec::new();
        for (name, rules) in file.repos.0 {
            let repo: Repo = name.parse().map_err(PolicyError)?;
            if repos.iter().any(|(r, _)| r == &repo) {
                return Err(PolicyError(format!("repos: {repo} is named twice")));
            }
            let compiled = Compiled::new(rules, &format!("repos.{repo}."))?;
            repos.push((repo, compiled));
        }
        Ok(Self {
            machine: Compiled::new(
                Rules {
                    r2: file.r2,
                    docs: file.docs,
                    not_docs: file.not_docs,
                    live: file.live,
                },
                "",
            )?,
            repos,
            never_docs: NEVER_DOCS
                .iter()
                .map(|p| Regex::new(p).expect("a fixed pattern"))
                .collect(),
            digest: blake3::hash(bytes).to_hex().to_string(),
        })
    }

    pub fn load(path: &std::path::Path) -> Result<Self, PolicyError> {
        let bytes =
            std::fs::read(path).map_err(|e| PolicyError(format!("{}: {e}", path.display())))?;
        Self::parse(&bytes)
    }

    /// The BLAKE3 hash of the file's bytes, which a `classify` op names, so
    /// the ledger says which policy decided.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    fn levels<'a>(&'a self, repo: &Repo) -> impl Iterator<Item = &'a Compiled> + Clone {
        std::iter::once(&self.machine).chain(
            self.repos
                .iter()
                .filter(move |(r, _)| r == repo)
                .map(|(_, c)| c),
        )
    }

    /// What the policy says of a change in `repo` touching `paths`.
    pub fn classify(&self, repo: &Repo, paths: &Paths) -> Classified {
        let levels = self.levels(repo);
        let every = || {
            paths
                .files
                .iter()
                .flat_map(|t| std::iter::once(&t.path).chain(&t.from))
        };
        let r2 = levels.clone().flat_map(|l| &l.r2);
        let because = every()
            .find(|path| r2.clone().any(|p| p.is_match(path)))
            .cloned()
            .or_else(|| {
                paths
                    .cut
                    .then(|| format!("{} files or more (too many to check)", paths.files.len()))
            });
        let docs_pats = levels.clone().flat_map(|l| &l.docs);
        let not_docs = levels
            .clone()
            .flat_map(|l| &l.not_docs)
            .chain(&self.never_docs);
        let docs = !paths.cut
            && !paths.files.is_empty()
            && every().all(|path| {
                docs_pats.clone().any(|p| p.is_match(path))
                    && !not_docs.clone().any(|p| p.is_match(path))
            });
        let live = levels.filter_map(|l| l.live).last().unwrap_or(false);
        let class = match (&because, live) {
            (Some(_), _) => Class::R2,
            (None, true) => Class::R1,
            (None, false) => Class::R0,
        };
        Classified {
            class,
            because,
            docs,
        }
    }
}

#[cfg(test)]
mod tests;
