//! Publishing a fix's answers as replies on the review threads it was
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
use super::*;
use crate::api::ApiError;
use conary_core::plugin;

pub(super) fn ours(f: &Facts, login: Option<&str>) -> bool {
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

/// Where a write's progress is kept in the PR's record: a job's, or the
/// flow's own concessions ([`super::stuck::concede`]).
pub(super) type Progress = fn(&mut Record, usize) -> &mut jobs::Sent;

/// A job's progress, by its index.
fn of_job(record: &mut Record, at: usize) -> &mut jobs::Sent {
    &mut record.jobs[at].sent
}

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
    send_once(record, data, pr, (of_job, at), key, now, send)
}

/// [`write`], its progress kept where `progress` says.
pub(super) fn send_once(
    record: &mut Record,
    data: &Data,
    pr: u64,
    (progress, at): (Progress, usize),
    key: &str,
    now: u64,
    send: impl FnOnce() -> std::result::Result<(), ApiError>,
) -> Result<()> {
    let previous = progress(record, at).writes.get(key).cloned();
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
    progress(record, at).writes.insert(
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
            if let Some(a) = progress(record, at).writes.get_mut(key) {
                a.acknowledged = true;
            }
            data.save(pr, record)?;
            Ok(())
        }
        // GitHub refused it: nothing went up.
        Err(e) if !e.unavailable => {
            match previous {
                Some(p) => progress(record, at).writes.insert(key.into(), p),
                None => progress(record, at).writes.remove(key),
            };
            data.save(pr, record)?;
            Err(e.into())
        }
        Err(e) => Err(e.into()),
    }
}

/// Job `at`'s fix answers, each a reply on the review thread it was handed,
/// as the flow's account, with that thread then resolved. Only for the head
/// the fix left: pushed, or unchanged. Complete only once every reply and
/// resolution is read back. A reply is read back by its text: the same
/// reply already on the thread, an earlier fix's included, is that answer.
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
    let mut planned = Vec::new();
    for answer in &answers {
        let thread = work
            .handed
            .get(answer.problem)
            .cloned()
            .flatten()
            .ok_or("a fix answer names no thread it was handed")?;
        planned.push((thread, answer.reply.trim_end().to_owned()));
    }
    let repo = f.repo.to_string();
    // Whether each reply is on its thread now, and whether that thread is
    // resolved.
    let read = || -> Result<Vec<(bool, bool)>> {
        let snapshot = github.snapshot(&repo, f.number)?;
        if snapshot.revision != f.revision {
            return Err("the PR moved before the fix's answers went up".into());
        }
        planned
            .iter()
            .map(|(thread, body)| {
                let thread = snapshot
                    .threads
                    .get(thread)
                    .ok_or("a handed thread is gone")?;
                let shown = thread
                    .comments
                    .iter()
                    .any(|c| ours(f, c.author.as_deref()) && &c.body == body);
                Ok((shown, thread.resolved))
            })
            .collect()
    };
    let seen = read()?;
    for (n, (thread, body)) in planned.iter().enumerate() {
        let (shown, resolved) = seen[n];
        if !shown {
            write(
                record,
                data,
                f.number,
                at,
                &format!("answer {n}"),
                now,
                || github.reply(&repo, f.number, thread, body),
            )?;
        }
        // Resolving is idempotent: until GitHub shows it resolved.
        if !resolved {
            github.resolve(&repo, f.number, thread)?;
        }
    }
    if read()?.iter().any(|&(shown, resolved)| !shown || !resolved) {
        return Err(lagging("not every answer is on GitHub yet"));
    }
    Ok(())
}
