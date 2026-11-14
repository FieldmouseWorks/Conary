//! A judgment speaks for its revision only on what it was judged on: every
//! reason the flow requires now, and the discussion as it is now.
use super::flow_case::Case;
use crate::flow::*;
use conary_core::plugin::{Finding, FindingAnswer, Severity};

fn finding() -> Finding {
    Finding {
        path: "src/lib.rs".into(),
        line: 1,
        severity: Severity::P1,
        text: "a finding".into(),
    }
}

fn answer() -> FindingAnswer {
    FindingAnswer {
        problem: 0,
        fixed: false,
        reply: "not a bug: the caller checks it".into(),
    }
}

/// Five rounds of findings, answered: past the round limit, with one
/// finding in the discussion, the flow asks a judge.
fn capped() -> Case {
    let mut c = Case::new();
    c.roles(&["primary", "secondary"]);
    for t in 1..=5 {
        c.review("primary", Verdict::Findings, t);
    }
    c.answered("primary");
    c.discussion = vec![(finding(), None)];
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
        c.discussion.push((finding(), Some(answer())));
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
        context: judging_context([]),
    };
    c.judge_on(Judgment::Pass, stale);
    assert_eq!(c.view(4005).step, asked);
}
