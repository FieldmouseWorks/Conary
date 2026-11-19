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

/// What GitHub shows of the flow's findings: each one's targets and whether
/// it's answered. Only the flow's review accounts' comments count, so a
/// marker quoted by anyone else names nothing.
#[derive(Debug, Default)]
pub struct Targets {
    /// The finding threads, by finding, in id order: each one's id, and
    /// whether it's resolved. One finding has two only when a review was
    /// sent twice; the first is its target.
    threads: BTreeMap<FindingMark, Vec<(String, bool)>>,
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
        let ours = |login: Option<&str>| login.is_some_and(|l| settings.flow_authors.contains(l));
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
                // finding's, never someone's request.
                result.recognised.insert(id.clone());
                result
                    .threads
                    .entry(mark)
                    .or_default()
                    .push((id.clone(), thread.resolved));
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
        if let Some((id, _)) = self.threads.get(mark).and_then(|t| t.first()) {
            Some(Target::Thread(id.clone()))
        } else {
            self.comments.contains(mark).then_some(Target::Comment)
        }
    }
    /// Every thread a finding has, and whether each is resolved.
    pub fn threads(&self, mark: &FindingMark) -> &[(String, bool)] {
        self.threads.get(mark).map_or(&[], Vec::as_slice)
    }
    /// Whether thread `id` is a finding's.
    pub fn recognises(&self, id: &str) -> bool {
        self.recognised.contains(id)
    }
    /// The findings whose threads are still open.
    pub fn open(&self) -> impl Iterator<Item = &FindingMark> {
        self.threads
            .iter()
            .filter(|(_, threads)| threads.iter().any(|(_, resolved)| !resolved))
            .map(|(mark, _)| mark)
    }
    /// The finding a thread's first comment names, if it's one of ours.
    pub fn of_thread(thread: &ReviewThread, settings: &Settings) -> Option<FindingMark> {
        let first = thread.comments.first()?;
        first
            .author
            .as_deref()
            .is_some_and(|l| settings.flow_authors.contains(l))
            .then(|| read(FINDING, &first.body))?
    }
}

/// Whether a PR comment carries a finding's or an answer's marker, whoever
/// wrote it: the conversation half of what a judge reads.
pub fn discussed(body: &str) -> bool {
    read::<FindingMark>(FINDING, body).is_some() || read::<AnswerMark>(ANSWER, body).is_some()
}

/// A review the flow published, read back: its role, head and issuance, and
/// its structured findings. None for a body that isn't a role's v2 review
/// marker by a review account, or that carries no structured findings.
pub struct Publication {
    pub id: Option<u64>,
    pub role: String,
    pub head: String,
    pub issuance: String,
    pub findings: Vec<Finding>,
}
impl Publication {
    pub fn read(record: &GithubReview, role: &str, settings: &Settings) -> Option<Self> {
        if !record
            .user
            .as_ref()
            .is_some_and(|u| settings.flow_authors.contains(&u.login))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: &str, line: u64) -> Finding {
        Finding {
            path: path.into(),
            line,
            severity: conary_core::plugin::Severity::P1,
            text: "t".into(),
        }
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
