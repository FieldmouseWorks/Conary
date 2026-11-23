//! The reviewer's part of a PR's step, as burrow's steward takes it
//! (review_step.py, and the review part of step.py): what the reviewer said
//! of the head ([`super::reviews`]) and what that leaves the PR waiting on.
//! The reviewer reviews each push of a person's PR on its own; the flow asks
//! it to look when a look is owed that no push will bring, and at once for
//! each head of an App's PR, which it reviews only when asked.
use super::decide::{self, Decision};
use super::*;

/// A push to a person's PR the reviewer hasn't begun on after this long is
/// asked for: it reviews each such push within minutes when its usage lasts.
pub(super) const QUIET: u64 = 30 * 60;
/// A head the reviewer has said nothing of this long after it was first
/// asked for (or, never asked, after the flow first saw it) is the owner's
/// to hear of.
pub(super) const LOST: u64 = 120 * 60;
/// One head is asked for at most this often.
const AGAIN: u64 = 3600;

/// What's said when the flow asks for a look at a head no push brings one to.
pub(super) const ANSWERED: &str = "Every finding is answered; this head has no push to review.";

pub(super) enum Look {
    Next(Decision),
    /// The reviewers are done with this head. `capped`: past the round
    /// limit without being clean. `changed`: clean, but the PR was
    /// retargeted or edited after its push, which their word can't cover.
    Settled {
        capped: bool,
        changed: bool,
    },
}

fn wait(stage: Stage, until: Option<u64>, why: &str) -> Look {
    Look::Next(decide::wait(stage, until, why))
}

/// Ask the reviewer to look at the head, unless it was asked for this head
/// within the hour; then wait for that hour to pass.
fn ask(f: &Facts, m: &Memory, now: u64, why: &str, description: &str) -> Look {
    match &m.asked {
        Some((head, at)) if head == &f.revision.head && now < at.saturating_add(AGAIN) => {
            wait(Stage::Review, Some(at.saturating_add(AGAIN)), description)
        }
        _ => Look::Next(decide::at(
            Stage::Review,
            Step::AskReview(why.into()),
            description,
        )),
    }
}

pub(super) fn look(f: &Facts, m: &mut Memory, now: u64, request_review: bool) -> Look {
    // The stuck role's job, no longer running (or the branch would still be
    // its), that didn't answer: no loop is broken on nothing.
    if super::stuck::looped(m).is_some_and(|h| !h.answered) {
        return wait(Stage::Fix, None, super::stuck::ENDED);
    }
    let rounds = f.reviewers.values().map(|r| r.rounds).max().unwrap_or(0);
    let mut clean = !f.reviewers.is_empty()
        && f.reviewers
            .values()
            .all(|r| r.verdict == Some(Verdict::Clean));
    // Findings the flow conceded to the judge on this head get no other
    // review round ([`super::stuck::concede`]).
    let conceded = m.conceded.as_deref() == Some(f.revision.head.as_str());
    // Past the limit the flow stops waiting on the reviewers: a fix job, or
    // with a `job` plugin the stuck role, settles what's left on its
    // merits, and a judge decides whether it lands. The reviewer still
    // reviews each push; it just isn't waited for.
    let mut capped = !clean && (rounds >= f.settings.rounds || conceded);
    // Documentation alone that isn't R2 needs green CI and no review round.
    let unreviewed = !clean && !(f.classified.review() && request_review);
    if unreviewed {
        (clean, capped) = (true, false);
    }
    // Once the loop went to the stuck role and its job ended, no fix round
    // follows: the head it left gets one more review, and the judge decides.
    let broken = super::stuck::looped(m).map(|h| h.head.clone());
    let again = broken.as_ref().is_some_and(|h| *h != f.revision.head);
    capped |= broken.is_some();
    // An answered P2 or P3 raised again blocks nothing when the switch says
    // the judge reads it.
    let disputed = super::stuck::disputed(f);
    let what = f.open_threads > disputed.len() as u32 || f.changes_requested;
    let age = now.saturating_sub(m.seen);
    let waiting: Vec<_> = if clean || capped && !again {
        Vec::new()
    } else {
        f.reviewers
            .iter()
            .filter(|(_, r)| !matches!(r.verdict, Some(Verdict::Clean | Verdict::Findings)))
            .collect()
    };
    if !waiting.is_empty() || f.reviewers.is_empty() && !unreviewed {
        // One reviewer's threads needn't wait for another to finish.
        if what && broken.is_none() {
            return findings(f, m, now, (rounds, false), &disputed);
        }
        if f.reviewers.is_empty() {
            return wait(Stage::Review, None, "No reviewer can review it");
        }
        for (name, r) in &waiting {
            if r.verdict.is_none() {
                // With no account to read its word from, asking would
                // start a review nothing can count.
                if f.settings.review_authors.is_empty() {
                    return wait(
                        Stage::Review,
                        None,
                        "No review author is set to read its review from",
                    );
                }
                let description = format!("Waiting for {name}'s review of this commit");
                let head = head_short(f);
                if f.by_app {
                    let why =
                        format!("{head} was pushed to an App's PR, reviewed only when asked.");
                    return ask(f, m, now, &why, &description);
                }
                if age >= QUIET {
                    let why = format!("No review of {head} came after its push.");
                    return ask(f, m, now, &why, &description);
                }
                return wait(
                    Stage::Review,
                    Some(m.seen.saturating_add(QUIET)),
                    &description,
                );
            }
        }
        let (name, _) = waiting[0];
        return wait(
            Stage::Review,
            None,
            &format!("{name} is reviewing this commit"),
        );
    }
    if what && broken.is_some() {
        if f.open_threads == 0 {
            return wait(
                Stage::Review,
                None,
                "Changes requested on the stuck role's head: waiting for that review",
            );
        }
        return concede("The stuck role is done: conceding its open findings to the judge");
    }
    if what {
        return findings(f, m, now, (rounds, capped), &disputed);
    }
    if !disputed.is_empty() {
        return concede("Conceding answered P2s raised again to the judge");
    }
    if !clean && !capped {
        // Every finding answered without a new commit: the reviewers that
        // had findings look again, once their answers have settled.
        let description = format!("Findings answered; review round {} next", rounds + 1);
        let mut until = None;
        for r in f.reviewers.values() {
            if r.verdict == Some(Verdict::Findings) {
                let due = r.at.saturating_add(f.settings.settle_delay);
                if now > due {
                    return ask(f, m, now, ANSWERED, &description);
                }
                until = Some(until.map_or(due + 1, |u: u64| u.min(due + 1)));
            }
        }
        return wait(Stage::Review, until, &description);
    }
    Look::Settled {
        capped,
        changed: clean && !unreviewed && f.reviewers.values().any(|r| r.changed),
    }
}

/// Another fix round is due on open findings after `rounds` rounds
/// (`capped`, past the limit): the stuck role's instead, when one of its
/// triggers holds ([`super::stuck::trigger`]). The first fix round fixes
/// the diff fix growth is measured against.
fn findings(
    f: &Facts,
    m: &mut Memory,
    now: u64,
    (rounds, capped): (u32, bool),
    disputed: &std::collections::BTreeSet<String>,
) -> Look {
    if let Some((why, detail)) = super::stuck::trigger(f, m, rounds, disputed) {
        return Look::Next(decide::stuck(f, m, now, why, detail));
    }
    let mut decision = decide::hand(f, m, now, Kind::Findings, true);
    if decision.1 == Step::HandFix(Kind::Findings)
        && let Some(diff) = f.diff
    {
        m.original.get_or_insert(diff);
    }
    if capped {
        decision.2 = format!("Past the review-round limit ({rounds}): {}", decision.2);
    }
    Look::Next(decision)
}

fn concede(why: &str) -> Look {
    Look::Next(decide::at(Stage::Review, Step::Concede, why))
}

fn head_short(f: &Facts) -> &str {
    let head = &f.revision.head;
    &head[..head.len().min(7)]
}

/// Whether the owner should hear that no review came: one of the reviewers
/// has said nothing of a head [`LOST`] after it was first asked for, or,
/// never asked, after the flow first saw it.
pub(super) fn overdue(f: &Facts, m: &Memory, now: u64) -> bool {
    let since = match &m.first_asked {
        Some((head, at)) if head == &f.revision.head => (*at).max(m.seen),
        _ => m.seen,
    };
    f.classified.review()
        && now.saturating_sub(since) >= LOST
        && f.reviewers.values().any(|r| r.verdict.is_none())
}
