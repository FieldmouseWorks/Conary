//! A job's GitHub comment is a verdict only with the right scoped seal.
use super::head;
use crate::api::Source;
use crate::flow::*;
use crate::sim::{Hub, REMI};
use serde_json::json;

fn settings() -> Settings {
    Settings {
        judge_key: Some(SealKey::from_hex(&"a".repeat(64)).unwrap()),
        major_key: Some(SealKey::from_hex(&"b".repeat(64)).unwrap()),
        verdict_authors: ["owner".into()].into(),
        ..Settings::default()
    }
}
fn setup() -> (Hub, Memory) {
    let (h, _, m) = super::flow::setup();
    (h, m)
}
fn read_f(h: &Hub, s: Settings) -> Facts {
    read(
        h,
        &REMI.parse().unwrap(),
        3,
        &conary_core::Policy::parse(br#"{"r2":["^secrets/"]}"#).unwrap(),
        s,
        &["owner".into()],
    )
    .unwrap()
}

fn provenance() -> conary_core::plugin::Provenance {
    use conary_core::plugin::{Context, Executed, Identity, Provenance};
    Provenance {
        by: Identity {
            engine: "fixture-engine".into(),
            session: Some("judge-session".into()),
        },
        execution: Executed {
            model: Some("fixture-model".into()),
            effort: Some("deliberate".into()),
            context: Context::Fresh,
        },
    }
}

#[test]
fn sealed_judge_provenance_is_retained_in_memory_and_bound_to_the_receipt() {
    let (h, memory) = setup();
    let s = settings();
    let revision = Revision::from(&h.pull(REMI, 3).unwrap());
    let by = provenance();
    let body = s
        .judge_key
        .as_ref()
        .unwrap()
        .judgment_by(REMI, 3, Judgment::Pass, &revision, 1, by.clone())
        .unwrap();
    h.flow_comment(REMI, 3, "owner", &body);
    let f = read_f(&h, s.clone());
    assert_eq!(f.judgment.as_ref().unwrap().provenance.as_ref(), Some(&by));
    let (_, retained) = next(&f, &memory, 4000);
    let roundtrip: Memory =
        serde_json::from_slice(&serde_json::to_vec(&retained).unwrap()).unwrap();
    assert_eq!(retained, roundtrip);
    let mut absent = read_f(&h, s.clone());
    absent.judgment = None;
    absent.judgment_generation = None;
    let (_, retained_again) = next(&absent, &roundtrip, 4001);
    assert_eq!(retained_again.judge_issuance, roundtrip.judge_issuance);

    let receipt = judgment_receipt_by(&revision, 1, Some(Judgment::Pass), Some(&by));
    let evidence: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
    assert_eq!(evidence["provenance"]["by"]["engine"], "fixture-engine");
    assert_eq!(
        evidence["provenance"]["execution"]["model"],
        "fixture-model"
    );
    let mut different = by.clone();
    different.execution.model = Some("alternate-model".into());
    assert_ne!(
        receipt_digest(&receipt),
        receipt_digest(&judgment_receipt_by(
            &revision,
            1,
            Some(Judgment::Pass),
            Some(&different)
        ))
    );
    // The MAC covers provenance: copying the seal and changing the model fails.
    let tampered = body.replace("fixture-model", "alternate-model");
    h.flow_comment(REMI, 3, "owner", &tampered);
    assert_eq!(
        read_f(&h, s.clone())
            .judgment
            .as_ref()
            .unwrap()
            .provenance
            .as_ref(),
        Some(&by)
    );
    // Two authenticated, different provenance claims at one generation conflict.
    let conflict = s
        .judge_key
        .as_ref()
        .unwrap()
        .judgment_by(REMI, 3, Judgment::Pass, &revision, 1, different)
        .unwrap();
    h.flow_comment(REMI, 3, "owner", &conflict);
    assert!(read_f(&h, s).judgment.is_none());
}
fn marker(h: &Hub, s: &Settings, name: &str, verdict: &str) -> String {
    let p = h.pull(REMI, 3).unwrap();
    let key = if name == "major-verdict" {
        s.major_key.as_ref().unwrap()
    } else {
        s.judge_key.as_ref().unwrap()
    };
    key.decision(
        REMI,
        3,
        name,
        verdict,
        &Revision::from(&p),
        h.comments(REMI, 3).unwrap().len() as u64 + 1,
    )
}
#[test]
fn judge_verdicts_are_read_from_sealed_comments() {
    // gates.py / misdescribed.py: each verdict is a scoped producer's answer.
    for (said, expected) in [
        ("pass", Step::Merge),
        ("owner", Step::ListOwner),
        ("fail", Step::HandFix(Kind::JudgeFailure)),
        // Old steward words, read once until Conary#256 slice 5 deletes them.
        ("land", Step::Merge),
        ("failed", Step::HandFix(Kind::JudgeFailure)),
        // An unknown word is no judgment: the flow asks the judge again.
        (
            "approve",
            Step::Judge(vec![Trigger::AuthorEngine, Trigger::Asked]),
        ),
        ("misdescribed", Step::HandFix(Kind::Description)),
    ] {
        let (h, m) = setup();
        let s = settings();
        h.commit_details(
            &head(3),
            Some("change: code\n\nEngine: codex\n"),
            Some("author@example.org"),
            Some("author@example.org"),
        );
        h.flow_comment(REMI, 3, "owner", &marker(&h, &s, "autoland-judgment", said));
        assert_eq!(next(&read_f(&h, s), &m, 4000).0.step, expected);
    }
}
#[test]
fn a_judge_plugin_answer_round_trips_through_its_marker_to_a_decision() {
    use conary_core::plugin::{Answer, Judgment};
    for (verdict, expected) in [
        (Judgment::Pass, Step::Merge),
        (Judgment::Fail, Step::HandFix(Kind::JudgeFailure)),
        (Judgment::Owner, Step::ListOwner),
        (Judgment::Misdescribed, Step::HandFix(Kind::Description)),
    ] {
        let (h, m) = setup();
        let s = settings();
        h.commit_details(
            &head(3),
            Some("change: code\n\nEngine: codex\n"),
            Some("author@example.org"),
            Some("author@example.org"),
        );
        let answer = Answer::Judge {
            verdict,
            why: "because".into(),
        };
        let wire: Answer = serde_json::from_str(&serde_json::to_string(&answer).unwrap()).unwrap();
        let revision = crate::flow::Revision::from(&h.pull(REMI, 3).unwrap());
        let judged = crate::flow::Judged::from_answer(revision, &wire).unwrap();
        h.flow_comment(
            REMI,
            3,
            "owner",
            &marker(&h, &s, "autoland-judgment", judged.verdict.word()),
        );
        let f = read_f(&h, s);
        assert_eq!(f.judgment.as_ref().map(|j| j.verdict), Some(verdict));
        assert_eq!(next(&f, &m, 4000).0.step, expected);
    }
}
#[test]
fn wrong_author_or_missing_seal_never_counts_as_a_judgment() {
    // gates.py: a quoted well-formed marker is not the producer's verdict.
    for who in ["outside", "owner[bot]", "owner"] {
        let (h, m) = setup();
        let s = settings();
        h.commit_details(
            &head(3),
            Some("change: code\n\nEngine: codex\n"),
            Some("a"),
            Some("a"),
        );
        let body = if who == "owner" {
            format!("<!-- autoland-judgment: land {} -->", head(3))
        } else {
            marker(&h, &s, "autoland-judgment", "land")
        };
        h.flow_comment(REMI, 3, who, &body);
        let f = read_f(&h, s);
        assert_eq!(f.judgment, None);
        assert_eq!(
            next(&f, &m, 4000).0.step,
            Step::Judge(vec![Trigger::AuthorEngine])
        );
    }
}
#[test]
fn a_seal_from_another_head_base_description_or_pr_is_not_reused() {
    // gates.py / misdescribed.py: every scope component is authenticated.
    for change in 0..6 {
        let (h, m) = setup();
        let s = settings();
        let mut body = marker(&h, &s, "autoland-judgment", "land");
        match change {
            0 => {
                h.push(REMI, 3, &head(4));
            }
            1 => {
                h.set_branch(REMI, "release", super::TRUNK);
                h.retarget(REMI, 3, "release");
            }
            2 => h.edit_pull(REMI, 3, |p| p["body"] = json!("new reasons")),
            3..=5 => {
                let p = h.pull(REMI, 3).unwrap();
                let key = if change == 5 {
                    SealKey::from_hex(&"d".repeat(64)).unwrap()
                } else {
                    s.judge_key.clone().unwrap()
                };
                body = key.decision(
                    if change == 4 { "other/repo" } else { REMI },
                    if change == 3 { 99 } else { 3 },
                    "autoland-judgment",
                    "land",
                    &Revision::from(&p),
                    1,
                );
            }
            _ => unreachable!(),
        }
        h.refresh_flow(REMI, 3);
        h.flow_review(REMI, 3, "reviewer", Verdict::Clean, 1);
        let p = h.pull(REMI, 3).unwrap();
        h.ci(&p.head.sha, &[("check", "success")]);
        h.commit_details(
            &p.head.sha,
            Some("change: code\n\nEngine: codex\n"),
            Some("a"),
            Some("a"),
        );
        h.flow_comment(REMI, 3, "owner", &body);
        let f = read_f(&h, s);
        assert_eq!(f.judgment, None);
        // A foreign seal may also read as an ask; the engine always holds.
        assert!(matches!(
            next(&f, &m, 4000).0.step,
            Step::Judge(why) if why.contains(&Trigger::AuthorEngine)
        ));
    }
}
#[test]
fn owner_release_requires_an_authenticated_review_for_this_revision() {
    // handle.py / gates.py: requesting the label is not the owner's answer.
    let (h, m) = setup();
    let mut s = settings();
    s.people.insert("maintainer".into());
    h.touch(&head(3), &["secrets/app"]);
    h.flow_comment(
        REMI,
        3,
        "owner",
        &marker(&h, &s, "autoland-judgment", "land"),
    );
    let (n, m) = next(&read_f(&h, s.clone()), &m, 4000);
    assert_eq!(n.step, Step::ListOwner);
    assert!(!read_f(&h, s.clone()).owner_approved);
    h.label(REMI, 3, &["needs-peter"]);
    let (_, m) = next(&read_f(&h, s.clone()), &m, 4001);
    h.label(REMI, 3, &[]);
    assert_eq!(
        next(&read_f(&h, s.clone()), &m, 4002).0.step,
        Step::ListOwner
    );
    let revision = Revision::from(&crate::api::Source::pull(&h, REMI, 3).unwrap());
    h.person_review(
        REMI,
        3,
        "maintainer",
        "User",
        &revision.head,
        &owner_approval(&REMI.parse().unwrap(), 3, &revision),
        "APPROVED",
    );
    assert_eq!(next(&read_f(&h, s), &m, 4003).0.step, Step::Merge);
}
#[test]
fn major_sweep_verdict_is_head_base_repo_and_key_scoped() {
    // deps.py: a major's verdict is checked with the sweep's separate key.
    for (verdict, expected) in [("ok", Some(Major::Cleared)), ("held", Some(Major::Held))] {
        let (h, _) = setup();
        let s = settings();
        h.flow_comment(REMI, 3, "owner", &marker(&h, &s, "major-verdict", verdict));
        assert_eq!(read_f(&h, s.clone()).major, expected);
        let mut wrong = s;
        wrong.major_key = Some(SealKey::from_hex(&"c".repeat(64)).unwrap());
        assert_eq!(read_f(&h, wrong).major, None);
    }
}
#[test]
fn signing_configuration_does_not_print_keys() {
    let s = settings();
    let shown = format!("{s:?}");
    assert!(!shown.contains(&"a".repeat(64)));
    assert!(!shown.contains(&"b".repeat(64)));
}

#[test]
fn burrows_fingerprint_and_seal_wire_formats_are_preserved() {
    // seal.py / common.py: fixed Python-generated vector, independent of the Rust verifier.
    assert_eq!(
        fingerprint("  Why café?\n\nThis is the change.  "),
        "cdbb8552558be0c8"
    );
    let key = SealKey::from_hex(&"a".repeat(64)).unwrap();
    assert_eq!(
        key.seal(
            "o/r",
            7,
            "autoland-judgment",
            "land",
            &"1".repeat(40),
            "main cdbb8552558be0c8"
        ),
        "5e205c8a699161f5c2ff24e5dc48f55e"
    );
}

#[test]
fn flow_config_refuses_malformed_seal_keys() {
    let config = |field: &str, key: &str| {
        serde_json::from_value::<Config>(json!({"settings": {field: key}}))
    };
    for field in ["judge_key", "major_key"] {
        for bad in [
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            format!("{}g", "a".repeat(63)),
        ] {
            let err = config(field, &bad).unwrap_err().to_string();
            assert!(
                err.contains("64 lowercase hexadecimal"),
                "{field} {bad}: {err}"
            );
        }
        let good = config(field, &"c".repeat(64)).unwrap().settings;
        let want = Some(SealKey::from_hex(&"c".repeat(64)).unwrap());
        assert_eq!(
            if field == "judge_key" {
                good.judge_key
            } else {
                good.major_key
            },
            want
        );
    }
}
