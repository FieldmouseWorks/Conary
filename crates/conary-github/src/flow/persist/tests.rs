use super::*;
use serde_json::{Value, json};

// Serialized by Record on the PR's base, 8d7719a. It contains every legacy
// verdict on both current and stale heads, plus conflict tombstones on both.
const BASE_RECORD: &str = include_str!("base-record.json");

fn legacy_words() -> [(&'static str, Judgment); 4] {
    [
        ("Land", Judgment::Pass),
        ("Failing", Judgment::Fail),
        ("Owner", Judgment::Owner),
        ("Misdescribed", Judgment::Misdescribed),
    ]
}

fn canonical_record() -> Value {
    let mut value: Value = serde_json::from_str(BASE_RECORD).unwrap();
    for entry in value["memory"]["judge_issuance"].as_array_mut().unwrap() {
        let word = &mut entry[1][1];
        if let Some((_, verdict)) = legacy_words()
            .iter()
            .find(|(old, _)| word.as_str() == Some(*old))
        {
            *word = json!(verdict.word());
        }
    }
    value
}

fn fixture() -> (tempfile::TempDir, conary_core::op::Repo, Data) {
    let root = tempfile::tempdir().unwrap();
    let repo = "owner/repo".parse().unwrap();
    let data = Data::open(root.path(), &repo).unwrap();
    std::fs::write(data.path(3), BASE_RECORD).unwrap();
    (root, repo, data)
}

#[test]
fn base_records_migrate_without_resetting_state() {
    let (root, repo, data) = fixture();
    let expected = canonical_record();
    for record in [
        data.load(3).unwrap(),
        Data::read(root.path(), &repo, 3).unwrap(),
    ] {
        for (index, (_, verdict)) in legacy_words().into_iter().enumerate() {
            for head in ["current-head", "stale-head"] {
                let revision = Revision {
                    head: head.into(),
                    base: "base".into(),
                    description: format!("verdict-{index}"),
                };
                assert_eq!(
                    record.memory.judge_issuance[&revision],
                    (100 + index as u64, Some(verdict))
                );
            }
        }
        // Compare the whole record: unrelated memory, permissions, pending
        // landings, jobs, decisions, failures and counters must all survive.
        assert_eq!(serde_json::to_value(&record).unwrap(), expected);
        assert_eq!(std::fs::read_to_string(data.path(3)).unwrap(), BASE_RECORD);
        data.save(4, &record).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(data.path(4)).unwrap()).unwrap();
        assert_eq!(saved, expected);
        assert_eq!(
            serde_json::to_value(data.load(4).unwrap()).unwrap(),
            expected
        );
        assert_eq!(
            serde_json::to_value(Data::read(root.path(), &repo, 4).unwrap()).unwrap(),
            expected
        );
    }
}

#[test]
fn migrated_issuances_keep_high_water_marks_and_conflict_tombstones() {
    let (_root, _repo, data) = fixture();
    let record = data.load(3).unwrap();
    for (revision, (generation, verdict)) in &record.memory.judge_issuance {
        let mut receipts = record.memory.judge_issuance.clone();
        let observe = super::super::state::issuance;
        assert_eq!(observe(&mut receipts, revision, None, None), *verdict);
        assert_eq!(
            observe(
                &mut receipts,
                revision,
                Some(generation - 1),
                Some(Judgment::Pass)
            ),
            *verdict
        );
        // A conflicting same-generation word must become/stay a tombstone,
        // including on stale revisions. It must never restore permission.
        let conflicting = if *verdict == Some(Judgment::Pass) {
            Judgment::Fail
        } else {
            Judgment::Pass
        };
        assert_eq!(
            observe(
                &mut receipts,
                revision,
                Some(*generation),
                Some(conflicting)
            ),
            None
        );
        assert_eq!(receipts[revision], (*generation, None));
        assert_eq!(
            observe(
                &mut receipts,
                revision,
                Some(*generation),
                Some(Judgment::Pass)
            ),
            None
        );
        assert_eq!(
            observe(
                &mut receipts,
                revision,
                Some(generation + 1),
                Some(Judgment::Pass)
            ),
            Some(Judgment::Pass)
        );
        assert_eq!(receipts[revision], (generation + 1, Some(Judgment::Pass)));
    }
}

#[test]
fn persisted_judgments_accept_canonical_words_and_reject_unknowns() {
    let (root, repo, data) = fixture();
    let mut value = canonical_record();
    std::fs::write(data.path(3), serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(data.load(3).unwrap()).unwrap(), value);
    assert_eq!(
        serde_json::to_value(Data::read(root.path(), &repo, 3).unwrap()).unwrap(),
        value
    );
    value["memory"]["judge_issuance"][0][1][1] = json!("unknown");
    std::fs::write(data.path(3), serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(data.load(3).is_err());
    assert!(Data::read(root.path(), &repo, 3).is_err());
}

#[test]
fn persisted_legacy_words_do_not_expand_the_plugin_contract() {
    use conary_core::plugin::Answer;
    for (legacy, verdict) in legacy_words() {
        let answer = |word| json!({"kind":"judge","verdict":word,"why":"fixture"});
        assert!(serde_json::from_value::<Answer>(answer(legacy)).is_err());
        assert_eq!(Judgment::from_word(legacy), None);
        assert!(
            matches!(serde_json::from_value::<Answer>(answer(verdict.word())).unwrap(), Answer::Judge { verdict: v, .. } if v == verdict)
        );
    }
    for legacy in ["land", "failed"] {
        assert!(
            serde_json::from_value::<Answer>(
                json!({"kind":"judge","verdict":legacy,"why":"fixture"})
            )
            .is_err()
        );
        assert_eq!(Judgment::from_word(legacy), None);
    }
}
