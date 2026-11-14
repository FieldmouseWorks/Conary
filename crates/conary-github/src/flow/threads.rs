//! Where each finding the flow publishes lives on GitHub, and how its
//! answers find it. A finding's target is a review thread when its line is
//! in the diff, and a marked PR comment otherwise. Every target and every
//! answer carries a marker naming its finding, so nothing is matched by
//! place, order or text.
use super::{ReviewThread, Settings};
use crate::api::{Comment, Review as GithubReview};
use conary_core::plugin::Finding;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const FINDING: &str = "conary-finding:v1";
const ANSWER: &str = "conary-answer:v1";

/// One finding of one review publication. The publication is named by its
/// issuance, `receipt_digest(review_receipt(head, body))`: unique per
/// publication, since the body holds the findings and their completion
/// time. The finding is its index in that review marker's `problems`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingMark {
    pub role: String,
    pub head: String,
    pub issuance: String,
    pub finding: usize,
}

/// An answer's marker: the finding it answers, and the fix invocation it
/// came from, so a retry of one fix keeps its own and another fix's answer
/// is another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerMark {
    role: String,
    head: String,
    issuance: String,
    finding: usize,
    fix: String,
}

/// Where a finding's answers go: its review thread, by GitHub's id, or the
/// PR's conversation for a finding with no line in the diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    Thread(String),
    Comment,
}

/// A finding a fix was handed, and the target its answer goes to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handed {
    pub mark: FindingMark,
    pub target: Target,
}

/// A line comment a review is submitted with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineComment {
    pub path: String,
    pub line: u64,
    pub body: String,
}

/// The issuance a review's findings are named by.
pub fn issuance(head: &str, body: &str) -> String {
    super::receipt_digest(&super::review_receipt(head, body))
}

/// A finding's target text: what it says, then its marker.
pub fn finding_body(finding: &Finding, mark: &FindingMark, with_place: bool) -> String {
    let place = if with_place {
        format!("{}:{}: ", finding.path, finding.line)
    } else {
        String::new()
    };
    format!(
        "{place}{:?}: {}\n\n{}",
        finding.severity,
        finding.text.trim_end(),
        marker(FINDING, mark)
    )
}

/// An answer's text: the fix's reply, then its marker.
pub fn answer_body(reply: &str, mark: &FindingMark, fix: &str) -> String {
    let answered = AnswerMark {
        role: mark.role.clone(),
        head: mark.head.clone(),
        issuance: mark.issuance.clone(),
        finding: mark.finding,
        fix: fix.into(),
    };
    format!("{}\n\n{}", reply.trim_end(), marker(ANSWER, &answered))
}

fn marker(name: &str, mark: &impl Serialize) -> String {
    format!(
        "<!-- {name} {} -->",
        serde_json::to_string(mark).expect("a marker is JSON")
    )
}

/// The marker on a body's last line, if it is one.
fn read<T: for<'a> Deserialize<'a>>(name: &str, body: &str) -> Option<T> {
    body.trim_end()
        .lines()
        .last()?
        .strip_prefix(&format!("<!-- {name} "))?
        .strip_suffix(" -->")
        .and_then(|m| serde_json::from_str(m).ok())
}

/// Every new-side line range of the diff's hunks, by path: the lines a
/// review comment can be placed on. A deleted file, a binary file or a path
/// git had to quote has none.
pub fn hunks(diff: &str) -> BTreeMap<String, Vec<(u64, u64)>> {
    let header =
        regex::Regex::new(r"^@@ -\d+(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").expect("constant regex");
    let mut result = BTreeMap::<String, Vec<(u64, u64)>>::new();
    let mut path: Option<String> = None;
    // Lines left in the hunk being read, old side and new side. Its lines
    // are content, whatever they start with.
    let (mut old, mut new) = (0u64, 0u64);
    for line in diff.lines() {
        if old > 0 || new > 0 {
            match line.as_bytes().first() {
                Some(b'+') => new = new.saturating_sub(1),
                Some(b'-') => old = old.saturating_sub(1),
                Some(b'\\') => {}
                _ => {
                    old = old.saturating_sub(1);
                    new = new.saturating_sub(1);
                }
            }
            continue;
        }
        if line.starts_with("diff --git ") {
            path = None;
        } else if let Some(name) = line.strip_prefix("+++ ") {
            let name = name.strip_suffix('\t').unwrap_or(name);
            path = name.strip_prefix("b/").map(str::to_owned);
        } else if let Some(c) = header.captures(line) {
            let count =
                |m: Option<regex::Match<'_>>| m.map_or(Some(1), |m| m.as_str().parse().ok());
            let (Some(o), Ok(start), Some(n)) =
                (count(c.get(1)), c[2].parse::<u64>(), count(c.get(3)))
            else {
                return BTreeMap::new();
            };
            (old, new) = (o, n);
            if let Some(path) = &path
                && n > 0
            {
                result
                    .entry(path.clone())
                    .or_default()
                    .push((start, start.saturating_add(n)));
            }
        }
    }
    result
}

/// Whether a finding's line is in one of the diff's hunks.
pub fn placed(hunks: &BTreeMap<String, Vec<(u64, u64)>>, finding: &Finding) -> bool {
    hunks.get(&finding.path).is_some_and(|ranges| {
        ranges
            .iter()
            .any(|(start, end)| (*start..*end).contains(&finding.line))
    })
}

/// What GitHub shows of the flow's findings: each one's targets and whether
/// it's answered. Only the flow's review accounts' comments count, so a
/// marker quoted by anyone else names nothing.
#[derive(Debug, Default)]
pub struct Targets {
    /// The finding threads, by finding: the thread's id, and whether it's
    /// resolved.
    threads: BTreeMap<FindingMark, (String, bool)>,
    /// The thread ids that are a finding's.
    recognised: BTreeSet<String>,
    /// Findings with a marked PR comment.
    comments: BTreeSet<FindingMark>,
    /// Findings with an answer posted as a PR comment.
    answers: BTreeSet<FindingMark>,
}
impl Targets {
    pub fn read(
        threads: &BTreeMap<String, ReviewThread>,
        comments: &[Comment],
        settings: &Settings,
    ) -> Self {
        let ours = |login: Option<&str>| login.is_some_and(|l| settings.review_authors.contains(l));
        let mut result = Self::default();
        for (id, thread) in threads {
            let Some(first) = thread.comments.first() else {
                continue;
            };
            if !ours(first.author.as_deref()) {
                continue;
            }
            if let Some(mark) = read::<FindingMark>(FINDING, &first.body) {
                // Every thread the flow started on a finding is that
                // finding's, never someone's request. Should one finding
                // have two, the first by id is its target on every read.
                result.recognised.insert(id.clone());
                result
                    .threads
                    .entry(mark)
                    .or_insert_with(|| (id.clone(), thread.resolved));
            }
        }
        for c in comments {
            if !ours(c.user.as_ref().map(|u| u.login.as_str())) {
                continue;
            }
            let body = c.body.as_deref().unwrap_or("");
            if let Some(mark) = read::<FindingMark>(FINDING, body) {
                result.comments.insert(mark);
            } else if let Some(a) = read::<AnswerMark>(ANSWER, body) {
                result.answers.insert(FindingMark {
                    role: a.role,
                    head: a.head,
                    issuance: a.issuance,
                    finding: a.finding,
                });
            }
        }
        result
    }
    /// A finding's target: its thread when it has one.
    pub fn target(&self, mark: &FindingMark) -> Option<Target> {
        if let Some((id, _)) = self.threads.get(mark) {
            Some(Target::Thread(id.clone()))
        } else {
            self.comments.contains(mark).then_some(Target::Comment)
        }
    }
    /// A finding is answered once its thread is resolved, or, with no
    /// thread, once an answer to it is posted.
    pub fn answered(&self, mark: &FindingMark) -> bool {
        match self.threads.get(mark) {
            Some((_, resolved)) => *resolved,
            None => self.answers.contains(mark),
        }
    }
    /// Whether thread `id` is a finding's.
    pub fn recognises(&self, id: &str) -> bool {
        self.recognised.contains(id)
    }
    /// The findings whose threads are still open.
    pub fn open(&self) -> impl Iterator<Item = &FindingMark> {
        self.threads
            .iter()
            .filter(|(_, (_, resolved))| !resolved)
            .map(|(mark, _)| mark)
    }
    /// The finding a thread's first comment names, if it's one of ours.
    pub fn of_thread(thread: &ReviewThread, settings: &Settings) -> Option<FindingMark> {
        let first = thread.comments.first()?;
        first
            .author
            .as_deref()
            .is_some_and(|l| settings.review_authors.contains(l))
            .then(|| read(FINDING, &first.body))?
    }
}

/// A review the flow published, read back: its role, head and issuance, and
/// its structured findings. None for a body that isn't a role's v2 review
/// marker by a review account, or that carries no structured findings.
pub struct Publication {
    pub id: Option<u64>,
    pub role: String,
    pub head: String,
    pub issuance: String,
    pub marker: serde_json::Value,
    pub findings: Vec<Finding>,
}
impl Publication {
    pub fn read(record: &GithubReview, role: &str, settings: &Settings) -> Option<Self> {
        if !record
            .user
            .as_ref()
            .is_some_and(|u| settings.review_authors.contains(&u.login))
        {
            return None;
        }
        let head = record.commit_id.clone()?;
        let body = record.body.as_deref()?;
        let marker: serde_json::Value = read(&format!("{role}-review:v2"), body)?;
        let findings: Vec<Finding> =
            serde_json::from_value(marker.get("problems")?.clone()).ok()?;
        Some(Self {
            id: record.id,
            role: role.into(),
            issuance: issuance(&head, body),
            head,
            marker,
            findings,
        })
    }
    pub fn mark(&self, finding: usize) -> FindingMark {
        FindingMark {
            role: self.role.clone(),
            head: self.head.clone(),
            issuance: self.issuance.clone(),
            finding,
        }
    }
    /// Whether every finding is answered on its own target. None until
    /// every finding's target is on GitHub: publication isn't complete.
    pub fn answered(&self, targets: &Targets) -> Option<bool> {
        let marks: Vec<_> = (0..self.findings.len()).map(|i| self.mark(i)).collect();
        marks
            .iter()
            .all(|m| targets.target(m).is_some())
            .then(|| marks.iter().all(|m| targets.answered(m)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1..2 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
 one
+++ an added line that looks like a header
 two
-three
+four
@@ -10 +11,2 @@ fn x()
-a
+b
+c
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-x
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
";

    fn at(path: &str, line: u64) -> Finding {
        Finding {
            path: path.into(),
            line,
            severity: conary_core::plugin::Severity::P1,
            text: "t".into(),
        }
    }

    #[test]
    fn places_only_new_side_lines_inside_hunks() {
        let h = hunks(DIFF);
        assert_eq!(h["src/lib.rs"], [(1, 5), (11, 13)]);
        assert!(!h.contains_key("gone.rs") && !h.contains_key("img.png"));
        for (line, inside) in [
            (1, true),
            (4, true),
            (5, false),
            (10, false),
            (12, true),
            (13, false),
        ] {
            assert_eq!(placed(&h, &at("src/lib.rs", line)), inside, "line {line}");
        }
        assert!(!placed(&h, &at("other.rs", 1)));
    }

    #[test]
    fn markers_read_back_only_from_their_own_kind() {
        let mark = FindingMark {
            role: "reviewer".into(),
            head: "h".into(),
            issuance: "i".into(),
            finding: 2,
        };
        let finding = finding_body(&at("a.rs", 1), &mark, true);
        assert_eq!(read::<FindingMark>(FINDING, &finding), Some(mark.clone()));
        assert!(read::<AnswerMark>(ANSWER, &finding).is_none());
        let answer = answer_body("done", &mark, "3-4");
        assert!(read::<FindingMark>(FINDING, &answer).is_none());
        assert_eq!(read::<AnswerMark>(ANSWER, &answer).unwrap().fix, "3-4");
        // A quoted marker followed by more text names nothing.
        assert!(read::<FindingMark>(FINDING, &format!("{finding}\n> quoted")).is_none());
    }
}
