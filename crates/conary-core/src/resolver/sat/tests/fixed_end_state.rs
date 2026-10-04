// crates/conary-core/src/resolver/sat/tests/fixed_end_state.rs

#![cfg(test)]

//! Certification of a fully determined incoming package set against the fixed
//! end state `(installed - outgoing) + incoming`, with no SAT solve.

use super::*;
use crate::repository::dependency_model::{
    CapabilityProvenance, ProvideArchitectureQualifier, ProvidedCapability,
    RepositoryCapabilityKind, RepositoryRequirementGroup, RepositoryRequirementKind,
};

fn generic_capability(name: &str) -> ProvidedCapability {
    ProvidedCapability {
        kind: RepositoryCapabilityKind::Generic,
        name: name.to_string(),
        version: None,
        version_relation: None,
        version_scheme: VersionScheme::Rpm,
        architecture_qualifier: ProvideArchitectureQualifier::Implicit,
        provenance: CapabilityProvenance::AuthorDeclared,
    }
}

fn hard_depends(name: &str) -> RepositoryRequirementGroup {
    crate::repository::requirement::parse_native_requirement(
        RepositoryRequirementKind::Depends,
        VersionScheme::Rpm,
        name,
    )
    .unwrap()
}

fn fixed_package(
    name: &str,
    provides: &[&str],
    requirements: Vec<RepositoryRequirementGroup>,
) -> FixedIncomingPackage {
    FixedIncomingPackage::new(
        name.to_string(),
        "1.0.0".to_string(),
        None,
        Some("x86_64".to_string()),
        None,
        VersionScheme::Rpm,
        provides
            .iter()
            .map(|capability| generic_capability(capability))
            .collect(),
        requirements,
    )
    .unwrap()
}

/// An incoming provide activates a surviving installed conditional whose
/// required side no incoming package provides.
#[test]
fn incoming_provide_activates_installed_conditional_and_refuses() {
    let (_dir, conn) = setup_test_db();
    let x_trove_id = insert_rpm_trove(&conn, "x", "1.0.0", &[("(foo if bar)", None)]);

    // Control through the same fixture: an incoming provider of `foo` makes the
    // activated group hold, so the certification is empty.
    let satisfied = certify_fixed_end_state(
        &conn,
        &[
            fixed_package("a", &["bar"], Vec::new()),
            fixed_package("b", &["foo"], Vec::new()),
        ],
        &[],
    )
    .unwrap();
    assert!(satisfied.is_empty(), "{satisfied:?}");

    let unsatisfied =
        certify_fixed_end_state(&conn, &[fixed_package("a", &["bar"], Vec::new())], &[]).unwrap();
    assert_eq!(unsatisfied.len(), 1, "{unsatisfied:?}");
    assert_eq!(
        unsatisfied[0].owner,
        SatGroupOwner::Installed {
            trove_id: x_trove_id,
            package_name: "x".to_string(),
        },
        "{unsatisfied:?}"
    );
}

/// A missing requirement of an incoming root is owned by the incoming package,
/// not by any installed package.
#[test]
fn incoming_root_missing_dependency_is_owned_by_incoming() {
    let (_dir, conn) = setup_test_db();

    // Control through the same fixture: the dependency present in the incoming
    // set satisfies the root, so the certification is empty.
    let satisfied = certify_fixed_end_state(
        &conn,
        &[
            fixed_package("d", &[], Vec::new()),
            fixed_package("r", &[], vec![hard_depends("d")]),
        ],
        &[],
    )
    .unwrap();
    assert!(satisfied.is_empty(), "{satisfied:?}");

    let unsatisfied = certify_fixed_end_state(
        &conn,
        &[fixed_package("r", &[], vec![hard_depends("d")])],
        &[],
    )
    .unwrap();
    assert_eq!(unsatisfied.len(), 1, "{unsatisfied:?}");
    assert_eq!(
        unsatisfied[0].owner,
        SatGroupOwner::Incoming,
        "{unsatisfied:?}"
    );
}

/// An installed group that was already broken before the transaction is observed
/// but never attributed to it.
#[test]
fn observed_preexisting_breakage_is_not_attributed() {
    let (_dir, conn) = setup_test_db();
    // `x` requires `qux`, absent from the start, and provides `baz`, which the
    // incoming root requires. That forces `x` into the affected set.
    let x_trove_id = insert_rpm_trove(&conn, "x", "1.0.0", &[("qux", None)]);
    insert_provide(&conn, x_trove_id, "baz", None);

    let satisfied = certify_fixed_end_state(
        &conn,
        &[fixed_package("a", &[], vec![hard_depends("baz")])],
        &[],
    )
    .unwrap();
    assert!(satisfied.is_empty(), "{satisfied:?}");

    // Control: the same fixture plus installed `y` with `(foo if bar)`, and `a`
    // also provides `bar`, leaves only `y`'s newly activated group unsatisfied.
    let y_trove_id = insert_rpm_trove(&conn, "y", "1.0.0", &[("(foo if bar)", None)]);
    let control = certify_fixed_end_state(
        &conn,
        &[fixed_package("a", &["bar"], vec![hard_depends("baz")])],
        &[],
    )
    .unwrap();
    assert_eq!(control.len(), 1, "{control:?}");
    assert_eq!(
        control[0].owner,
        SatGroupOwner::Installed {
            trove_id: y_trove_id,
            package_name: "y".to_string(),
        },
        "{control:?}"
    );
}

/// An outgoing installed provider leaves the exact end state, breaking a
/// surviving installed requirer unless an incoming package supplies the
/// capability.
#[test]
fn outgoing_provider_removal_breaks_installed_requirer() {
    let (_dir, conn) = setup_test_db();
    let p_trove_id = insert_rpm_trove(&conn, "p", "1.0.0", &[]);
    insert_provide(&conn, p_trove_id, "cap", None);
    let x_trove_id = insert_rpm_trove(&conn, "x", "1.0.0", &[("cap", None)]);

    // Control through the same fixture: the incoming package provides `cap`, so
    // `x` stays satisfied after `p` leaves.
    let satisfied = certify_fixed_end_state(
        &conn,
        &[fixed_package("a", &["cap"], Vec::new())],
        &[p_trove_id],
    )
    .unwrap();
    assert!(satisfied.is_empty(), "{satisfied:?}");

    let unsatisfied =
        certify_fixed_end_state(&conn, &[fixed_package("a", &[], Vec::new())], &[p_trove_id])
            .unwrap();
    assert_eq!(unsatisfied.len(), 1, "{unsatisfied:?}");
    assert_eq!(
        unsatisfied[0].owner,
        SatGroupOwner::Installed {
            trove_id: x_trove_id,
            package_name: "x".to_string(),
        },
        "{unsatisfied:?}"
    );
}
