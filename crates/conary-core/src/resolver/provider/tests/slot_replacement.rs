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
    provider.compile_slot_replacement_constrains().unwrap();

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
