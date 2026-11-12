//! v1 can carry located findings, red checks and conflicts. Other blockers
//! must wait for a protocol which can express their source and repair.
use super::*;
use conary_core::plugin::Problem;

pub(crate) fn problems<G: Read>(
    github: &G,
    f: &Facts,
    memory: &Memory,
    kind: Kind,
) -> Result<Vec<Problem>, String> {
    match kind {
        Kind::Findings => {
            if memory.lost_on.iter().any(|(role, bases)| {
                f.reviewers.contains_key(role) && bases.contains_key(&f.revision.base)
            }) {
                return Err("v1 fixer lacks the carried findings source lost from GitHub".into());
            }
            if f.changes_requested || f.open_threads > 0 {
                return Err(
                    "v1 fixer cannot represent blocking GitHub threads or native review requests"
                        .into(),
                );
            }
            let records = github
                .reviews(&f.repo.to_string(), f.number)
                .map_err(|e| e.to_string())?;
            let mut problems = Vec::new();
            for (role, reviewer) in &f.reviewers {
                if !reviewer
                    .review
                    .as_ref()
                    .is_some_and(|r| r.verdict == Verdict::Findings)
                    || reviewer.findings_answered
                {
                    continue;
                }
                let body = reviewer
                    .record_id
                    .and_then(|id| records.iter().find(|r| r.id == Some(id)))
                    .and_then(|r| r.body.as_deref())
                    .ok_or("v1 fixer lacks the current GitHub findings source")?;
                let prefix = format!("<!-- {role}-review:v2 ");
                let marker = body
                    .trim_end()
                    .lines()
                    .last()
                    .and_then(|l| l.strip_prefix(&prefix))
                    .and_then(|l| l.strip_suffix(" -->"))
                    .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                    .ok_or("v1 fixer cannot represent the current GitHub review")?;
                let observed = reviewer.review.as_ref().expect("findings selected above");
                if marker["commit_sha"].as_str() != reviewer.reviewed_head.as_deref()
                    || marker["base"].as_str() != Some(f.revision.base.as_str())
                    || marker["scope"].as_str() != Some(f.revision.description.as_str())
                    || marker["reviewed_at"].as_u64() != Some(observed.at)
                {
                    return Err("v1 fixer GitHub findings changed during handoff".into());
                }
                let findings: Vec<conary_core::plugin::Finding> =
                    serde_json::from_value(marker["problems"].clone())
                        .map_err(|_| "v1 fixer cannot represent unstructured GitHub findings")?;
                if findings.is_empty()
                    || findings.iter().any(|f| !f.valid())
                    || marker["findings"].as_u64() != Some(findings.len() as u64)
                {
                    return Err("v1 fixer lacks complete current GitHub findings".into());
                }
                problems.extend(findings.into_iter().map(|mut finding| {
                    finding.text = format!(
                        "GitHub review {} ({role}, head {}): {}",
                        reviewer.record_id.unwrap(),
                        reviewer
                            .reviewed_head
                            .as_deref()
                            .unwrap_or(&f.revision.head),
                        finding.text
                    );
                    Problem::Finding { finding }
                }));
            }
            if problems.is_empty() {
                return Err("v1 fixer lacks the current or carried GitHub findings source".into());
            }
            Ok(problems)
        }
        Kind::Red => {
            let mut problems: Vec<_> = f
                .checks
                .iter()
                .filter(|c| c.state == Ci::Red)
                .map(|c| Problem::RedCheck {
                    check: c.name.clone(),
                    text: "GitHub check failed".into(),
                })
                .collect();
            problems.extend(f.deferred_failures.iter().map(|(name, failure)| Problem::RedCheck {
                check: name.clone(),
                text: format!("Owned deferred workflow run {} ended with {}; diagnostics: https://github.com/{}/actions/runs/{}{}",
                    failure.run, failure.conclusion, f.repo, failure.run,
                    failure.check_run.map_or(String::new(), |id| format!(" (check run {id})"))),
            }));
            if problems.is_empty() {
                Err("v1 fixer lacks the blocking check".into())
            } else {
                Ok(problems)
            }
        }
        Kind::Conflict => Ok(vec![Problem::Conflict {
            base: f
                .base_tip
                .clone()
                .ok_or("conflict base commit is unavailable")?,
            text: "GitHub reports a merge conflict".into(),
        }]),
        _ => Err("v1 fixer cannot represent judgment or description failures".into()),
    }
}
