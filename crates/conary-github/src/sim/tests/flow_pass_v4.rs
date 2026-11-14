//! conary-plugin v4 from the flow's side: what its fix and judge requests
//! carry, through the real executor, runner and example plugin.
use super::flow_pass_case::{Case, REPO, example};
use super::head;
use crate::api::Source;
use crate::flow::{self, *};
use conary_core::plugin::{Answer, FindingAnswer, Problem, Task};

fn invocation(work: &flow::jobs::Work) -> flow::jobs::Invocation {
    serde_json::from_slice(&std::fs::read(&work.path).unwrap()).unwrap()
}

/// The example finds one problem on head 3, its fix answers it and pushes
/// head 4, and head 4's review is clean. The author's engine needs a judge,
/// which the next pass asks for.
fn fixed_then_reviewed(c: &mut Case, now: u64) {
    c.config.plugins.judge = Some(example());
    c.config.settings.judge_engines = ["fixture-author".into()].into();
    assert_eq!(
        c.pass(now, false)[0].next.as_ref().unwrap().step,
        Step::QueueReview("reviewer".into())
    );
    c.run_worker();
    c.pass(now + 1, false);
    assert_eq!(
        c.pass(now + 2, false)[0].next.as_ref().unwrap().step,
        Step::HandFix(Kind::Findings)
    );
    c.run_worker();
    assert_eq!(c.hub.pull(REPO, 3).unwrap().head.sha, head(4));
    c.hub.commit_details(
        &head(4),
        Some("change: fixture\n\nEngine: fixture-author\n"),
        Some("owner@example.org"),
        Some("owner@example.org"),
    );
    c.pass(now + 3, false);
    c.run_worker();
    c.pass(now + 4, false);
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
    let mut findings = vec![];
    let mut answered = vec![];
    let mut handed = vec![];
    for work in &record.jobs {
        let completion = flow::jobs::inspect(work, now + 6).unwrap();
        match (invocation(work).task, completion.result) {
            (
                Task::Review { pr, .. },
                Some(Ok(conary_core::plugin::Response {
                    answer: Answer::Review { findings: found },
                    ..
                })),
            ) => {
                assert_eq!(pr, 3);
                findings.extend(found);
            }
            (
                Task::Fix { pr, problems, .. },
                Some(Ok(conary_core::plugin::Response {
                    answer: Answer::Fix { answers, .. },
                    ..
                })),
            ) => {
                assert_eq!(pr, 3);
                // The example speaks v4: one answer for its one finding.
                let answers = answers.unwrap();
                assert_eq!(answers.len(), 1);
                let Problem::Finding { finding } = &problems[answers[0].problem] else {
                    panic!("an answer to a finding");
                };
                // Handed with the review it came from named in its text.
                assert!(finding.text.ends_with(&findings[0].text));
                handed.push(finding.clone());
                answered.extend(answers);
            }
            _ => {}
        }
    }
    assert_eq!(findings.len(), 1);
    let judge = record
        .jobs
        .iter()
        .find(|w| matches!(w.purpose, flow::jobs::Purpose::Judge))
        .unwrap();
    let Task::Judge {
        pr, why, context, ..
    } = invocation(judge).task
    else {
        panic!("not a judge");
    };
    assert_eq!(pr, 3);
    assert_eq!(why, [Trigger::AuthorEngine]);
    // The log: the review's finding, then the fix's answer to it as handed.
    let log = |answer| judging_context([(&findings[0], None), (&handed[0], answer)]);
    assert_eq!(context, log(Some(&answered[0])));
    // Another answer, or none, is another discussion.
    assert_ne!(context, log(None));
    let other = FindingAnswer {
        reply: "another reply".into(),
        ..answered[0].clone()
    };
    assert_ne!(context, log(Some(&other)));
    // Which problem index carried the answer doesn't change it.
    let moved = FindingAnswer {
        problem: answered[0].problem + 1,
        ..answered[0].clone()
    };
    assert_eq!(context, log(Some(&moved)));
}

/// A judgment counts only on the discussion a judge would be asked on now:
/// with the fix's answer in it, not as it was before the fix answered.
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
        fixed_then_reviewed(&mut c, now);
        let record = c.record();
        let mut findings = vec![];
        let mut answered = vec![];
        for work in &record.jobs {
            let Some(Ok(response)) = flow::jobs::inspect(work, now + 5).unwrap().result else {
                continue;
            };
            match (invocation(work).task, response.answer) {
                (_, Answer::Review { findings: found }) => {
                    findings.extend(found.into_iter().map(|f| (f, None)))
                }
                (Task::Fix { problems, .. }, Answer::Fix { answers, .. }) => {
                    for answer in answers.unwrap() {
                        let Problem::Finding { finding } = &problems[answer.problem] else {
                            panic!("an answer to a finding");
                        };
                        answered.push((finding.clone(), Some(answer)));
                    }
                }
                _ => {}
            }
        }
        let mut discussion = findings;
        if read_the_answer {
            discussion.extend(answered);
        }
        c.judgment_on(
            1,
            Judgment::Pass,
            true,
            Basis {
                why: vec![Trigger::AuthorEngine],
                context: judging_context(discussion.iter().map(|(f, a)| (f, a.as_ref()))),
            },
        );
        let report = c.pass(now + 5, true);
        assert_eq!(report[0].error, None);
        assert_eq!(report[0].next.as_ref().unwrap().step, expected);
    }
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
