//! v1 can carry located findings, red checks and conflicts. Other blockers
//! must wait for a protocol which can express their source and repair. A
//! finding is handed with the target its answer goes to.
use super::*;
use conary_core::plugin::Problem;

pub(crate) fn problems<G: Read>(
    github: &G,
    f: &Facts,
    kind: Kind,
) -> Result<Vec<(Problem, Option<Handed>)>, String> {
    match kind {
        Kind::Findings => {
            if f.changes_requested {
                return Err("v1 fixer cannot represent native review requests".into());
            }
            let repo = f.repo.to_string();
            let records = github.reviews(&repo, f.number).map_err(|e| e.to_string())?;
            let comments = github
                .comments(&repo, f.number)
                .map_err(|e| e.to_string())?;
            let targets = Targets::read(&f.threads, &comments, &f.settings);
            // A thread the flow didn't publish a finding on is someone's
            // unstructured request; a fix can't say what it answers.
            if f.threads
                .iter()
                .any(|(id, t)| !t.resolved && !targets.recognises(id))
            {
                return Err("v1 fixer cannot represent blocking GitHub threads".into());
            }
            let mut problems = Vec::new();
            let mut handed = std::collections::BTreeSet::new();
            let mut hand = |publication: &Publication,
                            index: usize,
                            problems: &mut Vec<(Problem, Option<Handed>)>|
             -> Result<(), String> {
                let mark = publication.mark(index);
                if !handed.insert(mark.clone()) {
                    return Ok(());
                }
                let target = targets
                    .target(&mark)
                    .ok_or("a finding's target isn't on GitHub yet")?;
                let mut finding = publication.findings[index].clone();
                finding.text = format!(
                    "GitHub review {} ({}, head {}): {}",
                    publication.id.unwrap_or(0),
                    publication.role,
                    publication.head,
                    finding.text
                );
                problems.push((Problem::Finding { finding }, Some(Handed { mark, target })));
                Ok(())
            };
            // An open finding thread is that finding, whichever review it's
            // on: one left open on an earlier head is still owed an answer.
            let open: Vec<FindingMark> = targets.open().cloned().collect();
            for mark in open {
                let publication = records
                    .iter()
                    .filter_map(|r| Publication::read(r, &mark.role, &f.settings))
                    .find(|p| p.issuance == mark.issuance && p.head == mark.head)
                    .filter(|p| mark.finding < p.findings.len())
                    .ok_or("a finding thread's review isn't on GitHub")?;
                hand(&publication, mark.finding, &mut problems)?;
            }
            if problems.is_empty() {
                return Err("v1 fixer lacks the current or carried GitHub findings source".into());
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
