// crates/conary-core/src/source_root/tests/registry_lifecycle.rs

use super::super::*;
use super::{fresh_registry, mode_of, name, set_mode};
use crate::runtime_root::ConaryRuntimeRoot;
use std::path::PathBuf;

#[test]
fn create_then_open_round_trips_with_explicit_runtime_root() {
    let (_temp, registry) = fresh_registry();
    let arch = name("arch");

    let created = registry.create(&arch).unwrap();
    let opened = registry.open(&arch).unwrap();
    assert_eq!(created, opened);

    let root_dir = registry.base().join("arch");
    let expected = ConaryRuntimeRoot::new(root_dir.clone(), root_dir.join("conary.db"));
    assert_eq!(opened.name(), &arch);
    assert_eq!(opened.pinned_identity(), &arch);
    assert!(!opened.pinned_at().is_empty());
    assert_eq!(opened.runtime_root(), &expected);
    assert_eq!(opened.root_dir(), root_dir.as_path());
    // The implicit parent-of-database rule still agrees with explicit
    // addressing; removing that rule belongs to the CLI cut-over.
    assert_eq!(ConaryRuntimeRoot::from_db_path(opened.db_path()), expected);

    assert_eq!(mode_of(&root_dir), SOURCE_ROOT_DIR_MODE);
    assert_eq!(mode_of(registry.base()), SOURCE_ROOTS_BASE_MODE);
    assert_eq!(
        crate::db::schema::inspect(opened.db_path()).unwrap(),
        crate::db::schema::SchemaCompatibility::Current
    );
}

#[test]
fn create_refuses_an_existing_root_and_leaves_no_staging() {
    let (_temp, registry) = fresh_registry();
    let arch = name("arch");
    let created = registry.create(&arch).unwrap();

    let error = registry.create(&arch).unwrap_err();
    assert!(
        matches!(&error, SourceRootError::AlreadyExists { name } if name == &arch),
        "{error:?}"
    );
    // Any pre-existing entry occupies the name, even one that is not a root.
    std::fs::write(registry.base().join("fedora-44"), b"not a root").unwrap();
    assert!(matches!(
        registry.create(&name("fedora-44")).unwrap_err(),
        SourceRootError::AlreadyExists { .. }
    ));

    let names: Vec<String> = std::fs::read_dir(registry.base())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "unexpected registry entries: {names:?}");
    assert_eq!(registry.open(&arch).unwrap(), created);
}

#[test]
fn two_roots_have_disjoint_runtime_paths() {
    let (_temp, registry) = fresh_registry();
    let arch = registry.create(&name("arch")).unwrap();
    let fedora = registry.create(&name("fedora-44")).unwrap();
    let host = ConaryRuntimeRoot::default();

    let paths = |root: &ConaryRuntimeRoot| -> Vec<PathBuf> {
        vec![
            root.db_path().to_path_buf(),
            root.objects_dir(),
            root.generations_dir(),
            root.generation_path(1),
            root.current_link(),
            root.mount_dir(),
            root.etc_state_dir(),
            root.gc_roots_dir(),
            root.keys_dir(),
        ]
    };
    let arch_paths = paths(arch.runtime_root());
    let fedora_paths = paths(fedora.runtime_root());
    for path in &arch_paths {
        assert!(path.starts_with(arch.root_dir()), "{}", path.display());
        assert!(!path.starts_with(fedora.root_dir()), "{}", path.display());
    }
    for path in &fedora_paths {
        assert!(path.starts_with(fedora.root_dir()), "{}", path.display());
        assert!(!path.starts_with(arch.root_dir()), "{}", path.display());
    }
    for path in arch_paths.iter().chain(&fedora_paths) {
        assert!(!paths(&host).contains(path), "{}", path.display());
    }
    assert_eq!(arch.runtime_root().keys_dir(), arch.root_dir().join("keys"));
}

#[test]
fn missing_base_lists_nothing_and_opens_nothing() {
    let (_temp, registry) = fresh_registry();
    assert!(registry.list().unwrap().is_empty());
    assert!(matches!(
        registry.open(&name("arch")).unwrap_err(),
        SourceRootError::NotFound { .. }
    ));
    assert!(!registry.base().exists(), "reads must not create the base");

    // Positive control: after creation the same registry lists the root.
    let arch = registry.create(&name("arch")).unwrap();
    assert_eq!(registry.list().unwrap(), vec![SourceRootEntry::Root(arch)]);
    assert!(matches!(
        registry.open(&name("fedora-44")).unwrap_err(),
        SourceRootError::NotFound { .. }
    ));
}

#[test]
fn unsafe_registry_base_is_refused() {
    let (_temp, registry) = fresh_registry();
    let arch = registry.create(&name("arch")).unwrap();

    set_mode(registry.base(), 0o777);
    for error in [
        registry.list().unwrap_err(),
        registry.open(&name("arch")).unwrap_err(),
        registry.create(&name("fedora-44")).unwrap_err(),
    ] {
        assert!(
            matches!(
                &error,
                SourceRootError::UnsafeRegistryBase {
                    defect: RegistryBaseDefect::WritableByOthers { mode: 0o777 },
                    ..
                }
            ),
            "{error:?}"
        );
    }

    set_mode(registry.base(), 0o755);
    let foreign_owner = nix::unistd::geteuid().as_raw().wrapping_add(1);
    let error = registry
        .clone()
        .with_expected_owner(foreign_owner)
        .list()
        .unwrap_err();
    assert!(
        matches!(
            &error,
            SourceRootError::UnsafeRegistryBase {
                defect: RegistryBaseDefect::WrongOwner { expected, .. },
                ..
            } if *expected == foreign_owner
        ),
        "{error:?}"
    );

    // Positive control: the restored base lists the root again.
    assert_eq!(registry.list().unwrap(), vec![SourceRootEntry::Root(arch)]);

    let file_base = registry.base().join("arch").join("conary.db");
    let error = SourceRootRegistry::new(file_base).list().unwrap_err();
    assert!(matches!(
        error,
        SourceRootError::UnsafeRegistryBase {
            defect: RegistryBaseDefect::NotADirectory,
            ..
        }
    ));
}
