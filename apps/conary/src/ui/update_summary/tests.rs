// apps/conary/src/ui/update_summary/tests.rs

use super::*;

fn entry(target: &str, status: CollectionUpdateStatus) -> CollectionUpdateEntry {
    CollectionUpdateEntry {
        target: target.into(),
        status,
    }
}

#[test]
fn preview_counts_packages_from_outcomes_and_keeps_noops_separate() {
    console::set_colors_enabled(false);
    let entries = [
        entry(
            "demo 1 [x86_64]",
            CollectionUpdateStatus::Completed(UpdateOutcome::Planned { packages: 2 }),
        ),
        entry(
            "other 1 [x86_64]",
            CollectionUpdateStatus::Completed(UpdateOutcome::NoChanges),
        ),
    ];
    assert_eq!(
        summary_lines("base", true, &entries).join("\n"),
        concat!(
            "Collection update preview\n",
            "  Collection: base\n",
            "  Planned packages: 2\n",
            "  Unchanged requests: 1\n",
            "  Failed requests: 0\n",
            "[pending]  demo 1 [x86_64]  2 planned\n",
            "[skip]     other 1 [x86_64]  no changes\n",
            "Dry run: no updates were applied.",
        )
    );
}

#[test]
fn partial_failure_never_claims_every_effect_was_rolled_back() {
    console::set_colors_enabled(false);
    let entries = [
        entry(
            "first 1",
            CollectionUpdateStatus::Completed(UpdateOutcome::Applied { packages: 2 }),
        ),
        entry(
            "second 1",
            CollectionUpdateStatus::Completed(UpdateOutcome::NoChanges),
        ),
        entry("third 1", CollectionUpdateStatus::Failed),
    ];
    let text = summary_lines("base", false, &entries).join("\n");
    assert!(text.contains("  Applied packages: 2\n"), "{text}");
    assert!(text.contains("  Unchanged requests: 1\n"), "{text}");
    assert!(text.contains("  Failed requests: 1\n"), "{text}");
    assert!(
        text.contains("[fail]     third 1  request failed"),
        "{text}"
    );
    assert!(!text.contains("complete"), "{text}");
    assert!(!text.contains("no updates were applied"), "{text}");
    assert!(
        text.contains("Failed requests may have left applied changes."),
        "{text}"
    );
}

#[test]
fn failed_preview_still_reports_that_nothing_was_applied() {
    let entries = [entry("demo 1", CollectionUpdateStatus::Failed)];
    let text = summary_lines("base", true, &entries).join("\n");
    assert!(text.contains("  Failed requests: 1\n"), "{text}");
    assert!(
        text.ends_with("Dry run: no updates were applied."),
        "{text}"
    );
}
