//! Breaking a review loop that won't settle, and a stall the flow couldn't
//! bring back, by handing the PR to the stuck role (burrow#945, burrow#961;
//! [review-loop.md](../../../../docs/review-loop.md) owns the policy).
//!
//! A loop goes to the stuck role, once for the PR, when another fix round
//! is due and one of these holds: the reviewers had findings for
//! `settings.rounds` rounds; the fix rounds grew the diff past
//! `settings.fix_growth` percent of what it was before the first of them;
//! or a reviewer raised again, unchanged, a finding the flow had answered.
//! No fix round follows. Once its job has answered, the head it leaves gets
//! one more review, the flow answers every finding still open with the
//! PR's follow-up issue and resolves it ([`concede`]), and the judge
//! decides. A job a pause or hold stopped undoes its handoff; one that
//! ended without its answer leaves the PR waiting ([`ENDED`]) until its
//! head moves ([`settle`]). A stall still overdue once its recovery had
//! its turn ([`super::recover`]) goes to the stuck role once for its head
//! and stage ([`stall`]).
//!
//! All of it needs a `job` plugin (`plugins.job`), which serves the stuck
//! role: without one, none of it happens, and the round limit sends what's
//! left to a fix and a judge.
//!
//! The request is the `job` kind's ([plugins.md](../../../../docs/plugins.md#the-job-kind)):
//! workdir `.`, and a prompt of one closed JSON object, `{"pr", "why",
//! "detail"}`, `why` one of [`Stuck`]'s words. Its answer's summary opens
//! with the outcome's line, then `follow-up: URL` or `follow-up: none`.
use super::persist::Result;
use super::*;
use conary_core::plugin::Severity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Why the flow hands a PR to the stuck role, by the word its request says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stuck {
    /// Its reviewers still had findings after the round limit.
    RoundLimit,
    /// Its fix rounds grew the diff past the configured share.
    FixGrowth,
    /// A reviewer raised again, unchanged, a finding the flow had answered.
    RaisedAgain,
    /// A stage stalled, and the flow couldn't bring it back.
    Stall,
}

impl Stuck {
    /// Its word in the request.
    pub fn word(self) -> &'static str {
        match self {
            Self::RoundLimit => "round_limit",
            Self::FixGrowth => "fix_growth",
            Self::RaisedAgain => "raised_again",
            Self::Stall => "stall",
        }
    }
}

/// What an answered P2 or P3 finding a reviewer raises again, unchanged,
/// does: the owner's open question on burrow#939, one switch.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Disputed {
    /// It's a finding like any other: it blocks, and, with a `job` plugin,
    /// hands the PR to the stuck role.
    #[default]
    Block,
    /// It blocks nothing: the flow answers that the judge reads it, and the
    /// judge decides.
    Judge,
}

/// A handoff to the stuck role, as the flow made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub why: Stuck,
    /// The head it was handed at.
    pub head: String,
    /// The stage that stalled; none for a loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    pub at: u64,
    /// The job it started. Stopped (a pause, a hold), the handoff is undone
    /// and made again once it's wanted ([`settle`]).
    pub job: String,
    /// Whether that job answered: only then is a loop broken.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub answered: bool,
}

/// What the PR waits on once its stuck-role job ended without its answer,
/// or with one that doesn't end it (`ask` or `await`, where the stuck
/// role's job is one turn that ends `done`):
/// overdue in its status, never conceded or handed again.
pub const ENDED: &str = "The stuck role's job ended without settling the PR";

/// The PR's loop handoff, once there is one: never a second.
pub(super) fn looped(m: &Memory) -> Option<&Handoff> {
    m.stuck.iter().find(|h| h.why != Stuck::Stall)
}

/// Each handoff as its job now stands: answered once it answered; undone
/// when the job was stopped, or, for a loop's job that ended without its
/// answer, once the head moved past where it was handed, so a push starts
/// the PR afresh rather than leave it waiting on a job that's gone.
pub(super) fn settle(f: &Facts, m: &mut Memory) {
    let job = f.jobs.get(&Kind::Stuck);
    if let Some(job) = job
        && job.state == JobState::Done
    {
        for h in m.stuck.iter_mut().filter(|h| h.job == job.id) {
            h.answered = true;
        }
    }
    let retired = &m.retired_jobs;
    let running = |h: &Handoff| job.is_some_and(|j| j.id == h.job && j.state.live());
    m.stuck.retain(|h| {
        !retired.contains(&h.job)
            && (h.answered || h.why == Stuck::Stall || h.head == f.revision.head || running(h))
    });
}

fn by(c: Option<&ThreadComment>, accounts: &BTreeSet<String>) -> bool {
    c.and_then(|c| c.author.as_deref())
        .is_some_and(|a| accounts.contains(a))
}

/// Each open thread a review author opened that raises again, unchanged, a
/// finding of theirs on the same file whose thread the flow answered and
/// that was resolved: the open thread's id, and its severity. Unchanged is
/// the same words, as [`super::fingerprint`] reads them, by the same
/// review author: one reviewer's finding never repeats another's.
fn repeats(f: &Facts) -> Vec<(&str, Severity)> {
    let opened = |t: &ReviewThread| {
        let first = t.comments.first()?;
        by(Some(first), &f.settings.review_authors)
            .then(|| (first.author.clone(), super::fingerprint(&first.body)))
    };
    let answered: Vec<_> = f
        .threads
        .values()
        .filter(|t| {
            t.resolved
                && t.comments
                    .iter()
                    .skip(1)
                    .any(|c| by(Some(c), &f.settings.flow_authors))
        })
        .filter_map(|t| Some((t.path.as_str(), opened(t)?)))
        .collect();
    f.threads
        .iter()
        .filter(|(_, t)| !t.resolved)
        .filter_map(|(id, t)| {
            let said = opened(t)?;
            answered
                .iter()
                .any(|(path, earlier)| *path == t.path && *earlier == said)
                .then(|| (id.as_str(), super::handoff::severity(&t.comments[0].body)))
        })
        .collect()
}

/// The open threads that block nothing: answered P2s and P3s raised again,
/// unchanged, when `disputed_p2` is `judge`.
pub(super) fn disputed(f: &Facts) -> BTreeSet<String> {
    if f.settings.disputed_p2 != Disputed::Judge {
        return BTreeSet::new();
    }
    repeats(f)
        .into_iter()
        .filter(|(_, s)| matches!(s, Severity::P2 | Severity::P3))
        .map(|(id, _)| id.to_owned())
        .collect()
}

/// How far the fix rounds grew the diff past their share, when they did.
fn grown(f: &Facts, m: &Memory) -> Option<String> {
    let (share, original, diff) = (f.settings.fix_growth?, m.original?, f.diff?);
    let grew = diff.saturating_sub(original);
    (u128::from(grew) * 100 > u128::from(original) * u128::from(share)).then(|| {
        format!(
            "its fix rounds grew the diff from {original} to {diff} changed lines, past {share}% of the original"
        )
    })
}

/// Why a PR another fix round is due on goes to the stuck role instead,
/// with what the flow saw: `rounds` rounds with findings, and the open
/// threads that block nothing. None without a `job` plugin, or once the
/// PR's loop was handed.
pub(super) fn trigger(
    f: &Facts,
    m: &Memory,
    rounds: u32,
    disputed: &BTreeSet<String>,
) -> Option<(Stuck, String)> {
    if !f.stuck_role || looped(m).is_some() {
        return None;
    }
    if rounds >= f.settings.rounds {
        return Some((
            Stuck::RoundLimit,
            format!(
                "{rounds} review rounds had findings; the limit is {}",
                f.settings.rounds
            ),
        ));
    }
    if let Some(detail) = grown(f, m) {
        return Some((Stuck::FixGrowth, detail));
    }
    repeats(f)
        .into_iter()
        .find(|(id, _)| !disputed.contains(*id))
        .map(|(id, _)| {
            (
                Stuck::RaisedAgain,
                format!("review thread {id} raises again, unchanged, a finding the flow answered"),
            )
        })
}

/// At a wait overdue at `stage` (`wait`, since `wait.since`), the handoff
/// of its stall to the stuck role: once for its head and stage, only once
/// its recovery had its turn (none applied, or GitHub refused it or never
/// answered). Only a stall of the stages a recovery is for (CI, the merge,
/// a fix), or one `failing` (three strikes, two lost jobs, a proof stalled
/// on its deadlines): never a hold, a wait on the owner, the base or a slot,
/// never a dependency PR, whose bot or sweep owns its branch, never while a
/// job owns the branch, and never the stuck role's own job ended without
/// its answer. The step, with the detail its request carries.
pub(super) fn stall(
    f: &Facts,
    m: &Memory,
    (stage, step, failing): (Stage, &Step, bool),
    wait: &Wait,
    recovery: Option<&Recovered>,
    now: u64,
) -> Option<Step> {
    if !f.stuck_role
        || now < wait.due
        || !matches!(step, Step::Wait { .. } | Step::Retry { .. })
        || !(failing || matches!(stage, Stage::Ci | Stage::Merge | Stage::Fix))
        || wait.description == ENDED
        || f.labels.contains(&f.settings.hold_label)
        || f.dependency.is_some()
        || super::decide::gate(f, m, now, false).is_some()
        || recovery.is_some_and(|r| matches!(r.answer, Answer::Asked | Answer::Accepted))
        || m.stuck
            .iter()
            .any(|h| h.why == Stuck::Stall && h.head == f.revision.head && h.stage == Some(stage))
    {
        return None;
    }
    let mut detail = format!(
        "The {stage:?} stage stalled: \"{}\", overdue since {}",
        wait.description, wait.due
    );
    if let Some(r) = recovery {
        detail.push_str(&format!(". Its recovery: {}", r.describe()));
    }
    Some(Step::Stuck(Stuck::Stall, detail))
}

/// The `job` request's prompt: the handoff, as the stuck role's plugin
/// reads it.
pub(super) fn prompt(f: &Facts, why: Stuck, detail: &str) -> String {
    serde_json::json!({"pr": f.number, "why": why.word(), "detail": detail}).to_string()
}

/// The head a stuck-role job's summary says, on its first line, that it
/// pushed.
pub(super) fn pushed(summary: &str) -> Option<&str> {
    summary.lines().next()?.strip_prefix("pushed ")
}

/// The follow-up issue a stuck-role job's summary names on its second
/// line, if it's an issue of `repo`'s.
pub fn followup(summary: &str, repo: &conary_core::op::Repo) -> Option<String> {
    let url = summary.lines().nth(1)?.strip_prefix("follow-up: ")?.trim();
    let prefix = format!("https://github.com/{repo}/issues/");
    let number = url
        .get(..prefix.len())
        .filter(|p| p.eq_ignore_ascii_case(&prefix))
        .and(url.get(prefix.len()..))?;
    (!number.is_empty() && !number.starts_with('0') && number.bytes().all(|b| b.is_ascii_digit()))
        .then(|| url.to_owned())
}

/// The open threads the flow concedes to the judge, each with its answer:
/// after a loop handoff, every one still open; otherwise those that block
/// nothing ([`disputed`]).
pub(super) fn conceding(f: &Facts, m: &Memory) -> Vec<(String, String)> {
    let reply = match looped(m) {
        Some(h) => format!(
            "The flow handed this PR to the stuck role ({}), so this finding gets no other fix round: the judge reads it before the PR lands. {}",
            h.why.word(),
            match &m.followup {
                Some(url) => format!("Its follow-up issue: {url}"),
                None => "The stuck role named no follow-up issue.".into(),
            }
        ),
        None => "Raised again, unchanged, after the flow answered it. With `disputed_p2` set to `judge`, an answered P2 or P3 blocks nothing: the judge reads it before the PR lands.".into(),
    };
    let disputed = disputed(f);
    f.threads
        .iter()
        .filter(|(id, t)| !t.resolved && (looped(m).is_some() || disputed.contains(*id)))
        .map(|(id, _)| (id.clone(), reply.clone()))
        .collect()
}

fn concessions(record: &mut Record, _: usize) -> &mut super::jobs::Sent {
    &mut record.memory.concessions
}

/// Answer each thread [`conceding`] names as the flow's account, and
/// resolve it: complete once every answer and resolution reads back, as a
/// fix's answers are ([`super::publish`]). Then no review round follows on
/// the head.
pub(super) fn concede<G: Write>(
    github: &G,
    f: &Facts,
    record: &mut Record,
    data: &Data,
    now: u64,
) -> Result<()> {
    let planned = conceding(f, &record.memory);
    if planned.is_empty() {
        return Err("no open finding to concede".into());
    }
    let repo = f.repo.to_string();
    let read = || -> Result<Vec<(bool, bool)>> {
        let snapshot = github.snapshot(&repo, f.number)?;
        if snapshot.revision != f.revision {
            return Err("the PR moved before the flow's answers went up".into());
        }
        planned
            .iter()
            .map(|(thread, body)| {
                let thread = snapshot
                    .threads
                    .get(thread)
                    .ok_or("a conceded thread is gone")?;
                let shown = thread
                    .comments
                    .iter()
                    .any(|c| super::publish::ours(f, c.author.as_deref()) && &c.body == body);
                Ok((shown, thread.resolved))
            })
            .collect()
    };
    let seen = read()?;
    for ((thread, body), (shown, resolved)) in planned.iter().zip(seen) {
        if !shown {
            super::publish::send_once(
                record,
                data,
                f.number,
                (concessions, 0),
                &format!("concede {} {thread}", f.revision.head),
                now,
                || github.reply(&repo, f.number, thread, body),
            )?;
        }
        if !resolved {
            github.resolve(&repo, f.number, thread)?;
        }
    }
    if read()?.iter().any(|&(shown, resolved)| !shown || !resolved) {
        return Err(crate::api::ApiError::unavailable("not every answer is on GitHub yet").into());
    }
    record.memory.conceded = Some(f.revision.head.clone());
    data.save(f.number, record)?;
    Ok(())
}
