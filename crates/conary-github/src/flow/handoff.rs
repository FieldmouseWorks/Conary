//! v1 can carry located findings, red checks and conflicts. Other blockers
//! must wait for a protocol which can express their source and repair.
//!
//! A finding is an open review thread, whoever opened it: the reviewer's
//! (Codex's GitHub connector) or anyone's, as burrow's steward hands them.
//! It's handed with the thread's id, since its answer is a reply on that
//! thread, which is then resolved ([`super::publish::answers`]).
use super::*;
use conary_core::plugin::{Finding, Problem, Severity};
use std::sync::LazyLock;

/// The severity badge the connector opens each finding with.
static BADGE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"!\[P([0-3]) Badge\]").expect("pattern"));

/// Open thread `id` as a finding: where it is, its severity, and all it
/// says, each comment under its author.
fn thread(id: &str, t: &ReviewThread) -> Result<Problem, String> {
    let first = t
        .comments
        .first()
        .ok_or("v1 fixer lacks a review thread's comments")?;
    let line = t
        .line
        .ok_or("v1 fixer cannot represent a review thread on no line")?;
    // A thread without the connector's badge, a person's, blocks landing as
    // a P1 does.
    let severity = match BADGE.captures(&first.body).map(|c| c[1].to_owned()) {
        Some(p) if p == "0" => Severity::P0,
        Some(p) if p == "2" => Severity::P2,
        Some(p) if p == "3" => Severity::P3,
        _ => Severity::P1,
    };
    let said: Vec<_> = t
        .comments
        .iter()
        .map(|c| {
            format!(
                "{}: {}",
                c.author.as_deref().unwrap_or("(deleted account)"),
                c.body.trim_end()
            )
        })
        .collect();
    let finding = Finding {
        path: t.path.clone(),
        line,
        severity,
        text: format!("GitHub review thread {id}:\n\n{}", said.join("\n\n")),
    };
    if !finding.valid() {
        return Err("v1 fixer cannot represent a review thread outside the repository".into());
    }
    Ok(Problem::Finding { finding })
}

pub(crate) fn problems(f: &Facts, kind: Kind) -> Result<Vec<(Problem, Option<String>)>, String> {
    match kind {
        Kind::Findings => {
            if f.changes_requested {
                return Err("v1 fixer cannot represent native review requests".into());
            }
            let problems: Vec<_> = f
                .threads
                .iter()
                .filter(|(_, t)| !t.resolved)
                .map(|(id, t)| Ok((thread(id, t)?, Some(id.clone()))))
                .collect::<Result<_, String>>()?;
            if problems.is_empty() {
                return Err("v1 fixer lacks an open GitHub review thread".into());
            }
            Ok(problems)
        }
        Kind::Red => {
            let problems: Vec<_> = f
                .checks
                .iter()
                .filter(|c| c.state == Ci::Red)
                .map(|c| {
                    let problem = Problem::RedCheck {
                        check: c.name.clone(),
                        text: "GitHub check failed".into(),
                    };
                    (problem, None)
                })
                .collect();
            if problems.is_empty() {
                Err("v1 fixer lacks the blocking check".into())
            } else {
                Ok(problems)
            }
        }
        Kind::Conflict => Ok(vec![(
            Problem::Conflict {
                base: f
                    .base_tip
                    .clone()
                    .ok_or("conflict base commit is unavailable")?,
                text: "GitHub reports a merge conflict".into(),
            },
            None,
        )]),
        _ => Err("v1 fixer cannot represent judgment or description failures".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: Option<u64>, comments: &[(&str, &str)]) -> ReviewThread {
        ReviewThread {
            resolved: false,
            path: "src/lib.rs".into(),
            line,
            comments: comments
                .iter()
                .map(|(a, b)| ThreadComment {
                    author: Some((*a).into()),
                    body: (*b).into(),
                })
                .collect(),
        }
    }

    fn finding(t: &ReviewThread) -> Finding {
        match thread("T1", t).unwrap() {
            Problem::Finding { finding } => finding,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_thread_is_handed_as_its_place_badge_and_whole_conversation() {
        // As the connector opens one (burrow#1217), and a reply on it.
        let badge = "**<sub><sub>![P2 Badge](https://img.shields.io/badge/P2-yellow?style=flat)</sub></sub>  Title**\n\nWhy.";
        let f = finding(&at(
            Some(186),
            &[
                ("chatgpt-codex-connector[bot]", badge),
                ("person", "Agreed."),
            ],
        ));
        assert_eq!(
            (f.path.as_str(), f.line, f.severity),
            ("src/lib.rs", 186, Severity::P2)
        );
        assert!(f.text.starts_with("GitHub review thread T1:"));
        assert!(
            f.text
                .contains(&format!("chatgpt-codex-connector[bot]: {badge}"))
        );
        assert!(f.text.ends_with("person: Agreed."));
        // A person's thread has no badge: it blocks as a P1 does.
        assert_eq!(
            finding(&at(Some(3), &[("person", "P0 this")])).severity,
            Severity::P1
        );
    }

    #[test]
    fn a_thread_with_nothing_to_place_it_is_refused() {
        assert!(thread("T1", &at(None, &[("person", "this file")])).is_err());
        assert!(thread("T1", &at(Some(1), &[])).is_err());
        let mut outside = at(Some(1), &[("person", "x")]);
        outside.path = "../etc/passwd".into();
        assert!(thread("T1", &outside).is_err());
    }
}
