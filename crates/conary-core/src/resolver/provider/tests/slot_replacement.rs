// crates/conary-core/src/resolver/provider/tests/slot_replacement.rs

#![cfg(test)]

use super::*;

#[test]
fn slot_replacer_excludes_only_its_install_slot_predecessor() {
    let (_dir, conn) = setup_test_db();
    let mut provider = ConaryProvider::new(&conn).unwrap();
    let name = provider.intern_name("libfoo").unwrap();

    let native_id = provider
        .add_solvable(installed_identity(
            "libfoo",
            "1",
            VersionScheme::Rpm,
            Some(7),
        ))
        .unwrap();
    let mut foreign = installed_identity("libfoo", "1", VersionScheme::Rpm, Some(9));
    foreign.architecture = Some("i686".to_string());
    let foreign_id = provider.add_solvable(foreign).unwrap();
    let replacer_id = provider
        .add_solvable(repo_identity("libfoo", "3", VersionScheme::Rpm, Some(8)))
        .unwrap();
    provider.lock_surviving_installed_candidates();
    provider.intern_all_dependency_version_sets().unwrap();
    provider.compile_replacement_constrains().unwrap();

    // The repository package shares only the native variant's install slot, so
    // it is that variant's exact replacer and the name must admit both
    // surviving variants beside it.
    assert_eq!(provider.slot_predecessor(replacer_id), Some(native_id));
    let candidates = block_on(provider.get_candidates(name)).unwrap();
    assert!(candidates.allow_multiple);
    assert!(candidates.locked.is_none());
    for id in [native_id, foreign_id, replacer_id] {
        assert!(candidates.candidates.contains(&id), "{id:?}");
    }

    // Selecting the replacer forbids exactly its predecessor: the parallel
    // variant in another slot stays selectable.
    let resolvo::Dependencies::Known(dependencies) =
        block_on(provider.get_dependencies(replacer_id))
    else {
        panic!("the replacer has compiled dependencies");
    };
    assert_eq!(dependencies.constrains.len(), 1);
    let forbidden =
        provider.matching_candidates(&candidates.candidates, dependencies.constrains[0], true);
    assert_eq!(forbidden, vec![native_id]);

    // Control: an installed package imposes no slot exclusion.
    let resolvo::Dependencies::Known(installed) = block_on(provider.get_dependencies(native_id))
    else {
        panic!("the installed variant has compiled dependencies");
    };
    assert!(installed.constrains.is_empty());
}

#[test]
fn ambiguous_install_slot_has_no_replacer() {
    let (_dir, conn) = setup_test_db();
    let mut provider = ConaryProvider::new(&conn).unwrap();
    let name = provider.intern_name("kernel").unwrap();
    provider
        .add_solvable(installed_identity(
            "kernel",
            "1",
            VersionScheme::Rpm,
            Some(7),
        ))
        .unwrap();
    provider
        .add_solvable(installed_identity(
            "kernel",
            "2",
            VersionScheme::Rpm,
            Some(9),
        ))
        .unwrap();
    let repository_id = provider
        .add_solvable(repo_identity("kernel", "3", VersionScheme::Rpm, Some(8)))
        .unwrap();
    provider.lock_surviving_installed_candidates();

    // Two installed packages share the repository package's slot, so the
    // installer could not name the trove it replaces and the solver must not
    // offer it.
    assert_eq!(provider.slot_predecessor(repository_id), None);
    let candidates = block_on(provider.get_candidates(name)).unwrap();
    assert!(!candidates.candidates.contains(&repository_id));
}

#[test]
fn cross_scheme_install_slot_has_no_replacer() {
    let (_dir, conn) = setup_test_db();
    let mut provider = ConaryProvider::new(&conn).unwrap();
    let name = provider.intern_name("libfoo").unwrap();
    provider
        .add_solvable(installed_identity(
            "libfoo",
            "1",
            VersionScheme::Rpm,
            Some(7),
        ))
        .unwrap();
    let same_scheme_id = provider
        .add_solvable(repo_identity("libfoo", "3", VersionScheme::Rpm, Some(8)))
        .unwrap();
    let cross_scheme_id = provider
        .add_solvable(repo_identity("libfoo", "3", VersionScheme::Debian, Some(9)))
        .unwrap();
    provider.lock_surviving_installed_candidates();

    // Both repository packages occupy the installed trove's machine slot, but a
    // dependency install cannot replace across version schemes without an
    // explicit replatform, so only the same-scheme package is a replacer.
    let installed = &provider.solvables[0];
    let cross_scheme = &provider.solvables[cross_scheme_id.to_index()];
    assert!(
        crate::repository::selector::package_install_slots_match(
            installed.version_scheme,
            installed.architecture.as_deref(),
            cross_scheme.version_scheme,
            cross_scheme.architecture.as_deref(),
            &provider.native_architecture,
        ),
        "control: the cross-scheme package must share the installed machine slot"
    );
    assert!(provider.slot_predecessor(same_scheme_id).is_some());
    assert_eq!(provider.slot_predecessor(cross_scheme_id), None);
    let candidates = block_on(provider.get_candidates(name)).unwrap();
    assert!(candidates.candidates.contains(&same_scheme_id));
    assert!(!candidates.candidates.contains(&cross_scheme_id));
}

#[test]
fn relation_remover_excludes_exactly_the_installed_trove_it_obsoletes() {
    let (_dir, conn) = setup_test_db();
    let mut provider = ConaryProvider::new(&conn).unwrap();
    let oldlib = provider.intern_name("oldlib").unwrap();

    let obsoleted_id = provider
        .add_solvable(installed_identity(
            "oldlib",
            "1",
            VersionScheme::Rpm,
            Some(7),
        ))
        .unwrap();
    let survivor_id = provider
        .add_solvable(installed_identity(
            "keptlib",
            "1",
            VersionScheme::Rpm,
            Some(9),
        ))
        .unwrap();
    let remover_id = provider
        .add_solvable(repo_identity("obsoleter", "9", VersionScheme::Rpm, Some(8)))
        .unwrap();
    for id in [obsoleted_id, survivor_id] {
        provider.relations.insert(id.into_raw(), Vec::new());
    }
    provider.relations.insert(
        remover_id.into_raw(),
        vec![SolverRelation {
            scheme: VersionScheme::Rpm,
            relation: crate::repository::package_relation::parse_native_relation(
                crate::repository::dependency_model::RepositoryRequirementKind::Obsolete,
                VersionScheme::Rpm,
                "oldlib < 2",
            )
            .unwrap(),
        }],
    );
    provider.lock_surviving_installed_candidates();
    provider.intern_all_dependency_version_sets().unwrap();
    provider.compile_replacement_constrains().unwrap();

    // Selecting the obsoleter forbids the trove it removes, so no pass can both
    // select it and satisfy a requirement from what it obsoletes.
    assert_eq!(
        provider.relation_removed_installed(remover_id).unwrap(),
        vec![obsoleted_id]
    );
    let resolvo::Dependencies::Known(dependencies) =
        block_on(provider.get_dependencies(remover_id))
    else {
        panic!("the remover has compiled dependencies");
    };
    assert_eq!(dependencies.constrains.len(), 1);
    let pool = block_on(provider.get_candidates(oldlib))
        .unwrap()
        .candidates;
    assert_eq!(
        provider.matching_candidates(&pool, dependencies.constrains[0], true),
        vec![obsoleted_id]
    );

    // Control: a trove the relation does not match imposes nothing.
    assert!(
        provider
            .relation_removed_installed(survivor_id)
            .unwrap()
            .is_empty()
    );
}
