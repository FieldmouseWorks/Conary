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

#[test]
fn a_judge_after_a_fix_carries_its_pr_reasons_and_the_answered_finding() {
    let mut c = Case::new();
    c.config.plugins.judge = Some(example());
    c.config.settings.judge_engines = ["fixture-author".into()].into();
    let now = flow::jobs::now();
    // The example finds one problem on head 3, and its fix pushes head 4.
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
