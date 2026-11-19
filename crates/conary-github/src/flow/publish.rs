//! Publishing a fix's answers on the targets of the findings it was
//! handed.
//!
//! Every write is saved in the job's record before it's sent
//! ([`super::jobs::Sent`]), and is sent only while GitHub doesn't show it.
//! One GitHub acknowledged is never sent again, however long GitHub takes
//! to show it. One whose answer was lost, or that a crash left unanswered,
//! may have gone up: it's waited for, and sent again only once GitHub has
//! gone [`SETTLE`] seconds without showing it, at most [`TRIES`] times in
//! all. One GitHub refused went nowhere and may be sent again at once.
//! Nothing counts as done until it's read back; until then GitHub is
//! lagging, and asking again later is the whole fix.
use super::jobs;
use super::persist::Result;
use super::threads::Targets;
use super::*;
use crate::api::ApiError;
use conary_core::plugin;

fn ours(f: &Facts, login: Option<&str>) -> bool {
    login.is_some_and(|l| f.settings.flow_authors.contains(l))
}

fn lagging(what: &str) -> Box<dyn std::error::Error + Send + Sync> {
    ApiError::unavailable(what).into()
}

/// How long a write that may have gone up is waited for before it's taken
/// not to have: far longer than GitHub takes to show what it took.
pub(crate) const SETTLE: u64 = 600;
/// How many times one write is ever sent without an acknowledgement.
pub(crate) const TRIES: u32 = 3;

/// Send write `key` of job `at` with `send`, unless it was sent already and
/// may still show: see the module's documentation. The caller sends it
/// only while GitHub doesn't show it.
fn write(
    record: &mut Record,
    data: &Data,
    pr: u64,
    at: usize,
    key: &str,
    now: u64,
    send: impl FnOnce() -> std::result::Result<(), ApiError>,
) -> Result<()> {
    let previous = record.jobs[at].sent.writes.get(key).cloned();
    if let Some(p) = &previous {
        if p.acknowledged || now < p.at.saturating_add(SETTLE) {
            return Ok(());
        }
        if p.tries >= TRIES {
            return Err(format!(
                "{key} may have gone up {TRIES} times and never showed; it isn't sent again"
            )
            .into());
        }
    }
    let tries = previous.as_ref().map_or(0, |p| p.tries) + 1;
    record.jobs[at].sent.writes.insert(
        key.into(),
        jobs::Attempt {
            at: now,
            tries,
            acknowledged: false,
        },
    );
    data.save(pr, record)?;
    match send() {
        Ok(()) => {
            if let Some(a) = record.jobs[at].sent.writes.get_mut(key) {
                a.acknowledged = true;
            }
            data.save(pr, record)?;
            Ok(())
        }
        // GitHub refused it: nothing went up.
        Err(e) if !e.unavailable => {
            match previous {
                Some(p) => record.jobs[at].sent.writes.insert(key.into(), p),
                None => record.jobs[at].sent.writes.remove(key),
            };
            data.save(pr, record)?;
            Err(e.into())
        }
        Err(e) => Err(e.into()),
    }
}

/// Job `at`'s fix answers, each on the target of the finding it answers, as
/// the flow's review account, with every thread of that finding resolved.
/// Only for the head the fix left: pushed, or unchanged. Complete only once
/// every reply and resolution is read back.
pub(super) fn answers<G: Write>(
    github: &G,
    f: &Facts,
    record: &mut Record,
    data: &Data,
    at: usize,
    now: u64,
) -> Result<()> {
    let work = &record.jobs[at];
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
    let id = work.job.id.clone();
    let mut planned = Vec::new();
    for answer in &answers {
        let handed = work
            .handed
            .get(answer.problem)
            .cloned()
            .flatten()
            .ok_or("a fix answer names no finding it was handed")?;
        let body = answer_body(&answer.reply, &handed.mark, &id);
        planned.push((handed, body));
    }
    let repo = f.repo.to_string();
    // Whether each answer, and each resolution, is on GitHub now.
    let read = || -> Result<(Vec<bool>, Vec<Vec<String>>)> {
        let snapshot = github.snapshot(&repo, f.number)?;
        if snapshot.revision != f.revision {
            return Err("the PR moved before the fix's answers went up".into());
        }
        let comments = github.comments(&repo, f.number)?;
        let targets = Targets::read(&snapshot.threads, &comments, &f.settings);
        let mut shown = Vec::new();
        let mut open = Vec::new();
        for (handed, body) in &planned {
            shown.push(match &handed.target {
                Target::Thread(thread) => {
                    let thread = snapshot
                        .threads
                        .get(thread)
                        .ok_or("a finding's thread is gone")?;
                    if Targets::of_thread(thread, &f.settings).as_ref() != Some(&handed.mark) {
                        return Err("a thread no longer holds the finding it was handed for".into());
                    }
                    thread
                        .comments
                        .iter()
                        .any(|c| ours(f, c.author.as_deref()) && &c.body == body)
                }
                Target::Comment => comments.iter().any(|c| {
                    ours(f, c.user.as_ref().map(|u| u.login.as_str()))
                        && c.body.as_deref() == Some(body.as_str())
                }),
            });
            open.push(
                targets
                    .threads(&handed.mark)
                    .iter()
                    .filter(|(_, resolved)| !resolved)
                    .map(|(thread, _)| thread.clone())
                    .collect(),
            );
        }
        Ok((shown, open))
    };
    let (shown, open) = read()?;
    for (n, (handed, body)) in planned.iter().enumerate() {
        if !shown[n] {
            write(
                record,
                data,
                f.number,
                at,
                &format!("answer {n}"),
                now,
                || match &handed.target {
                    Target::Thread(thread) => github.reply(&repo, f.number, thread, body),
                    Target::Comment => github.comment(&repo, f.number, body),
                },
            )?;
        }
        // Resolving is idempotent: every thread of the finding, a
        // duplicate's too, until GitHub shows it resolved.
        for thread in &open[n] {
            github.resolve(&repo, f.number, thread)?;
        }
    }
    let (shown, open) = read()?;
    if shown.contains(&false) || open.iter().any(|o| !o.is_empty()) {
        return Err(lagging("not every answer is on GitHub yet"));
    }
    Ok(())
}
