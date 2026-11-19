//! A judgment speaks for its revision only on what it was judged on: every
//! reason the flow requires now, and the discussion as it is now.
use super::flow_case::Case;
use crate::api::Source;
use crate::flow::*;
use crate::sim::{CONNECTOR, REMI, STEWARD};

/// A fix's answer to the finding, posted on its thread as the flow posts
/// one.
fn answer(c: &Case, fix: &str) {
    c.hub.edit_flow(REMI, 3, |s| {
        s.threads
            .get_mut("T0")
            .unwrap()
            .comments
            .push(ThreadComment {
                author: Some(STEWARD.into()),
                body: format!("not a bug: the caller checks it ({fix})"),
            });
    });
}

/// Five rounds of findings, answered: past the round limit, with the
/// reviewer's last finding on a resolved thread, the flow asks a judge.
fn capped() -> Case {
    let mut c = Case::new();
    c.roles(&["primary", "secondary"]);
    for t in 1..=5 {
        c.review("primary", Verdict::Findings, t);
    }
    c.answered();
    c.hub
        .flow_thread_comments(REMI, 3, "T0", &[(CONNECTOR, "a finding")]);
    c.hub.flow_thread(REMI, 3, "T0", true);
    assert_eq!(c.pass(4000).step, Step::Judge(vec![Trigger::RoundLimit]));
    c
}

/// Judged for `why` on the discussion now.
fn on(c: &Case, why: &[Trigger]) -> Basis {
    Basis {
        why: why.to_vec(),
        context: c.context(),
    }
}

#[test]
fn a_judgment_on_the_reasons_and_the_discussion_now_still_counts() {
    for (verdict, expected) in [
        (Judgment::Pass, Step::Merge),
        (Judgment::Owner, Step::ListOwner),
    ] {
        let mut c = capped();
        let basis = on(&c, &[Trigger::RoundLimit]);
        c.judge_on(verdict, basis);
        // The judgment itself makes `asked` hold; it needn't name it.
        assert_eq!(c.view(4001).step, expected);
    }
}

#[test]
fn a_finding_answered_without_a_push_asks_for_a_new_judgment() {
    for (verdict, counts) in [
        (Judgment::Pass, Step::Merge),
        (Judgment::Owner, Step::ListOwner),
    ] {
        let mut c = capped();
        let basis = on(&c, &[Trigger::RoundLimit]);
        c.judge_on(verdict, basis);
        let judged = c.facts();
        assert_eq!(c.view(4001).step, counts);
        // A fix answers the finding and pushes nothing.
        answer(&c, "fix-1");
        assert_eq!(c.scope(), judged.revision);
        assert_eq!(
            c.pass(4001).step,
            Step::Judge(vec![Trigger::Asked, Trigger::RoundLimit])
        );
        // The old issuance stays. It only no longer speaks.
        let f = c.facts();
        assert_eq!(f.judgment_generation, judged.judgment_generation);
        assert_eq!(f.judgment, judged.judgment);
        // A judgment on the discussion now counts.
        c.judge(verdict.word());
        assert_eq!(c.view(4002).step, counts);
    }
}

#[test]
fn a_newly_required_reason_asks_for_a_new_judgment() {
    let mut c = capped();
    // Judged for the author's engine alone, before the round limit held.
    let basis = on(&c, &[Trigger::AuthorEngine]);
    c.judge_on(Judgment::Pass, basis);
    assert_eq!(
        c.view(4001).step,
        Step::Judge(vec![Trigger::Asked, Trigger::RoundLimit])
    );
    let basis = on(&c, &[Trigger::AuthorEngine, Trigger::RoundLimit]);
    c.judge_on(Judgment::Pass, basis);
    assert_eq!(c.view(4002).step, Step::Merge);
}

#[test]
fn an_issuance_that_does_not_say_what_it_judged_on_is_insufficient() {
    for word in ["pass", "land", "owner"] {
        let mut c = capped();
        c.judge_v2(word);
        assert_eq!(
            c.view(4001).step,
            Step::Judge(vec![Trigger::Asked, Trigger::RoundLimit]),
            "{word}"
        );
    }
    // A failure needs no basis to be acted on.
    let mut c = capped();
    c.judge_v2("fail");
    assert_eq!(c.view(4001).step, Step::HandFix(Kind::JudgeFailure));
}

#[test]
fn generations_stay_ordered_across_issued_versions() {
    let mut c = capped();
    c.judge_v2("pass");
    let asked = Step::Judge(vec![Trigger::Asked, Trigger::RoundLimit]);
    assert_eq!(c.view(4001).step, asked);
    let current = on(&c, &[Trigger::RoundLimit]);
    c.judge_on(Judgment::Pass, current.clone());
    assert_eq!(c.view(4002).step, Step::Merge);
    // A newer v2 issuance supersedes a v3 one.
    c.judge_v2("fail");
    assert_eq!(c.view(4003).step, Step::HandFix(Kind::JudgeFailure));
    // A newer v3 pass on the discussion now supersedes it.
    c.judge_on(Judgment::Pass, current);
    assert_eq!(c.view(4004).step, Step::Merge);
    // A newer pass judged on another discussion revives no older one.
    let stale = Basis {
        why: vec![Trigger::RoundLimit],
        context: judging_context(&Default::default(), &Default::default()),
    };
    c.judge_on(Judgment::Pass, stale);
    assert_eq!(c.view(4005).step, asked);
}

#[test]
fn a_finding_or_answer_on_github_asks_for_a_new_judgment() {
    let thread = |c: &Case, comments: &[(&str, &str)]| {
        c.hub
            .flow_thread_comments(crate::sim::REMI, 3, "T1", comments);
        c.hub.flow_thread(crate::sim::REMI, 3, "T1", true);
    };
    let asked = Step::Judge(vec![Trigger::Asked, Trigger::RoundLimit]);
    let mut c = capped();
    let basis = on(&c, &[Trigger::RoundLimit]);
    c.judge_on(Judgment::Pass, basis);
    assert_eq!(c.view(4001).step, Step::Merge);
    // A person's finding and its answer, on GitHub alone, resolved without a
    // push: no worker posted them.
    thread(&c, &[("person", "a finding"), ("owner", "an answer")]);
    assert_eq!(c.view(4002).step, asked);
    let basis = on(&c, &[Trigger::RoundLimit]);
    c.judge_on(Judgment::Pass, basis);
    assert_eq!(c.view(4003).step, Step::Merge);
    // Resolving or reopening it adds nothing a judge hasn't read.
    c.hub.flow_thread(crate::sim::REMI, 3, "T1", false);
    c.hub.flow_thread(crate::sim::REMI, 3, "T1", true);
    assert_eq!(c.view(4004).step, Step::Merge);
    // An edited answer is another discussion.
    thread(
        &c,
        &[("person", "a finding"), ("owner", "an edited answer")],
    );
    assert_eq!(c.view(4005).step, asked);
}

#[test]
fn a_superseded_judgment_never_spends_the_unanswered_budget() {
    let mut c = capped();
    let asked = Step::Judge(vec![Trigger::RoundLimit]);
    for round in 0..4 {
        // Its judge answered, then the discussion moved on.
        c.jobs.insert(
            Kind::Judge,
            Job {
                id: format!("judge-{round}"),
                revision: c.scope(),
                state: JobState::Answered,
                started: 4000,
                repeated: false,
            },
        );
        answer(&c, &format!("fix-{round}"));
        assert_eq!(c.pass(5000 + round * 100).step, asked, "{round}");
        assert_eq!(c.pass(5060 + round * 100).step, asked, "{round}");
    }
    assert!(c.memory.unanswered.is_empty());
}

#[test]
fn a_finding_or_answer_in_a_review_body_asks_for_a_new_judgment() {
    let mut c = capped();
    let head = c.scope().head;
    c.judge("pass");
    assert_eq!(c.view(4001).step, Step::Merge);
    // A person's finding in a review's body: no thread, no worker answer.
    c.hub
        .person_review(REMI, 3, "person", "User", &head, "a finding", "COMMENTED");
    let id = c.hub.reviews(REMI, 3).unwrap().last().unwrap().id.unwrap();
    let asked = Step::Judge(vec![Trigger::Asked, Trigger::RoundLimit]);
    assert_eq!(c.view(4002).step, asked);
    c.judge("pass");
    assert_eq!(c.view(4003).step, Step::Merge);
    // Its answer, edited into the same body.
    c.hub
        .edit_review_body(REMI, 3, id, "a finding\n\nAnswered: not a bug");
    assert_eq!(c.view(4004).step, asked);
    c.judge("pass");
    assert_eq!(c.view(4005).step, Step::Merge);
    // An approval is permission, not discussion.
    c.hub
        .person_review(REMI, 3, "person", "User", &head, "looks right", "APPROVED");
    assert_eq!(c.view(4006).step, Step::Merge);
}

/// A finished judge job, asked on the discussion now.
fn asked_judge(c: &mut Case) -> Basis {
    let asked = on(c, &[Trigger::RoundLimit]);
    c.jobs.insert(
        Kind::Judge,
        Job {
            id: "j1".into(),
            revision: c.scope(),
            state: JobState::Done,
            started: 3900,
            repeated: false,
        },
    );
    c.judge_requests.insert("j1".into(), Some(asked.clone()));
    asked
}

#[test]
fn an_answered_judge_stays_answered_whatever_is_read_later() {
    let mut c = capped();
    let asked = asked_judge(&mut c);
    let again = Step::Judge(vec![Trigger::Asked, Trigger::RoundLimit]);
    // An issuance on another basis answers some other request: the job's
    // own answer is still missing, and counts as such.
    c.judge_on(Judgment::Pass, on(&c, &[Trigger::AuthorEngine]));
    assert!(matches!(c.pass(4001).step, Step::Wait { .. }));
    assert_eq!(c.pass(4062).step, again);
    assert!(!c.memory.unanswered.is_empty());
    // Its answer comes late: it leaves the missing count, and it counts.
    let sealed = c.hub.comments(REMI, 3).unwrap().len();
    c.judge_on(Judgment::Pass, asked);
    assert_eq!(c.pass(4100).step, Step::Merge);
    assert!(c.memory.unanswered.is_empty());
    assert!(c.memory.answered_judges.contains("j1"));
    // A newer generation on another discussion supersedes it: asked again
    // at once, never waiting on it or counting it as missing.
    let stale = Basis {
        why: vec![Trigger::RoundLimit],
        context: judging_context(&Default::default(), &Default::default()),
    };
    c.judge_on(Judgment::Pass, stale);
    assert_eq!(c.pass(4200).step, again);
    assert!(c.memory.unanswered.is_empty());
    // Its comment gone from GitHub, and the job read as owed again, as a
    // pass reads a finished job with no generation in sight: still
    // answered by what the flow remembers.
    c.hub.edit_flow_comment(REMI, 3, sealed, "");
    c.hub.edit_flow_comment(REMI, 3, sealed + 1, "");
    c.jobs.get_mut(&Kind::Judge).unwrap().state = JobState::Unknown;
    assert_eq!(c.pass(4300).step, again);
    // After a restart too: what was answered is in the saved memory.
    let saved: Memory = serde_json::from_slice(&serde_json::to_vec(&c.memory).unwrap()).unwrap();
    assert_eq!(saved, c.memory);
    c.memory = saved;
    assert_eq!(c.pass(4400).step, again);
    assert!(c.memory.unanswered.is_empty());
}
