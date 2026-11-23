//! conary-plugin v4 from the flow's side: what its fix and judge requests
//! carry, through the real executor, runner and example plugin.
use super::flow_pass_case::{Case, REPO, example};
use super::head;
use crate::api::Source;
use crate::flow::{self, *};
use conary_core::plugin::{Answer, Finding, Problem, Severity, Task};

fn invocation(work: &flow::jobs::Work) -> flow::jobs::Invocation {
    serde_json::from_slice(&std::fs::read(&work.path).unwrap()).unwrap()
}

/// `flow::judging_context` over a fresh read of GitHub alone.
fn context_now(c: &Case) -> String {
    read_judging_context(&c.hub, REPO, 3).unwrap()
}

/// The finding the connector's review left open on head 3, on its thread.
fn finding() -> Finding {
    Finding {
        path: "src/lib.rs".into(),
        line: 1,
        severity: Severity::P1,
        text: "Demonstration finding on the first simulated head".into(),
    }
}

/// The connector's review left one finding open on head 3, the example's fix
/// answers it and pushes head 4, and the connector reviews head 4 clean.
/// The author's engine needs a judge, which the next pass asks for. Returns
/// the context GitHub showed before the fix's answer went up.
fn fixed_then_reviewed(c: &mut Case, now: u64) -> String {
    c.config.plugins.judge = Some(example());
    c.config.settings.judge_engines = ["fixture-author".into()].into();
    c.connector_findings(&[finding()], now);
    c.pass(now, false);
    assert_eq!(
        c.pass(now + 1, false)[0].next.as_ref().unwrap().step,
        Step::HandFix(Kind::Findings)
    );
    let fix = format!("3-{}", c.record().sequence);
    c.run_worker();
    assert_eq!(c.hub.pull(REPO, 3).unwrap().head.sha, head(4));
    c.hub.commit_details(
        &head(4),
        Some("change: fixture\n\nEngine: fixture-author\n"),
        Some("owner@example.org"),
        Some("owner@example.org"),
    );
    let unanswered = context_now(c);
    // Its answer goes up, the PR is read again, and head 4 is reviewed.
    assert_eq!(
        c.pass(now + 3, false)[0].next.as_ref().unwrap().step,
        Step::PostFix(fix)
    );
    c.pass(now + 3, false);
    c.pass(now + 3, false);
    c.clean_review();
    unanswered
}

#[test]
fn a_judge_after_a_fix_carries_its_pr_reasons_and_the_answered_finding() {
    let mut c = Case::new();
    let now = flow::jobs::now();
    fixed_then_reviewed(&mut c, now);
    assert_eq!(
        c.pass(now + 5, false)[0].next.as_ref().unwrap().step,
        Step::Judge(vec![Trigger::AuthorEngine])
    );
    let record = c.record();
    let findings = [finding()];
    let mut answered = vec![];
    let mut fixed_by = None;
    for work in &record.jobs {
        let completion = flow::jobs::inspect(work, now + 6).unwrap();
        if let (
            Task::Fix { pr, problems, .. },
            Some(Ok(conary_core::plugin::Response {
                answer: Answer::Fix { answers, .. },
                by,
                ..
            })),
        ) = (invocation(work).task, completion.result)
        {
            assert_eq!(pr, 3);
            fixed_by = Some(by);
            // The example speaks v4: one answer for its one finding.
            let answers = answers.unwrap();
            assert_eq!(answers.len(), 1);
            let Problem::Finding { finding } = &problems[answers[0].problem] else {
                panic!("an answer to a finding");
            };
            // Handed with the thread it came from named in its text.
            assert!(finding.text.contains("GitHub review thread PRRT_"));
            assert!(finding.text.contains(&findings[0].text));
            answered.extend(answers);
        }
    }
    assert_eq!(answered.len(), 1);
    let judge = record
        .jobs
        .iter()
        .find(|w| matches!(w.purpose, flow::jobs::Purpose::Judge))
        .unwrap();
    let Task::Judge {
        pr,
        why,
        context,
        author,
        ..
    } = invocation(judge).task
    else {
        panic!("not a judge");
    };
    assert_eq!(pr, 3);
    // The fix pushed the head: its validated answer names who wrote it,
    // session and all, so the fix's session can't judge its own push.
    let fixed_by = fixed_by.unwrap();
    assert!(fixed_by.session.is_some());
    assert_eq!(author, fixed_by);
    assert_eq!(why, [Trigger::AuthorEngine]);
    // What GitHub shows, read afresh: the review's finding and the fix's
    // answer to it are both there.
    assert_eq!(context, context_now(&c));
    let shown = |text: &str| {
        flow::Read::snapshot(&c.hub, REPO, 3)
            .unwrap()
            .threads
            .values()
            .flat_map(|t| &t.comments)
            .any(|c| c.body.contains(text))
    };
    assert!(shown(&findings[0].text));
    assert!(shown(&answered[0].reply));
}

/// A judgment counts only on the discussion a judge would be asked on now:
/// with the fix's answer on GitHub in it, not as GitHub showed it before the
/// fix answered.
#[test]
fn a_judgment_counts_only_on_the_discussion_the_flow_reads_now() {
    for (read_the_answer, expected) in [
        (true, Step::Merge),
        (
            false,
            Step::Judge(vec![Trigger::AuthorEngine, Trigger::Asked]),
        ),
    ] {
        let mut c = Case::new();
        let now = flow::jobs::now();
        let unanswered = fixed_then_reviewed(&mut c, now);
        assert_ne!(unanswered, context_now(&c));
        c.judgment_on(
            1,
            Judgment::Pass,
            true,
            Basis {
                why: vec![Trigger::AuthorEngine],
                context: if read_the_answer {
                    context_now(&c)
                } else {
                    unanswered
                },
            },
        );
        let report = c.pass(now + 5, true);
        assert_eq!(report[0].error, None);
        assert_eq!(report[0].next.as_ref().unwrap().step, expected);
    }
}

/// The producer answers each judge with an issuance sealing what it was
/// asked on. Then a finding or answer on GitHub alone, resolved without a
/// push, makes that judgment stale: the flow asks a judge again at once,
/// every time, and never takes the answered judges for missing ones.
#[test]
fn a_judgment_made_stale_on_github_is_asked_again_each_time() {
    let mut c = Case::new();
    let now = flow::jobs::now();
    fixed_then_reviewed(&mut c, now);
    let step = |c: &Case, t: u64, dry: bool| {
        let report = c.pass(now + t, dry).remove(0);
        assert_eq!(report.error, None, "{t}");
        report.next.unwrap().step
    };
    assert_eq!(step(&c, 5, false), Step::Judge(vec![Trigger::AuthorEngine]));
    let discussion: [&[(&str, &str)]; 3] = [
        &[("person", "a finding")],
        &[("person", "a finding"), ("owner", "an answer")],
        &[("person", "a finding"), ("owner", "an edited answer")],
    ];
    for (round, comments) in discussion.into_iter().enumerate() {
        let t = 10 + 10 * round as u64;
        c.run_worker();
        assert_eq!(step(&c, t, true), Step::Merge, "{round}");
        c.hub.flow_thread_comments(REPO, 3, "T1", comments);
        c.hub.flow_thread(REPO, 3, "T1", true);
        assert!(matches!(step(&c, t + 1, false), Step::Judge(_)), "{round}");
    }
    assert!(c.record().memory.unanswered.is_empty());
}

/// A judge started because of the author engine. Then the engine trailer is
/// gone, the description changes, and that judge ends without an answer. The
/// flow keeps the judge owed on that head and base and asks again: the new
/// request carries the first judge's reasons, or `started` when that judge
/// predates recorded reasons.
fn reasked(predates_reasons: bool) -> Vec<Trigger> {
    let mut c = Case::new();
    c.config.plugins.judge = Some(example());
    c.config.settings.judge_engines = ["fixture-author".into()].into();
    c.config.settings.job_timeout = 1;
    c.clean_review();
    let engine = |trailer: &str| {
        c.hub.commit_details(
            &head(3),
            Some(&format!("change: fixture\n\n{trailer}")),
            Some("owner@example.org"),
            Some("owner@example.org"),
        )
    };
    engine("Engine: fixture-author\n");
    let now = flow::jobs::now();
    assert_eq!(
        c.pass(now, false)[0].next.as_ref().unwrap().step,
        Step::Judge(vec![Trigger::AuthorEngine])
    );
    let judges = |c: &Case| {
        c.record()
            .jobs
            .into_iter()
            .filter(|w| matches!(w.purpose, flow::jobs::Purpose::Judge))
            .collect::<Vec<_>>()
    };
    let first = judges(&c).remove(0);
    if predates_reasons {
        // What a judge invocation saved before v4 decodes to.
        let mut saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&first.path).unwrap()).unwrap();
        let task = saved["task"].as_object_mut().unwrap();
        for field in ["pr", "why", "context"] {
            task.remove(field);
        }
        std::fs::write(&first.path, saved.to_string()).unwrap();
    }
    engine("Session: none\n");
    c.hub
        .edit_pull(REPO, 3, |p| p["body"] = serde_json::json!("new reasons"));
    c.hub.refresh_flow(REPO, 3);
    c.clean_review();
    // The first judge never answers; its deadline passes.
    let mut step = None;
    for t in 1..40 {
        let report = c.pass(now + t * 120, false);
        assert_eq!(report[0].error, None);
        step = report[0].next.clone().map(|n| n.step);
        if judges(&c).len() > 1 {
            break;
        }
    }
    let all = judges(&c);
    assert_eq!(all.len(), 2, "never asked again: {step:?}");
    assert_eq!(step, Some(Step::Judge(vec![])));
    assert_ne!(all[0].job.revision, all[1].job.revision);
    let Task::Judge { why, .. } = invocation(&all[1]).task else {
        panic!("not a judge");
    };
    why
}

#[test]
fn a_judge_owed_on_its_head_and_base_keeps_its_reasons_when_its_trigger_goes() {
    assert_eq!(reasked(false), [Trigger::AuthorEngine]);
}

#[test]
fn a_judge_owed_from_before_v4_is_asked_again_as_started() {
    assert_eq!(reasked(true), [Trigger::Started]);
}
