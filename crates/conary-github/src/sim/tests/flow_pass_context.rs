//! What a judge request's `context` digests is what GitHub shows, read
//! afresh: the flow and a judge producer call the same helper on the same
//! reads, and nothing the flow's own job records say goes in. Through the
//! real executor, for each place a finding or its answer lives.
use super::flow_pass_case::{Case, REPO, example};
use super::flow_pass_threads::{
    case, findings, respond, reviewed, step, threads, unchanged, until,
};
use super::{TRUNK, head};
use crate::api::Source;
use crate::flow::{self, *};
use conary_core::plugin::{Answer, Task};

/// A judge plugin, and an author engine that needs one.
fn judging(c: &mut Case) {
    c.config.plugins.judge = Some(example());
    c.config.settings.judge_engines = ["fixture-author".into()].into();
}

/// `sha` names the engine that needs a judge.
fn by_the_engine(c: &Case, sha: &str) {
    c.hub.commit_details(
        sha,
        Some("change: fixture\n\nEngine: fixture-author\n"),
        Some("owner@example.org"),
        Some("owner@example.org"),
    );
}

/// The reviewer looks again and finds nothing, and the flow asks a judge:
/// the `context` its request carries.
fn asked(c: &Case, now: u64, t: &mut u64) -> String {
    respond(c, now + *t, Answer::Review { findings: vec![] });
    for _ in 0..20 {
        *t += 60;
        if matches!(step(c, now + *t), Step::Judge(_)) {
            let path = c.deferred.0.borrow().last().unwrap().clone();
            let invocation: flow::jobs::Invocation =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let Task::Judge { context, .. } = invocation.task else {
                panic!("not a judge");
            };
            return context;
        }
    }
    panic!("never asked a judge");
}

/// The helper over a fresh read of GitHub alone.
fn on_github(c: &Case) -> String {
    read_judging_context(&c.hub, REPO, 3).unwrap()
}

/// The PR as the flow reads it now.
fn read(c: &Case) -> Facts {
    flow::read(
        &c.hub,
        &REPO.parse().unwrap(),
        3,
        &c.policy,
        c.config.settings.clone(),
        &c.config.trusted_authors,
    )
    .unwrap()
}

/// Rewrite every fix's saved answer to say `to` where it said `from`: the
/// flow's own records now disagree with what GitHub shows.
fn rewrite_fix_records(c: &Case, from: &str, to: &str) {
    let mut rewritten = 0;
    for work in c.record().jobs {
        if !matches!(work.purpose, flow::jobs::Purpose::Fix(_)) {
            continue;
        }
        let path = work.path.with_extension("answer");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains(from));
        std::fs::write(&path, saved.replace(from, to)).unwrap();
        rewritten += 1;
    }
    assert!(rewritten > 0);
}

/// Rewrite comment `index` of thread `id` on GitHub, as its author can.
fn edit_thread(c: &Case, id: &str, index: usize, body: &str) {
    let thread = threads(c).remove(id).unwrap();
    let mut comments: Vec<(String, String)> = thread
        .comments
        .into_iter()
        .map(|c| (c.author.unwrap(), c.body))
        .collect();
    comments[index].1 = body.into();
    let comments: Vec<(&str, &str)> = comments
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    c.hub.flow_thread_comments(REPO, 3, id, &comments);
}

/// Reviewed with [`findings`], answered by a fix that changes nothing, and
/// reviewed again clean: the context the judge the author's engine needs
/// is asked on.
fn answered_unchanged(c: &Case, now: u64) -> String {
    reviewed(c, now);
    assert_eq!(step(c, now + 2), Step::HandFix(Kind::Findings));
    unchanged(c, now + 2, "kept because");
    let mut t = 2;
    until(c, now, &mut t, Step::QueueReview("reviewer".into()));
    by_the_engine(c, &head(3));
    asked(c, now, &mut t)
}

#[test]
fn a_reviews_thread_findings_and_their_unchanged_answers_are_read_from_github() {
    let mut c = case(true);
    judging(&mut c);
    let now = flow::jobs::now();
    let context = answered_unchanged(&c, now);
    assert_eq!(context, on_github(&c));
    assert_eq!(context, read(&c).context());
    // Two findings in the diff have threads, each with its answer.
    let all = threads(&c);
    assert_eq!(all.len(), 2);
    assert!(all.values().all(|t| t.comments.len() == 2));
    // The flow's records of what its fix said don't count: GitHub does.
    rewrite_fix_records(&c, "kept because", "something else");
    assert_eq!(read(&c).context(), context);
    // An answer or a finding edited on its thread is another discussion.
    let id = all.keys().next().unwrap().clone();
    let answer = all[&id].comments[1].body.replacen("kept", "still kept", 1);
    edit_thread(&c, &id, 1, &answer);
    let edited = on_github(&c);
    assert_ne!(edited, context);
    assert_eq!(read(&c).context(), edited);
    let finding = all[&id].comments[0].body.replacen("P1", "P2", 1);
    edit_thread(&c, &id, 0, &finding);
    assert_ne!(on_github(&c), edited);
    assert_ne!(on_github(&c), context);
}

#[test]
fn a_reviews_comment_findings_and_their_unchanged_answers_are_read_from_github() {
    let mut c = case(false);
    judging(&mut c);
    let now = flow::jobs::now();
    let context = answered_unchanged(&c, now);
    assert_eq!(context, on_github(&c));
    assert_eq!(context, read(&c).context());
    // No diff: every finding and every answer is a marked PR comment.
    assert!(threads(&c).is_empty());
    assert_eq!(read(&c).conversation.len(), 2 * findings().len());
    rewrite_fix_records(&c, "kept because", "something else");
    assert_eq!(read(&c).context(), context);
    // A finding edited, its marker kept, is another discussion.
    let comments = c.hub.comments(REPO, 3).unwrap();
    let at = comments
        .iter()
        .position(|c| {
            c.body
                .as_deref()
                .is_some_and(|b| b.contains("conary-finding:v1"))
        })
        .unwrap();
    let edited = comments[at]
        .body
        .as_deref()
        .unwrap()
        .replacen("P1", "P2", 1);
    c.hub.edit_flow_comment(REPO, 3, at, &edited);
    assert_ne!(on_github(&c), context);
    assert_eq!(read(&c).context(), on_github(&c));
}

#[test]
fn a_carried_reviews_findings_and_answers_are_read_from_github() {
    let mut c = case(true);
    judging(&mut c);
    let now = flow::jobs::now();
    reviewed(&c, now);
    // Trunk moves and is merged in: the review on head 3 carries to head 4.
    let tip = c.hub.add(TRUNK, "trunk-only patch");
    c.hub.set_branch(REPO, "main", &tip);
    let mut patches = c.hub.patches(&tip);
    patches.extend(c.hub.patches(&head(3)));
    c.hub.commit_as(&head(4), &[head(3), tip], patches);
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    let mut t = 2;
    until(&c, now, &mut t, Step::HandFix(Kind::Findings));
    unchanged(&c, now + t, "carried");
    until(&c, now, &mut t, Step::QueueReview("reviewer".into()));
    by_the_engine(&c, &head(4));
    let context = asked(&c, now, &mut t);
    assert_eq!(context, on_github(&c));
    assert_eq!(context, read(&c).context());
    rewrite_fix_records(&c, "carried", "something else");
    assert_eq!(read(&c).context(), context);
}

#[test]
fn the_conversations_findings_and_answers_are_what_a_judge_reads() {
    let c = case(false);
    let now = flow::jobs::now();
    reviewed(&c, now);
    assert_eq!(step(&c, now + 2), Step::HandFix(Kind::Findings));
    unchanged(&c, now + 2, "answer");
    until(&c, now, &mut 2, Step::QueueReview("reviewer".into()));
    // What the flow reads: the conversation, and the context it makes,
    // which is the helper's over GitHub alone.
    let facts = || {
        let f = read(&c);
        assert_eq!(f.context(), on_github(&c));
        (f.conversation.clone(), f.context())
    };
    let (before, context) = facts();
    // Four findings and four answers; a sealed verdict or a person's word
    // isn't the findings' discussion.
    assert_eq!(before.len(), 8);
    c.hub.flow_comment(REPO, 3, "someone", "just a comment");
    assert_eq!(facts(), (before.clone(), context.clone()));
    // An answer edited, its marker kept, is another discussion; so is one
    // removed.
    let comments = c.hub.comments(REPO, 3).unwrap();
    let at = comments
        .iter()
        .position(|c| {
            c.body
                .as_deref()
                .is_some_and(|b| b.contains("conary-answer:v1"))
        })
        .unwrap();
    let edited = comments[at]
        .body
        .as_deref()
        .unwrap()
        .replacen("answer", "a different answer", 1);
    c.hub.edit_flow_comment(REPO, 3, at, &edited);
    let (after, edited_context) = facts();
    assert_ne!(after, before);
    assert_ne!(edited_context, context);
    c.hub.edit_flow_comment(REPO, 3, at, "removed");
    assert_ne!(facts().1, context);
    assert_ne!(facts().1, edited_context);
}
