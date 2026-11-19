//! The reviewer's part of a PR's step, as burrow's steward takes it
//! (review_step.py, and the review part of step.py): what the reviewer said
//! of the head ([`super::reviews`]) and what that leaves the PR waiting on.
//! The reviewer reviews each push on its own; the flow only asks it to look
//! again when a look is owed that no push will bring.
use super::decide::{self, Decision};
use super::*;

/// A push the reviewer hasn't begun on after this long is asked for, and
/// after [`LOST`] the owner hears of it: it reviews each push within
/// minutes when its usage lasts.
pub(super) const QUIET: u64 = 30 * 60;
/// See [`QUIET`].
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
    let rounds = f.reviewers.values().map(|r| r.rounds).max().unwrap_or(0);
    let mut clean = !f.reviewers.is_empty()
        && f.reviewers
            .values()
            .all(|r| r.verdict == Some(Verdict::Clean));
    // Past the limit the flow stops waiting on the reviewers: a fix job
    // settles what's left on its merits, and a judge decides whether it
    // lands. The reviewer still reviews each push; it just isn't waited for.
    let mut capped = rounds >= f.settings.rounds && !clean;
    // Documentation alone that isn't R2 needs green CI and no review round.
    let unreviewed = !clean && !(f.classified.review() && request_review);
    if unreviewed {
        (clean, capped) = (true, false);
    }
    let what = f.open_threads > 0 || f.changes_requested;
    let age = now.saturating_sub(m.seen);
    let waiting: Vec<_> = if clean || capped {
        Vec::new()
    } else {
        f.reviewers
            .iter()
            .filter(|(_, r)| !matches!(r.verdict, Some(Verdict::Clean | Verdict::Findings)))
            .collect()
    };
    if !waiting.is_empty() || f.reviewers.is_empty() && !unreviewed {
        // One reviewer's threads needn't wait for another to finish.
        if what {
            return Look::Next(decide::hand(f, m, now, Kind::Findings, true));
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
                if age >= QUIET {
                    let head = &f.revision.head;
                    let why = format!(
                        "No review of {} came after its push.",
                        &head[..head.len().min(7)]
                    );
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
    if what && capped {
        let mut decision = decide::hand(f, m, now, Kind::Findings, true);
        decision.2 = format!("Past the review-round limit ({rounds}): {}", decision.2);
        return Look::Next(decision);
    }
    if what {
        return Look::Next(decide::hand(f, m, now, Kind::Findings, true));
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

/// Whether the owner should hear that no review came: one of the reviewers
/// has said nothing of a head the flow first saw [`LOST`] ago.
pub(super) fn overdue(f: &Facts, m: &Memory, now: u64) -> bool {
    f.classified.review()
        && now.saturating_sub(m.seen) >= LOST
        && f.reviewers.values().any(|r| r.verdict.is_none())
}
