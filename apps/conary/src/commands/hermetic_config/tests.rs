// apps/conary/src/commands/hermetic_config/tests.rs

use super::*;
use conary_core::recipe::parse_recipe;
use std::ffi::OsString;

const HASH_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

fn secure_tempdir() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    set_mode(temp.path(), 0o700);
    temp
}

fn secure_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    set_mode(path, 0o700);
}

fn secure_config_file(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    set_mode(path, 0o600);
}

fn write_config(path: &Path, sysroot: &Path, body: &str) {
    secure_dir(path.parent().unwrap());
    secure_dir(sysroot);
    secure_config_file(path, &body.replace("{SYSROOT}", &sysroot.to_string_lossy()));
}

fn write_config_without_sysroot(path: &Path, sysroot: &Path, body: &str) {
    secure_dir(path.parent().unwrap());
    secure_config_file(path, &body.replace("{SYSROOT}", &sysroot.to_string_lossy()));
}

fn valid_config(sysroot: &Path) -> String {
    format!(
        r#"
default_builder = "native"

[builders.native]
kind = "pristine"
sysroot_path = "{}"
sysroot_hash = "{HASH_A}"
toolchain_hash = "{HASH_B}"
description = "test builder"
"#,
        sysroot.display()
    )
}

#[test]
fn config_path_resolution_prefers_explicit_env_over_xdg_and_home() {
    let path = resolve_default_config_path_with(|key| match key {
        "CONARY_HERMETIC_CONFIG" => Some(OsString::from("/explicit/hermetic.toml")),
        "XDG_CONFIG_HOME" => Some(OsString::from("/xdg")),
        "HOME" => Some(OsString::from("/home/test")),
        _ => None,
    })
    .unwrap();

    assert_eq!(path, PathBuf::from("/explicit/hermetic.toml"));
}

#[test]
fn config_path_resolution_uses_xdg_before_home() {
    let path = resolve_default_config_path_with(|key| match key {
        "XDG_CONFIG_HOME" => Some(OsString::from("/xdg")),
        "HOME" => Some(OsString::from("/home/test")),
        _ => None,
    })
    .unwrap();

    assert_eq!(path, PathBuf::from("/xdg/conary/hermetic.toml"));
}

#[test]
fn explicit_path_loads_valid_pristine_builder() {
    let temp = secure_tempdir();
    let config = temp.path().join("hermetic.toml");
    let sysroot = temp.path().join("sysroot");
    write_config(&config, &sysroot, &valid_config(&sysroot));

    let builder = load_default_hermetic_builder_from_path(&config).unwrap();

    assert_eq!(builder.sysroot_path, sysroot.canonicalize().unwrap());
    assert_eq!(builder.identity.kind, BuilderEnvironmentKind::Pristine);
    assert_eq!(builder.identity.sysroot_hash.as_deref(), Some(HASH_A));
    assert_eq!(builder.identity.toolchain_hash.as_deref(), Some(HASH_B));
}

#[test]
fn missing_default_builder_fails_closed() {
    let temp = secure_tempdir();
    let config = temp.path().join("hermetic.toml");
    let sysroot = temp.path().join("sysroot");
    write_config(
        &config,
        &sysroot,
        r#"
[builders.native]
kind = "pristine"
sysroot_path = "{SYSROOT}"
sysroot_hash = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#,
    );

    let error = load_default_hermetic_builder_from_path(&config).unwrap_err();

    assert!(
        format!("{error:#}").contains("default_builder"),
        "{error:#}"
    );
}

#[test]
fn unknown_default_builder_fails_closed() {
    let temp = secure_tempdir();
    let config = temp.path().join("hermetic.toml");
    let sysroot = temp.path().join("sysroot");
    write_config(
        &config,
        &sysroot,
        r#"
default_builder = "missing"

[builders.native]
kind = "pristine"
sysroot_path = "{SYSROOT}"
sysroot_hash = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#,
    );

    let error = load_default_hermetic_builder_from_path(&config).unwrap_err();

    assert!(format!("{error:#}").contains("unknown default_builder"));
}

#[test]
fn invalid_hash_fails_closed() {
    let temp = secure_tempdir();
    let config = temp.path().join("hermetic.toml");
    let sysroot = temp.path().join("sysroot");
    write_config(
        &config,
        &sysroot,
        r#"
default_builder = "native"

[builders.native]
kind = "pristine"
sysroot_path = "{SYSROOT}"
sysroot_hash = "sha256:nothex"
"#,
    );

    let error = load_default_hermetic_builder_from_path(&config).unwrap_err();

    assert!(format!("{error:#}").contains("sha256:<64 hex>"));
}

#[test]
fn unsupported_kind_fails_closed() {
    let temp = secure_tempdir();
    let config = temp.path().join("hermetic.toml");
    let sysroot = temp.path().join("sysroot");
    write_config(
        &config,
        &sysroot,
        r#"
default_builder = "native"

[builders.native]
kind = "host-mounted"
sysroot_path = "{SYSROOT}"
sysroot_hash = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#,
    );

    let error = load_default_hermetic_builder_from_path(&config).unwrap_err();

    assert!(format!("{error:#}").contains("unsupported kind"));
}

#[test]
fn missing_sysroot_fails_closed_without_creating_it() {
    let temp = secure_tempdir();
    let config = temp.path().join("hermetic.toml");
    let sysroot = temp.path().join("missing-sysroot");
    write_config_without_sysroot(&config, &sysroot, &valid_config(&sysroot));

    assert!(
        !sysroot.exists(),
        "fixture must not create the missing sysroot"
    );
    let error = load_default_hermetic_builder_from_path(&config).unwrap_err();
    let error = format!("{error:#}");

    assert!(error.contains("sysroot_path"), "{error}");
    assert!(error.contains(&sysroot.display().to_string()), "{error}");
    assert!(
        !sysroot.exists(),
        "load must not create the missing sysroot"
    );
}

#[test]
fn build_dependencies_are_refused_until_content_locks_exist() {
    let recipe = parse_recipe(
        r#"
[package]
name = "deps"
version = "1.0"

[source]
path = "."

[build]
requires = ["make"]
makedepends = ["gcc"]
install = "true"
"#,
    )
    .unwrap();

    let error = ensure_no_build_dependencies_for_m2a(&recipe).unwrap_err();
    let error = format!("{error:#}");

    assert!(error.contains("build dependencies"), "{error}");
    assert!(error.contains("content locks"), "{error}");
    assert!(error.contains("make"), "{error}");
    assert!(error.contains("gcc"), "{error}");
}

#[cfg(unix)]
#[test]
fn group_or_world_writable_config_file_fails_closed() {
    for mode in [0o664, 0o646] {
        let temp = secure_tempdir();
        let config = temp.path().join("hermetic.toml");
        let sysroot = temp.path().join("sysroot");
        write_config(&config, &sysroot, &valid_config(&sysroot));

        set_mode(&config, mode);

        let error = load_default_hermetic_builder_from_path(&config).unwrap_err();
        let config_path = config.canonicalize().unwrap().display().to_string();
        assert_eq!(
            error.root_cause().to_string(),
            format!("{config_path} must not be group- or world-writable"),
            "mode {mode:o}: {error:#}"
        );
    }
}

#[cfg(unix)]
#[test]
fn group_or_world_writable_sysroot_fails_closed() {
    for mode in [0o775, 0o757] {
        let temp = secure_tempdir();
        let config = temp.path().join("hermetic.toml");
        let sysroot = temp.path().join("sysroot");
        write_config(&config, &sysroot, &valid_config(&sysroot));

        set_mode(&sysroot, mode);

        let error = load_default_hermetic_builder_from_path(&config).unwrap_err();
        let sysroot_path = sysroot.canonicalize().unwrap().display().to_string();
        assert_eq!(
            error.root_cause().to_string(),
            format!("{sysroot_path} must not be group- or world-writable"),
            "mode {mode:o}: {error:#}"
        );
    }
}

#[cfg(unix)]
#[test]
fn group_or_world_writable_config_ancestor_fails_closed() {
    for mode in [0o775, 0o757] {
        let temp = secure_tempdir();
        let loose = temp.path().join("loose");
        let config = loose.join("hermetic.toml");
        let sysroot = temp.path().join("sysroot");
        write_config(&config, &sysroot, &valid_config(&sysroot));

        set_mode(&loose, mode);

        let error = load_default_hermetic_builder_from_path(&config).unwrap_err();
        let loose_path = loose.canonicalize().unwrap().display().to_string();
        assert_eq!(
            error.root_cause().to_string(),
            format!("{loose_path} must not be group- or world-writable"),
            "mode {mode:o}: {error:#}"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlinked_config_is_canonicalized_before_policy_checks() {
    use std::os::unix::fs::symlink;

    let temp = secure_tempdir();
    let real_dir = temp.path().join("real");
    let real_config = real_dir.join("hermetic.toml");
    let link_config = temp.path().join("link.toml");
    let sysroot = temp.path().join("sysroot");
    write_config(&real_config, &sysroot, &valid_config(&sysroot));
    symlink(&real_config, &link_config).unwrap();

    let builder = load_default_hermetic_builder_from_path(&link_config).unwrap();

    assert_eq!(builder.sysroot_path, sysroot.canonicalize().unwrap());
}
