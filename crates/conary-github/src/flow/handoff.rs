//! v1 can carry located findings, red checks and conflicts. Other blockers
//! must wait for a protocol which can express their source and repair. A
//! finding is handed with the target its answer goes to.
use super::*;
use conary_core::plugin::Problem;

pub(crate) fn problems<G: Read>(
    github: &G,
    f: &Facts,
    memory: &Memory,
    kind: Kind,
) -> Result<Vec<(Problem, Option<Handed>)>, String> {
    match kind {
        Kind::Findings => {
            if memory.lost_on.iter().any(|(role, bases)| {
                f.reviewers.contains_key(role) && bases.contains_key(&f.revision.base)
            }) {
                return Err("v1 fixer lacks the carried findings source lost from GitHub".into());
            }
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
            for (role, reviewer) in &f.reviewers {
                if !reviewer
                    .review
                    .as_ref()
                    .is_some_and(|r| r.verdict == Verdict::Findings)
                    || reviewer.findings_answered
                {
                    continue;
                }
                let record = reviewer
                    .record_id
                    .and_then(|id| records.iter().find(|r| r.id == Some(id)))
                    .ok_or("v1 fixer lacks the current GitHub findings source")?;
                let publication = Publication::read(record, role, &f.settings)
                    .ok_or("v1 fixer cannot represent unstructured GitHub findings")?;
                let marker = &publication.marker;
                let observed = reviewer.review.as_ref().expect("findings selected above");
                if Some(publication.head.as_str()) != reviewer.reviewed_head.as_deref()
                    || marker["commit_sha"].as_str() != Some(publication.head.as_str())
                    || marker["base"].as_str() != Some(f.revision.base.as_str())
                    || marker["scope"].as_str() != Some(f.revision.description.as_str())
                    || marker["reviewed_at"].as_u64() != Some(observed.at)
                {
                    return Err("v1 fixer GitHub findings changed during handoff".into());
                }
                if publication.findings.is_empty()
                    || publication.findings.iter().any(|f| !f.valid())
                    || marker["findings"].as_u64() != Some(publication.findings.len() as u64)
                {
                    return Err("v1 fixer lacks complete current GitHub findings".into());
                }
                for index in 0..publication.findings.len() {
                    if !targets.answered(&publication.mark(index)) {
                        hand(&publication, index, &mut problems)?;
                    }
                }
            }
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
