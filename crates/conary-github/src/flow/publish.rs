//! Publishing a review's findings, each on its own target, and a fix's
//! answers on the targets of the findings it was handed. Every write is
//! reconciled against what GitHub shows before it's sent again, so a lost
//! answer never makes a duplicate, and nothing counts as done until it's
//! read back.
use super::jobs::{self, Work};
use super::persist::Result;
use super::threads::{Targets, hunks, placed};
use super::*;
use conary_core::plugin::{self, Finding};

fn ours(f: &Facts, login: Option<&str>) -> bool {
    login.is_some_and(|l| f.settings.review_authors.contains(l))
}

/// The review on `head` with `body`, with a line comment for each finding
/// whose line is in the diff's hunks, and a marked PR comment for every
/// other finding. Complete only once every finding's target is read back.
pub(super) fn review<G: Write>(
    github: &G,
    f: &Facts,
    role: &str,
    head: &str,
    body: &str,
    findings: &[Finding],
) -> Result<()> {
    let repo = f.repo.to_string();
    let issuance = super::threads::issuance(head, body);
    let mark = |finding| FindingMark {
        role: role.into(),
        head: head.into(),
        issuance: issuance.clone(),
        finding,
    };
    // The exact review, by the flow's account, on its head.
    let posted = || -> Result<bool> {
        Ok(github.reviews(&repo, f.number)?.iter().any(|r| {
            r.commit_id.as_deref() == Some(head)
                && r.body.as_deref() == Some(body)
                && ours(f, r.user.as_ref().map(|u| u.login.as_str()))
        }))
    };
    // A diff GitHub won't give (too large, say) places no line: every
    // finding then goes to the conversation. One it can't give just now is
    // asked for again.
    let base = f.base_tip.as_deref().ok_or("base tip unknown")?;
    let hunks = match findings.is_empty() {
        true => Default::default(),
        false => match github.diff(&repo, base, head) {
            Ok(diff) => hunks(&diff),
            Err(e) if e.unavailable => return Err(e.into()),
            Err(_) => Default::default(),
        },
    };
    // A write can succeed even when its response is lost. Reconcile the
    // exact review before sending it again; its line comments came with it.
    if !posted()? {
        let comments: Vec<LineComment> = findings
            .iter()
            .enumerate()
            .filter(|(_, finding)| placed(&hunks, finding))
            .map(|(i, finding)| LineComment {
                path: finding.path.clone(),
                line: finding.line,
                body: finding_body(finding, &mark(i), false),
            })
            .collect();
        github.review(&repo, f.number, head, body, &comments)?;
        if !posted()? {
            return Err(crate::api::ApiError::unavailable("the review isn't on GitHub yet").into());
        }
    }
    if findings.is_empty() {
        return Ok(());
    }
    let targets = || -> Result<Targets> {
        Ok(Targets::read(
            &github.snapshot(&repo, f.number)?.threads,
            &github.comments(&repo, f.number)?,
            &f.settings,
        ))
    };
    let before = targets()?;
    for (i, finding) in findings.iter().enumerate() {
        if !placed(&hunks, finding) && before.target(&mark(i)).is_none() {
            github.comment(&repo, f.number, &finding_body(finding, &mark(i), true))?;
        }
    }
    let after = targets()?;
    if (0..findings.len()).any(|i| after.target(&mark(i)).is_none()) {
        // GitHub took what was sent but doesn't show it yet: asking again
        // later is the whole fix, so it spends no review attempt.
        return Err(crate::api::ApiError::unavailable(
            "not every finding's target is on GitHub yet",
        )
        .into());
    }
    Ok(())
}

/// A fix's answers, each on the target of the finding it answers, as the
/// flow's review account, resolving its thread. Only for the head the fix
/// left: pushed, or unchanged.
pub(super) fn answers<G: Write>(github: &G, f: &Facts, work: &Work, now: u64) -> Result<()> {
    let Some(Ok(plugin::Response {
        answer:
            plugin::Answer::Fix {
                result,
                answers: Some(answers),
            },
        ..
    })) = jobs::completion(work, now)?.result
    else {
        return Err("fix answer invalid".into());
    };
    let head = match &result {
        plugin::FixResult::Pushed { head } => head.as_str(),
        plugin::FixResult::Unchanged => work.job.revision.head.as_str(),
        plugin::FixResult::Unable { .. } => return Err("an unable fix posts no answers".into()),
    };
    if head != f.revision.head {
        return Err("the head moved past the fix; its answers aren't posted".into());
    }
    let repo = f.repo.to_string();
    let snapshot = github.snapshot(&repo, f.number)?;
    if snapshot.revision != f.revision {
        return Err("the PR moved before the fix's answers went up".into());
    }
    let comments = github.comments(&repo, f.number)?;
    for answer in &answers {
        let handed = work
            .handed
            .get(answer.problem)
            .cloned()
            .flatten()
            .ok_or("a fix answer names no finding it was handed")?;
        let body = answer_body(&answer.reply, &handed.mark, &work.job.id);
        match &handed.target {
            Target::Thread(id) => {
                let thread = snapshot
                    .threads
                    .get(id)
                    .ok_or("a finding's thread is gone")?;
                if Targets::of_thread(thread, &f.settings).as_ref() != Some(&handed.mark) {
                    return Err("a thread no longer holds the finding it was handed for".into());
                }
                if !thread
                    .comments
                    .iter()
                    .any(|c| ours(f, c.author.as_deref()) && c.body == body)
                {
                    github.reply(&repo, f.number, id, &body)?;
                }
                if !thread.resolved {
                    github.resolve(&repo, f.number, id)?;
                }
            }
            Target::Comment => {
                if !comments.iter().any(|c| {
                    ours(f, c.user.as_ref().map(|u| u.login.as_str()))
                        && c.body.as_deref() == Some(body.as_str())
                }) {
                    github.comment(&repo, f.number, &body)?;
                }
            }
        }
    }
    Ok(())
}
