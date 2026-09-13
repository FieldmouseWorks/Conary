// third_party/resolvo-0.12.1-patched/src/solver/conary_tests.rs
#![cfg(test)]

// Backported from prefix-dev/resolvo#293 at dfb403fce20c989a150ef5bbf89c2d2c1e2b3199.
use crate::{
    DenseIndex, KnownDependencies, NameId,
    snapshot::{
        DependencySnapshot, Package as SnapshotPackage, SnapshotProvider,
        Solvable as SnapshotSolvable, VersionSet as SnapshotVersionSet,
    },
};
use crate::{Problem, SolvableId, Solver, VersionSetId};

/// A version set can represent a virtual capability provided by multiple
/// concrete package names. Forbid-multiple clauses must be grouped by each
/// solvable's concrete name, not by the first candidate in the version set.
#[test]
fn test_virtual_package_candidates_can_have_different_names() {
    let virtual_name = NameId::from_index(0);
    let package_a = NameId::from_index(1);
    let package_b = NameId::from_index(2);
    let solvable_a = SolvableId::from_index(0);
    let solvable_b = SolvableId::from_index(1);
    let virtual_set = VersionSetId::from_index(0);
    let package_a_set = VersionSetId::from_index(1);
    let package_b_set = VersionSetId::from_index(2);

    let mut snapshot = DependencySnapshot::default();
    snapshot.solvables.insert(
        solvable_a,
        SnapshotSolvable {
            display: "package-a=1".to_owned(),
            name: package_a,
            order: 0,
            dependencies: crate::Dependencies::Known(KnownDependencies::default()),
            hint_dependencies_available: false,
        },
    );
    snapshot.solvables.insert(
        solvable_b,
        SnapshotSolvable {
            display: "package-b=1".to_owned(),
            name: package_b,
            order: 0,
            dependencies: crate::Dependencies::Known(KnownDependencies::default()),
            hint_dependencies_available: false,
        },
    );

    for (name, display, solvables) in [
        (virtual_name, "virtual", vec![solvable_a, solvable_b]),
        (package_a, "package-a", vec![solvable_a]),
        (package_b, "package-b", vec![solvable_b]),
    ] {
        snapshot.packages.insert(
            name,
            SnapshotPackage {
                name: display.to_owned(),
                solvables,
                excluded: Vec::new(),
            },
        );
    }

    for (id, name, display, matching_candidates) in [
        (
            virtual_set,
            virtual_name,
            "*",
            [solvable_a, solvable_b].into_iter().collect(),
        ),
        (
            package_a_set,
            package_a,
            "*",
            [solvable_a].into_iter().collect(),
        ),
        (
            package_b_set,
            package_b,
            "*",
            [solvable_b].into_iter().collect(),
        ),
    ] {
        snapshot.version_sets.insert(
            id,
            SnapshotVersionSet {
                name,
                display: display.to_owned(),
                matching_candidates,
            },
        );
    }

    let provider = SnapshotProvider::new(&snapshot);
    let mut solver = Solver::new(provider);
    let problem = Problem::new().requirements(vec![
        virtual_set.into(),
        package_a_set.into(),
        package_b_set.into(),
    ]);
    let solved = solver.solve(problem).unwrap();

    assert_eq!(solved.len(), 2);
    assert!(solved.contains(&solvable_a));
    assert!(solved.contains(&solvable_b));
}
