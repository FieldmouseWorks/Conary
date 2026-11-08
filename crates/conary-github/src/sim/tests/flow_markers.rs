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
fn marker(h: &Hub, s: &Settings, name: &str, verdict: &str) -> String {
    let p = h.pull(REMI, 3).unwrap();
    let body = p.body.as_deref().unwrap_or("");
    let (key, repo, on, base) = if name == "major-verdict" {
        (
            s.major_key.as_ref().unwrap(),
            REMI.to_lowercase(),
            p.base.name.clone(),
            format!(" on {}", p.base.name),
        )
    } else {
        (
            s.judge_key.as_ref().unwrap(),
            REMI.to_owned(),
            format!("{} {}", p.base.name, fingerprint(body)),
            String::new(),
        )
    };
    format!(
        "<!-- {name}: {verdict} {}{base} {} -->",
        p.head.sha,
        key.seal(&repo, 3, name, verdict, &p.head.sha, &on)
    )
}
#[test]
fn judge_verdicts_are_read_from_sealed_comments() {
    // gates.py / misdescribed.py: each verdict is a scoped producer's answer.
    for (said, expected) in [
        ("land", Step::Merge),
        ("owner", Step::ListOwner),
        ("failed", Step::HandFix(Kind::JudgeFailure)),
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
        assert_eq!(next(&f, &m, 4000).0.step, Step::Judge);
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
                let on = format!(
                    "{} {}",
                    p.base.name,
                    fingerprint(p.body.as_deref().unwrap_or(""))
                );
                let key = if change == 5 {
                    SealKey::from_hex(&"d".repeat(64)).unwrap()
                } else {
                    s.judge_key.clone().unwrap()
                };
                let tag = key.seal(
                    if change == 4 { "other/repo" } else { REMI },
                    if change == 3 { 99 } else { 3 },
                    "autoland-judgment",
                    "land",
                    &p.head.sha,
                    &on,
                );
                body = format!("<!-- autoland-judgment: land {} {tag} -->", p.head.sha);
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
        assert_eq!(next(&f, &m, 4000).0.step, Step::Judge);
    }
}
#[test]
fn owner_release_is_observed_from_labels_for_this_revision_only() {
    // handle.py / gates.py: requesting the label is not the owner's answer.
    let (h, m) = setup();
    let s = settings();
    h.touch(&head(3), &["secrets/app"]);
    h.flow_comment(
        REMI,
        3,
        "owner",
        &marker(&h, &s, "autoland-judgment", "land"),
    );
    let (n, m) = next(&read_f(&h, s.clone()), &m, 4000);
    assert_eq!(n.step, Step::ListOwner);
    assert_eq!(m.owner_saw, None);
    h.label(REMI, 3, &["needs-peter"]);
    let (_, m) = next(&read_f(&h, s.clone()), &m, 4001);
    h.label(REMI, 3, &[]);
    assert_eq!(next(&read_f(&h, s), &m, 4002).0.step, Step::Merge);
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
