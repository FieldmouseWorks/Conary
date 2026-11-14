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
                    (100 + index as u64, Some(JudgeResult::Legacy(verdict)))
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
    let pass = || Some(JudgeResult::Legacy(Judgment::Pass));
    for (revision, (generation, verdict)) in &record.memory.judge_issuance {
        let mut receipts = record.memory.judge_issuance.clone();
        let observe = super::super::state::issuance;
        assert_eq!(observe(&mut receipts, revision, None, None), *verdict);
        assert_eq!(
            observe(&mut receipts, revision, Some(generation - 1), pass()),
            *verdict
        );
        // A conflicting same-generation word must become/stay a tombstone,
        // including on stale revisions. It must never restore permission.
        let conflicting = if *verdict == pass() {
            JudgeResult::Legacy(Judgment::Fail)
        } else {
            JudgeResult::Legacy(Judgment::Pass)
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
            observe(&mut receipts, revision, Some(*generation), pass()),
            None
        );
        assert_eq!(
            observe(&mut receipts, revision, Some(generation + 1), pass()),
            pass()
        );
        assert_eq!(receipts[revision], (generation + 1, pass()));
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
fn issuances_with_provenance_persist_in_the_contracts_words() {
    let (root, repo, data) = fixture();
    let mut record = data.load(3).unwrap();
    let revision = Revision {
        head: "provenanced-head".into(),
        base: "base".into(),
        description: "provenance".into(),
    };
    let recorded = JudgeResult::Recorded {
        verdict: Judgment::Misdescribed,
        provenance: provenance(),
        basis: Some(Basis {
            why: vec![Trigger::RoundLimit],
            context: "c".repeat(64),
        }),
    };
    record
        .memory
        .judge_issuance
        .insert(revision.clone(), (7, Some(recorded.clone())));
    data.save(4, &record).unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(data.path(4)).unwrap()).unwrap();
    let entry = saved["memory"]["judge_issuance"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e[0]["head"] == "provenanced-head")
        .unwrap();
    assert_eq!(entry[1][1]["verdict"], "misdescribed");
    assert_eq!(entry[1][1]["provenance"]["by"]["engine"], "fixture-engine");
    assert_eq!(entry[1][1]["basis"]["why"][0], "round_limit");
    for loaded in [
        data.load(4).unwrap(),
        Data::read(root.path(), &repo, 4).unwrap(),
    ] {
        assert_eq!(
            loaded.memory.judge_issuance[&revision],
            (7, Some(recorded.clone()))
        );
        assert_eq!(loaded.memory, record.memory);
    }
}

#[test]
fn issuances_with_provenance_take_only_the_contracts_words() {
    let (root, repo, data) = fixture();
    let mut value = canonical_record();
    let entry = json!({"verdict": "pass", "provenance": provenance()});
    value["memory"]["judge_issuance"][0][1][1] = entry.clone();
    std::fs::write(data.path(3), serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(data.load(3).unwrap()).unwrap(), value);
    // Pre-contract words were only ever saved bare; with provenance, as any
    // unknown word or field, they reject the record rather than reset it.
    for bad in [
        json!({"verdict": "Land", "provenance": provenance()}),
        json!({"verdict": "unknown", "provenance": provenance()}),
        json!({"verdict": "pass", "provenance": provenance(), "extra": 1}),
        json!({"verdict": "pass"}),
    ] {
        value["memory"]["judge_issuance"][0][1][1] = bad;
        std::fs::write(data.path(3), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(data.load(3).is_err());
        assert!(Data::read(root.path(), &repo, 3).is_err());
    }
}

#[test]
fn saved_judge_decisions_load_from_before_and_after_they_carried_reasons() {
    let (root, repo, data) = fixture();
    for (saved, step) in [
        (json!("Judge"), Step::Judge(vec![])),
        (json!({"Judge": []}), Step::Judge(vec![])),
        (
            json!({"Judge": ["no_ci", "round_limit"]}),
            Step::Judge(vec![Trigger::NoCi, Trigger::RoundLimit]),
        ),
    ] {
        let mut value: Value = serde_json::from_str(BASE_RECORD).unwrap();
        value["decision"]["step"] = saved;
        std::fs::write(data.path(3), value.to_string()).unwrap();
        let mut expected = canonical_record();
        expected["decision"]["step"] = serde_json::to_value(&step).unwrap();
        for record in [
            data.load(3).unwrap(),
            Data::read(root.path(), &repo, 3).unwrap(),
        ] {
            assert_eq!(record.decision.as_ref().unwrap().step, step);
            // Everything else survives: jobs, permissions, issuances, memory.
            assert_eq!(serde_json::to_value(&record).unwrap(), expected);
        }
    }
}
