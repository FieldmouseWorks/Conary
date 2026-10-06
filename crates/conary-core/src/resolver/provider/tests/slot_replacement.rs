// crates/conary-core/src/resolver/provider/tests/slot_replacement.rs

#![cfg(test)]

use super::*;
use crate::error::Error;
use crate::repository::resolution_policy::InstalledReplacementPolicy;
use crate::repository::versioning::VersionComparisonError;

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
    provider.lock_surviving_installed_candidates(InstalledReplacementPolicy::UpgradeOnly);
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
    provider.lock_surviving_installed_candidates(InstalledReplacementPolicy::UpgradeOnly);

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
    provider.lock_surviving_installed_candidates(InstalledReplacementPolicy::UpgradeOnly);

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
    provider.lock_surviving_installed_candidates(InstalledReplacementPolicy::UpgradeOnly);
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

#[test]
fn equal_version_repository_candidate_is_not_an_upgrade_replacer() {
    let (_dir, conn) = setup_test_db();
    let mut provider = ConaryProvider::new(&conn).unwrap();

    let installed_id = provider
        .add_solvable(installed_identity(
            "libfoo",
            "3",
            VersionScheme::Rpm,
            Some(7),
        ))
        .unwrap();
    let equal_id = provider
        .add_solvable(repo_identity("libfoo", "3", VersionScheme::Rpm, Some(8)))
        .unwrap();
    let newer_id = provider
        .add_solvable(repo_identity("libfoo", "4", VersionScheme::Rpm, Some(9)))
        .unwrap();
    provider.lock_surviving_installed_candidates(InstalledReplacementPolicy::UpgradeOnly);

    // Equal version is not strictly newer, so `UpgradeOnly` refuses it. The
    // newer candidate on the same fixture is still the exact successor, proving
    // the refusal comes from the version-direction rule and not a broken fixture.
    assert_eq!(provider.slot_predecessor(equal_id), None);
    assert_eq!(provider.slot_predecessor(newer_id), Some(installed_id));
}

#[test]
fn invalid_package_release_is_refused_when_a_solvable_is_admitted() {
    let (_dir, conn) = setup_test_db();
    let mut provider = ConaryProvider::new(&conn).unwrap();

    let mut invalid = repo_identity("libfoo", "3", VersionScheme::Rpm, Some(8));
    invalid.package_release = Some("x".to_string());
    let error = provider.add_solvable(invalid).unwrap_err();
    assert!(matches!(
        error,
        Error::VersionComparison(VersionComparisonError::InvalidPackageRelease { .. })
    ));

    // Positive control: the same repository candidate with a valid release
    // passes the same admission check, proving the refusal comes from the
    // release rule rather than a broken fixture.
    let mut valid = repo_identity("libfoo", "3", VersionScheme::Rpm, Some(8));
    valid.package_release = Some("1".to_string());
    let id = provider.add_solvable(valid).unwrap();
    assert_eq!(
        provider.get_solvable(id).package_release.as_deref(),
        Some("1")
    );
}

fn with_release(mut identity: PackageIdentity, release: Option<&str>) -> PackageIdentity {
    identity.package_release = release.map(str::to_string);
    identity
}

#[test]
fn identical_identity_is_not_a_replacer_even_when_downgrades_are_allowed() {
    let (_dir, conn) = setup_test_db();
    let mut provider = ConaryProvider::new(&conn).unwrap();

    let installed_id = provider
        .add_solvable(with_release(
            installed_identity("libfoo", "3", VersionScheme::Rpm, Some(7)),
            Some("2"),
        ))
        .unwrap();
    let identical_id = provider
        .add_solvable(with_release(
            repo_identity("libfoo", "3", VersionScheme::Rpm, Some(8)),
            Some("2"),
        ))
        .unwrap();
    let older_id = provider
        .add_solvable(with_release(
            repo_identity("libfoo", "2", VersionScheme::Rpm, Some(9)),
            Some("2"),
        ))
        .unwrap();
    let respelled_id = provider
        .add_solvable(with_release(
            repo_identity("libfoo", "0:3", VersionScheme::Rpm, Some(10)),
            Some("2"),
        ))
        .unwrap();
    provider.lock_surviving_installed_candidates(InstalledReplacementPolicy::AllowDowngrade);

    // The installer classifies an identical version string and package release
    // as already installed under every policy, so the identical candidate is
    // never a replacer.
    assert_eq!(provider.slot_predecessor(identical_id), None);
    // Positive controls on the same fixture: an older candidate is the
    // downgrade the installer applies under `--allow-downgrade`, and a
    // candidate the RPM scheme orders as equal but spells differently is not
    // the installer's textual identity, so it is a replacer too.
    let installed = provider.get_solvable(installed_id);
    let respelled = provider.get_solvable(respelled_id);
    assert_eq!(
        crate::repository::versioning::compare_package_identities(
            respelled.version_scheme,
            &respelled.version,
            respelled.package_release.as_deref(),
            installed.version_scheme,
            &installed.version,
            installed.package_release.as_deref(),
        )
        .unwrap(),
        std::cmp::Ordering::Equal,
        "control: the respelled candidate must order equal to the installed trove"
    );
    assert_eq!(provider.slot_predecessor(older_id), Some(installed_id));
    assert_eq!(provider.slot_predecessor(respelled_id), Some(installed_id));
}

#[test]
fn upgrade_only_breaks_equal_version_ties_by_package_release() {
    let (_dir, conn) = setup_test_db();
    let mut provider = ConaryProvider::new(&conn).unwrap();

    let installed_id = provider
        .add_solvable(with_release(
            installed_identity("libfoo", "3", VersionScheme::Rpm, Some(7)),
            Some("2"),
        ))
        .unwrap();
    let higher_id = provider
        .add_solvable(with_release(
            repo_identity("libfoo", "3", VersionScheme::Rpm, Some(8)),
            Some("3"),
        ))
        .unwrap();
    let lower_id = provider
        .add_solvable(with_release(
            repo_identity("libfoo", "3", VersionScheme::Rpm, Some(9)),
            Some("1"),
        ))
        .unwrap();
    let missing_id = provider
        .add_solvable(repo_identity("libfoo", "3", VersionScheme::Rpm, Some(10)))
        .unwrap();
    provider.lock_surviving_installed_candidates(InstalledReplacementPolicy::UpgradeOnly);

    // `compare_package_identities` breaks an equal-version tie by release, and
    // a missing release counts as zero: only the higher release is newer.
    assert_eq!(provider.slot_predecessor(higher_id), Some(installed_id));
    assert_eq!(provider.slot_predecessor(lower_id), None);
    assert_eq!(provider.slot_predecessor(missing_id), None);
}

#[test]
fn upgrade_only_treats_a_missing_installed_release_as_zero() {
    let (_dir, conn) = setup_test_db();
    let mut provider = ConaryProvider::new(&conn).unwrap();

    let installed_id = provider
        .add_solvable(installed_identity(
            "libfoo",
            "3",
            VersionScheme::Rpm,
            Some(7),
        ))
        .unwrap();
    let released_id = provider
        .add_solvable(with_release(
            repo_identity("libfoo", "3", VersionScheme::Rpm, Some(8)),
            Some("1"),
        ))
        .unwrap();
    let unreleased_id = provider
        .add_solvable(repo_identity("libfoo", "3", VersionScheme::Rpm, Some(9)))
        .unwrap();
    provider.lock_surviving_installed_candidates(InstalledReplacementPolicy::UpgradeOnly);

    // Any valid release (at least 1) is newer than the installed trove's
    // implicit release zero; a candidate that also lacks a release is the
    // installed identity and is refused.
    assert_eq!(provider.slot_predecessor(released_id), Some(installed_id));
    assert_eq!(provider.slot_predecessor(unreleased_id), None);
}
