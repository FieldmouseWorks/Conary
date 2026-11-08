//! The review part of step.py/review_step.py. Findings survive outages;
//! answered findings request a fresh look until the cap, then a judge.
use super::decide::{self, Decision};
use super::*;

pub(super) enum Look {
    Next(Decision),
    Settled { capped: bool },
}
fn wait(stage: Stage, until: Option<u64>, why: &str) -> Look {
    Look::Next(decide::wait(stage, until, why))
}
fn at(stage: Stage, step: Step, why: &str) -> Look {
    Look::Next(decide::at(stage, step, why))
}

fn current<'a>(r: &'a Reviewer, revision: &Revision) -> Option<&'a Review> {
    r.review.as_ref().filter(|v| &v.revision == revision)
}

pub(super) fn look(f: &Facts, m: &mut Memory, now: u64) -> Look {
    // A newly read clean review can cover an earlier loss on this base.
    // A clean review already present before the loss cannot. Old records
    // without this observation keep their loss until a fresh look appears.
    for (name, r) in &f.reviewers {
        let lost = m.lost_on.get(name).and_then(|s| s.get(&f.revision.base));
        let clean = r
            .review
            .as_ref()
            .is_some_and(|v| v.revision == f.revision && v.verdict == Verdict::Clean);
        let changed = r
            .record_id
            .zip(m.latest_reviews.get(name).copied())
            .is_some_and(|(a, b)| a != b);
        if clean && lost.is_some_and(|h| h != &f.revision.head || changed) {
            if let Some(bases) = m.lost_on.get_mut(name) {
                bases.remove(&f.revision.base);
            }
            if m.lost_on.get(name).is_some_and(|s| s.is_empty()) {
                m.lost_on.remove(name);
            }
        }
        if let Some(id) = r.record_id {
            m.latest_reviews.insert(name.clone(), id);
        }
    }
    let needs_review = f.classified.review();
    let available = |r: &Reviewer| r.unavailable_until.is_none_or(|t| t <= now);
    let lost = |name: &str| m.lost_on.get(name).and_then(|s| s.get(&f.revision.base));
    let on: Vec<_> = f
        .reviewers
        .iter()
        .filter(|(name, r)| {
            available(r)
                || (current(r, &f.revision).is_some_and(|v| v.verdict == Verdict::Clean)
                    && lost(name).is_none())
        })
        .collect();
    let clean = !on.is_empty()
        && on.iter().all(|(name, r)| {
            current(r, &f.revision).is_some_and(|v| v.verdict == Verdict::Clean)
                && lost(name).is_none()
        });
    let rounds = f.reviewers.values().map(|r| r.rounds).max().unwrap_or(0);
    let capped = needs_review && !clean && rounds >= f.settings.rounds;
    // Consume obsolete service answers in memory, without pretending that
    // a GitHub write succeeded. Posting remains an explicit intention.
    for (name, r) in &f.reviewers {
        if let Some(answer) = &r.answer {
            let key = (name.clone(), answer.revision.clone(), answer.at);
            if m.consumed_reviews.contains(&key) {
                continue;
            }
            if answer.revision.base != f.revision.base {
                if answer.verdict == Verdict::Findings {
                    m.lost_on
                        .entry(name.clone())
                        .or_default()
                        .insert(answer.revision.base.clone(), answer.revision.head.clone());
                }
                m.consumed_reviews.insert(key);
                continue;
            }
            if answer.revision.head == f.revision.head || capped || !available(r) || !needs_review {
                return at(
                    Stage::Review,
                    Step::PostReview(name.clone()),
                    "Posting the review that came back",
                );
            }
            // A look at the current head follows: old findings could have
            // been fixed by its push and must not raise another round.
            m.consumed_reviews.insert(key);
        }
    }
    let findings = f.open_threads > 0
        || f.changes_requested
        || f.reviewers.values().any(|r| {
            current(r, &f.revision).is_some_and(|v| v.verdict == Verdict::Findings)
                && !r.findings_answered
        });
    if findings {
        let mut decision = decide::hand(f, m, now, Kind::Findings, true);
        if capped {
            decision.2 = format!("Past the review-round limit ({rounds}): {}", decision.2);
        }
        return Look::Next(decision);
    }
    let mut coverage = 0;
    for (name, r) in &f.reviewers {
        let loss = m.lost_on.get(name).and_then(|s| s.get(&f.revision.base));
        if current(r, &f.revision).is_some_and(|v| v.verdict == Verdict::Clean) && loss.is_none() {
            coverage += 1;
            continue;
        }
        if !available(r) {
            if r.under_way {
                return wait(
                    Stage::Review,
                    r.unavailable_until,
                    "A reviewer out of service is still reviewing it",
                );
            }
            if loss.is_some_and(|h| h == &f.revision.head || capped || !needs_review)
                || capped && r.queued
            {
                return wait(
                    Stage::Reviewer,
                    r.unavailable_until,
                    "A reviewer out of service still owes it findings",
                );
            }
            continue;
        }
        coverage += 1;
        if capped && (r.queued || r.under_way) {
            return wait(
                Stage::Review,
                None,
                "Past the round limit; a queued review is still owed",
            );
        }
        let wanted = loss.is_some() || needs_review && !capped;
        if wanted {
            let quiet = m.seen.saturating_add(f.settings.settle_delay);
            let answered = current(r, &f.revision)
                .filter(|v| v.verdict == Verdict::Findings && r.findings_answered)
                .map(|v| {
                    v.at.saturating_add(f.settings.settle_delay)
                        .saturating_add(1)
                })
                .unwrap_or(0);
            let due = quiet.max(answered);
            if now < due {
                return wait(
                    Stage::Review,
                    Some(due),
                    "Waiting for the head or answered findings to settle before review",
                );
            }
            return if r.queued || r.under_way {
                wait(Stage::Review, None, "Reviewing this commit")
            } else {
                at(
                    Stage::Review,
                    Step::QueueReview(name.clone()),
                    "Queue a review of this commit",
                )
            };
        }
    }
    if needs_review && coverage == 0 && !capped {
        let until = f
            .reviewers
            .values()
            .filter_map(|r| r.unavailable_until)
            .filter(|t| *t > now)
            .min();
        return wait(
            if f.reviewers.is_empty() {
                Stage::Review
            } else {
                Stage::Reviewer
            },
            until,
            "No reviewer can review it now",
        );
    }
    Look::Settled { capped }
}
