// apps/conary/src/commands/model/apply/metadata_selection_tests.rs

//! Model metadata actions select installed packages by name only.
//!
//! The model language addresses packages by name, so apply resolves that name
//! through the shared package-only resolver: a same-name component never
//! competes with the package, a component-only name is missing, and duplicate
//! installed package variants are refused instead of silently first-matched.

use conary_core::db::models::{InstallReason, InstallSource, Trove, TroveType};
use conary_core::model::DiffAction;
use conary_core::repository::versioning::VersionScheme;

use super::apply_metadata_changes;
use crate::commands::test_helpers::create_test_db;

const FIXTURE_SELECTION_REASON: &str = "metadata selection fixture";

#[derive(Clone, Copy)]
enum MetadataAction {
    Pin,
    Unpin,
    MarkExplicit,
    MarkDependency,
}

impl MetadataAction {
    const ALL: [Self; 4] = [
        Self::Pin,
        Self::Unpin,
        Self::MarkExplicit,
        Self::MarkDependency,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Pin => "Pin",
            Self::Unpin => "Unpin",
            Self::MarkExplicit => "MarkExplicit",
            Self::MarkDependency => "MarkDependency",
        }
    }

    fn diff_action(self, package: &str) -> DiffAction {
        match self {
            Self::Pin => DiffAction::Pin {
                package: package.to_string(),
                pattern: "*".to_string(),
            },
            Self::Unpin => DiffAction::Unpin {
                package: package.to_string(),
            },
            Self::MarkExplicit => DiffAction::MarkExplicit {
                package: package.to_string(),
            },
            Self::MarkDependency => DiffAction::MarkDependency {
                package: package.to_string(),
            },
        }
    }

    /// Initial install reason that makes the action a real change.
    fn initial_install_reason(self) -> InstallReason {
        match self {
            Self::MarkExplicit => InstallReason::Dependency,
            _ => InstallReason::Explicit,
        }
    }

    /// Initial pin state that makes the action a real change.
    fn initial_pinned(self) -> bool {
        matches!(self, Self::Unpin)
    }

    fn assert_target_mutated(self, trove: &Trove) {
        match self {
            Self::Pin => assert!(trove.pinned, "pin must persist on the package"),
            Self::Unpin => assert!(!trove.pinned, "unpin must persist on the package"),
            Self::MarkExplicit => {
                assert_eq!(trove.install_reason, InstallReason::Explicit);
                assert_eq!(
                    trove.selection_reason.as_deref(),
                    Some("Marked explicit by model apply")
                );
            }
            Self::MarkDependency => {
                assert_eq!(trove.install_reason, InstallReason::Dependency);
                assert_eq!(
                    trove.selection_reason.as_deref(),
                    Some(FIXTURE_SELECTION_REASON)
                );
            }
        }
    }

    fn assert_untouched(self, trove: &Trove, context: &str) {
        assert_eq!(
            trove.install_reason,
            self.initial_install_reason(),
            "{context} install reason must not change"
        );
        assert_eq!(
            trove.pinned,
            self.initial_pinned(),
            "{context} pin state must not change"
        );
        assert_eq!(
            trove.selection_reason.as_deref(),
            Some(FIXTURE_SELECTION_REASON),
            "{context} selection reason must not change"
        );
    }
}

fn insert_trove(
    db_path: &str,
    name: &str,
    release: &str,
    trove_type: TroveType,
    action: MetadataAction,
) -> i64 {
    let conn = conary_core::db::open(db_path).unwrap();
    let mut trove = Trove::new_with_source(
        name.to_string(),
        "1.0.0".to_string(),
        trove_type,
        InstallSource::Repository,
        VersionScheme::Conary,
    );
    trove.architecture = Some("x86_64".to_string());
    trove.package_release = Some(release.to_string());
    trove.install_reason = action.initial_install_reason();
    trove.selection_reason = Some(FIXTURE_SELECTION_REASON.to_string());
    trove.pinned = action.initial_pinned();
    trove.insert(&conn).unwrap()
}

fn insert_package(db_path: &str, name: &str, release: &str, action: MetadataAction) -> i64 {
    insert_trove(db_path, name, release, TroveType::Package, action)
}

fn insert_component(db_path: &str, name: &str, action: MetadataAction) -> i64 {
    insert_trove(db_path, name, "0", TroveType::Component, action)
}

fn stored_trove(db_path: &str, id: i64) -> Trove {
    let conn = conary_core::db::open(db_path).unwrap();
    Trove::find_by_id(&conn, id)
        .unwrap()
        .expect("metadata selection tests must not delete the fixture trove")
}

fn run_action(db_path: &str, action: MetadataAction, package: &str) -> (usize, Vec<String>) {
    let diff_action = action.diff_action(package);
    apply_metadata_changes(db_path, &[&diff_action])
}

fn assert_action_labeled_diagnostic(
    errors: &[String],
    action: MetadataAction,
    package: &str,
    expected_detail: &str,
) {
    assert_eq!(
        errors.len(),
        1,
        "{} must report exactly one diagnostic: {errors:?}",
        action.label()
    );
    let diagnostic = &errors[0];
    assert!(
        diagnostic.starts_with(&format!("{} '{package}': ", action.label())),
        "diagnostic must name the action and package: {diagnostic}"
    );
    assert!(
        diagnostic.contains(expected_detail),
        "diagnostic must explain the {expected_detail:?} failure: {diagnostic}"
    );
    for flag in ["--version", "--release", "--arch"] {
        assert!(
            !diagnostic.contains(flag),
            "model metadata diagnostics must not recommend {flag}: {diagnostic}"
        );
    }
}

#[test]
fn same_name_component_does_not_block_package_selection() {
    for action in MetadataAction::ALL {
        let (_temp, db_path) = create_test_db();
        let name = "shared-name-fixture";
        let package = insert_package(&db_path, name, "1", action);
        let component = insert_component(&db_path, name, action);
        let unrelated = insert_package(&db_path, "unrelated-fixture", "1", action);

        let (applied, errors) = run_action(&db_path, action, name);

        assert_eq!(
            applied,
            1,
            "{} must apply to exactly the package: {errors:?}",
            action.label()
        );
        assert!(errors.is_empty(), "{}: {errors:?}", action.label());

        action.assert_target_mutated(&stored_trove(&db_path, package));
        let (reapplied, errors) = run_action(&db_path, action, name);
        assert!(errors.is_empty(), "{errors:?}");
        let expected = usize::from(matches!(
            action,
            MetadataAction::Pin | MetadataAction::Unpin
        ));
        assert_eq!(reapplied, expected, "install-reason actions are idempotent");
        action.assert_target_mutated(&stored_trove(&db_path, package));
        action.assert_untouched(&stored_trove(&db_path, component), "same-name component");
        action.assert_untouched(&stored_trove(&db_path, unrelated), "unrelated package");
    }
}

#[test]
fn component_only_name_is_missing_for_every_metadata_action() {
    for action in MetadataAction::ALL {
        let (_temp, db_path) = create_test_db();
        let name = "component-only-fixture";
        let component = insert_component(&db_path, name, action);

        let (applied, errors) = run_action(&db_path, action, name);

        assert_eq!(applied, 0, "{} must not mutate a component", action.label());
        assert_action_labeled_diagnostic(&errors, action, name, "not installed");
        action.assert_untouched(&stored_trove(&db_path, component), "component-only target");
    }
}

#[test]
fn ambiguous_package_variants_are_refused_for_every_metadata_action() {
    for action in MetadataAction::ALL {
        let (_temp, db_path) = create_test_db();
        let name = "ambiguous-selection-fixture";
        let component = insert_component(&db_path, name, action);
        let first = insert_package(&db_path, name, "1", action);
        let second = insert_package(&db_path, name, "2", action);

        let (applied, errors) = run_action(&db_path, action, name);

        assert_eq!(
            applied,
            0,
            "{} must not first-match an ambiguous name",
            action.label()
        );
        assert_action_labeled_diagnostic(&errors, action, name, "variants");
        for (id, context) in [
            (component, "same-name component"),
            (first, "first package variant"),
            (second, "second package variant"),
        ] {
            action.assert_untouched(&stored_trove(&db_path, id), context);
        }
    }
}
