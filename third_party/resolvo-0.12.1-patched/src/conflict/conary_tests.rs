// third_party/resolvo-0.12.1-patched/src/conflict/conary_tests.rs
#![cfg(test)]

use super::*;
use crate::{Dependencies, KnownDependencies, NameId, snapshot};

#[test]
fn conflict_graph_discards_learned_branches_unreachable_from_root() {
    // Build the same diagnostic evidence as the original graph-only regression:
    // root -> causal -> missing, with unrelated -> missing retained from a
    // previously explored branch. Exercise Conflict::graph itself so this test
    // can run unchanged against an unpatched upstream crate.
    let mut snapshot = snapshot::DependencySnapshot::default();
    let causal = SolvableId::from_raw(0);
    let unrelated = SolvableId::from_raw(1);
    for (index, label, solvables) in [
        (0, "causal", vec![causal]),
        (1, "unrelated", vec![unrelated]),
        (2, "missing", vec![]),
    ] {
        let name = NameId::from_raw(index);
        snapshot.packages.insert(
            name,
            snapshot::Package {
                name: label.into(),
                solvables: solvables.clone(),
                excluded: vec![],
            },
        );
        snapshot.version_sets.insert(
            VersionSetId(index),
            snapshot::VersionSet {
                name,
                display: "*".into(),
                matching_candidates: solvables.iter().copied().collect(),
            },
        );
        for solvable in solvables {
            snapshot.solvables.insert(
                solvable,
                snapshot::Solvable {
                    display: format!("{label} 1"),
                    name,
                    order: 0,
                    dependencies: Dependencies::Known(KnownDependencies::default()),
                    hint_dependencies_available: false,
                },
            );
        }
    }
    let mut solver = Solver::new(snapshot.provider());
    let causal_var = solver.state.variable_map.intern_solvable(causal);
    let unrelated_var = solver.state.variable_map.intern_solvable(unrelated);
    let mut conflict = Conflict::default();
    for (parent, requirement) in [
        (VariableId::root(), VersionSetId(0)),
        (causal_var, VersionSetId(2)),
        (unrelated_var, VersionSetId(2)),
    ] {
        let id = ClauseId::from_index(solver.state.clauses.kinds.len());
        solver
            .state
            .clauses
            .kinds
            .push(Clause::Requires(parent, None, requirement.into()));
        conflict.add_clause(id);
    }

    let filtered = conflict.graph(&solver);
    assert_eq!(filtered.graph.node_count(), 3);
    assert_eq!(filtered.graph.edge_count(), 2);
    assert!(matches!(
        filtered.graph[filtered.root_node],
        ConflictNode::Root
    ));
    assert!(matches!(
        filtered.graph[filtered.unresolved_node.unwrap()],
        ConflictNode::UnresolvedDependency
    ));
    assert!(
        !filtered
            .graph
            .node_weights()
            .any(|node| matches!(node, ConflictNode::Solvable(id) if *id == unrelated))
    );
    let diagnostic = conflict.display_user_friendly(&solver).to_string();
    assert!(diagnostic.contains("causal"));
    assert!(diagnostic.contains("missing"));
    assert!(!diagnostic.contains("unrelated"));
}
