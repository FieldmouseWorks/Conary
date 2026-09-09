// crates/conary-core/src/recipe/kitchen/cook/tests.rs

use super::*;
use crate::ccs::builder::CcsBuilder;
use crate::ccs::manifest::CcsManifest;
use crate::payload::PayloadIdentity;
use crate::recipe::format::{
    BuildSection, LocalSourceSection, PackageSection, PatchInfo, PatchSection, Recipe,
    RemoteSourceSection, SourceSection,
};
use crate::recipe::hermetic::source_identity::{CiMode, canonical_local_file_list};
use crate::recipe::hermetic::{
    BuildCommandRiskReport, BuildInputIdentity, BuilderEnvironmentIdentity, BuilderEnvironmentKind,
    DependencyLock, HERMETIC_EVIDENCE_SCHEMA, HermeticBuildEvidence, RecipeIdentity,
    ReproducibilityConfig, ReproducibilityRecord, SourceIdentity,
};
use crate::recipe::kitchen::KitchenConfig;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn minimal_recipe() -> Recipe {
    Recipe {
        package: PackageSection {
            name: "test-pkg".to_string(),
            version: "1.0.0".to_string(),
            release: "1".to_string(),
            summary: None,
            description: None,
            license: None,
            homepage: None,
        },
        source: SourceSection::Remote(RemoteSourceSection {
            archive: "https://example.invalid/test.tar.gz".to_string(),
            checksum: "sha256:test".to_string(),
            signature: None,
            additional: Vec::new(),
            extract_dir: None,
        }),
        build: BuildSection {
            requires: Vec::new(),
            makedepends: Vec::new(),
            configure: None,
            make: None,
            install: None,
            check: None,
            setup: None,
            post_install: None,
            environment: HashMap::new(),
            workdir: None,
            script_file: None,
            jobs: None,
            stage: None,
        },
        cross: None,
        patches: None,
        components: None,
        variables: HashMap::new(),
    }
}

#[test]
fn test_run_build_step_direct_clears_host_environment() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("CONARY_KITCHEN_LEAK", "host-secret");
    }

    let kitchen = Kitchen::new(KitchenConfig {
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let recipe = minimal_recipe();
    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    let workdir = cook.build_dir.clone();

    let result = cook.run_build_step_direct(
        "configure",
        "test -z \"$CONARY_KITCHEN_LEAK\"",
        &workdir,
        &[],
    );

    unsafe {
        std::env::remove_var("CONARY_KITCHEN_LEAK");
    }

    assert!(
        result.is_ok(),
        "direct kitchen build steps should not inherit host environment variables: {result:?}"
    );
}

#[test]
fn test_apply_direct_build_env_filters_dangerous_loader_variables() {
    let mut cmd = Command::new("env");
    apply_direct_build_env(
        &mut cmd,
        &[
            ("LD_PRELOAD".to_string(), "/tmp/malicious.so".to_string()),
            ("SAFE_FLAG".to_string(), "1".to_string()),
        ],
    );

    let envs: HashMap<String, Option<String>> = cmd
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();

    assert!(!envs.contains_key("LD_PRELOAD"));
    assert_eq!(envs.get("SAFE_FLAG"), Some(&Some("1".to_string())));
}

#[test]
fn test_chroot_env_args_filter_dangerous_loader_variables() {
    let args = chroot_env_args(
        &[
            ("LD_LIBRARY_PATH".to_string(), "/tmp/evil".to_string()),
            ("CUSTOM".to_string(), "value".to_string()),
        ],
        8,
    );

    assert!(!args.iter().any(|arg| arg.starts_with("LD_LIBRARY_PATH=")));
    assert!(args.iter().any(|arg| arg == "CUSTOM=value"));
    assert!(args.iter().any(|arg| arg == "MAKEFLAGS=-j8"));
}

#[test]
fn test_chroot_path_translation_maps_sysroot_paths_inside_chroot() {
    let sysroot = Path::new("/tmp/conary-seed/sysroot");
    assert_eq!(
        translate_path_for_chroot(Path::new("/tmp/conary-seed/sysroot/var/tmp/build"), sysroot),
        PathBuf::from("/var/tmp/build")
    );
    assert_eq!(
        translate_path_for_chroot(Path::new("/outside/build"), sysroot),
        PathBuf::from("/outside/build")
    );
}

#[test]
fn test_chroot_reproducibility_config_uses_compiler_visible_roots() {
    let dir = tempfile::tempdir().unwrap();
    let sysroot = dir.path().join("sysroot");
    let dest = sysroot.join("dest");
    let kitchen = Kitchen::new(KitchenConfig {
        sysroot: Some(sysroot.clone()),
        reproducibility: Some(ReproducibilityConfig::default()),
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let recipe = minimal_recipe();
    let cook = Cook::new_with_dest(&kitchen, &recipe, &dest).unwrap();

    let config = cook.reproducibility_config_for_execution().unwrap();
    let env = config.env_vars();
    let rustflags = env
        .iter()
        .find(|(key, _)| key == "RUSTFLAGS")
        .unwrap()
        .1
        .as_str();
    let cflags = env
        .iter()
        .find(|(key, _)| key == "CFLAGS")
        .unwrap()
        .1
        .as_str();
    let sysroot_text = sysroot.to_string_lossy();

    assert!(!rustflags.contains(sysroot_text.as_ref()));
    assert!(!cflags.contains(sysroot_text.as_ref()));
    assert!(rustflags.contains("--remap-path-prefix=/var/tmp/conary-derivation-build/"));
    assert!(cflags.contains("-ffile-prefix-map=/var/tmp/conary-derivation-build/"));
}

#[test]
fn test_prep_host_local_path_source_uses_workspace_as_source_root() {
    let dir = tempfile::tempdir().unwrap();
    let recipe_dir = dir.path().join("recipe");
    let workspace = recipe_dir.join("src");
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("marker.txt"), "local workspace").unwrap();

    let kitchen = Kitchen::new(KitchenConfig {
        source_cache: cache.clone(),
        recipe_source_base_dir: Some(recipe_dir),
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.source = SourceSection::Local(LocalSourceSection {
        path: PathBuf::from("./src"),
    });

    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    cook.prep().unwrap();
    cook.unpack().unwrap();

    assert_eq!(cook.source_dir, workspace.canonicalize().unwrap());
    assert!(!cook.source_dir.starts_with(&cook.build_dir));
    assert_eq!(
        std::fs::read_to_string(cook.source_dir.join("marker.txt")).unwrap(),
        "local workspace"
    );
    assert!(
        !cache.exists() || std::fs::read_dir(&cache).unwrap().next().is_none(),
        "local path source prep should not fetch or cache an archive"
    );
}

#[test]
fn test_prep_local_path_source_requires_recipe_source_base_dir() {
    let mut recipe = minimal_recipe();
    recipe.source = SourceSection::Local(LocalSourceSection {
        path: PathBuf::from("./src"),
    });

    let kitchen = Kitchen::new(KitchenConfig {
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let mut cook = Cook::new(&kitchen, &recipe).unwrap();

    let error = cook.prep().unwrap_err();

    assert!(
        error.to_string().contains("recipe source base dir"),
        "expected missing base dir error, got: {error}"
    );
}

#[test]
fn test_prep_isolated_local_path_source_copies_workspace_into_build_root() {
    let dir = tempfile::tempdir().unwrap();
    let recipe_dir = dir.path().join("recipe");
    let workspace = recipe_dir.join("src");
    std::fs::create_dir_all(workspace.join("nested")).unwrap();
    std::fs::write(workspace.join("nested/marker.txt"), "isolated copy").unwrap();

    let kitchen = Kitchen::new(KitchenConfig {
        recipe_source_base_dir: Some(recipe_dir),
        use_isolation: true,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.source = SourceSection::Local(LocalSourceSection {
        path: PathBuf::from("./src"),
    });

    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    cook.prep().unwrap();
    cook.unpack().unwrap();

    assert!(cook.source_dir.starts_with(&cook.build_dir));
    assert_eq!(
        std::fs::read_to_string(cook.source_dir.join("nested/marker.txt")).unwrap(),
        "isolated copy"
    );
}

#[test]
fn test_prep_isolated_local_path_source_uses_hermetic_file_list_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let recipe_dir = dir.path().join("recipe");
    let workspace = recipe_dir.join("src");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("included.txt"), "included\n").unwrap();
    std::fs::write(workspace.join("excluded.txt"), "excluded\n").unwrap();
    let mut hermetic_files = canonical_local_file_list(&workspace, CiMode::Off).unwrap();
    hermetic_files.retain(|file| file.relative_path == Path::new("included.txt"));

    let kitchen = Kitchen::new(KitchenConfig {
        recipe_source_base_dir: Some(recipe_dir),
        use_isolation: true,
        hermetic_local_files: Some(hermetic_files),
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.source = SourceSection::Local(LocalSourceSection {
        path: PathBuf::from("./src"),
    });

    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    cook.prep().unwrap();
    cook.unpack().unwrap();

    assert_eq!(
        std::fs::read_to_string(cook.source_dir.join("included.txt")).unwrap(),
        "included\n"
    );
    assert!(!cook.source_dir.join("excluded.txt").exists());
}

#[test]
fn test_prep_local_path_source_records_local_provenance_marker() {
    let dir = tempfile::tempdir().unwrap();
    let recipe_dir = dir.path().join("recipe");
    let workspace = recipe_dir.join("src");
    std::fs::create_dir_all(&workspace).unwrap();

    let kitchen = Kitchen::new(KitchenConfig {
        recipe_source_base_dir: Some(recipe_dir),
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.source = SourceSection::Local(LocalSourceSection {
        path: PathBuf::from("./src"),
    });

    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    cook.prep().unwrap();

    assert_eq!(cook.provenance.upstream_url.as_deref(), Some("local:./src"));
    assert!(
        cook.provenance.upstream_hash.is_none(),
        "local source provenance should leave upstream_hash unset until tree hashing exists"
    );
}

#[test]
fn test_patch_local_path_resolves_relative_to_recipe_source_base_dir() {
    let dir = tempfile::tempdir().unwrap();
    let recipe_dir = dir.path().join("recipe");
    let patch_dir = recipe_dir.join("patches");
    std::fs::create_dir_all(&patch_dir).unwrap();
    std::fs::write(
        patch_dir.join("fix.patch"),
        r#"--- file.txt
+++ file.txt
@@ -1 +1 @@
-old
+new
"#,
    )
    .unwrap();

    let kitchen = Kitchen::new(KitchenConfig {
        recipe_source_base_dir: Some(recipe_dir),
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.patches = Some(PatchSection {
        files: vec![PatchInfo {
            file: "patches/fix.patch".to_string(),
            checksum: None,
            strip: 0,
            condition: None,
        }],
    });

    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    std::fs::write(cook.source_dir.join("file.txt"), "old\n").unwrap();

    cook.patch().unwrap();

    assert_eq!(
        std::fs::read_to_string(cook.source_dir.join("file.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn test_patch_local_path_substitutes_recipe_variables() {
    let dir = tempfile::tempdir().unwrap();
    let recipe_dir = dir.path().join("recipe");
    let patch_dir = recipe_dir.join("patches");
    std::fs::create_dir_all(&patch_dir).unwrap();
    std::fs::write(
        patch_dir.join("1.0.0.patch"),
        r#"--- file.txt
+++ file.txt
@@ -1 +1 @@
-old
+new
"#,
    )
    .unwrap();

    let kitchen = Kitchen::new(KitchenConfig {
        recipe_source_base_dir: Some(recipe_dir),
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.patches = Some(PatchSection {
        files: vec![PatchInfo {
            file: "patches/%(version)s.patch".to_string(),
            checksum: None,
            strip: 0,
            condition: None,
        }],
    });

    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    std::fs::write(cook.source_dir.join("file.txt"), "old\n").unwrap();

    cook.patch().unwrap();

    assert_eq!(
        std::fs::read_to_string(cook.source_dir.join("file.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn test_hermetic_local_patch_requires_recipe_source_base_dir() {
    let kitchen = Kitchen::new(KitchenConfig {
        hermetic_evidence: Some(dummy_hermetic_evidence()),
        pristine_mode: true,
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.patches = Some(PatchSection {
        files: vec![PatchInfo {
            file: "patches/fix.patch".to_string(),
            checksum: None,
            strip: 0,
            condition: None,
        }],
    });
    let mut cook = Cook::new(&kitchen, &recipe).unwrap();

    let error = cook.patch().unwrap_err();

    assert!(error.to_string().contains("hermetic"));
    assert!(error.to_string().contains("recipe source base dir"));
}

#[test]
fn test_cook_new_rejects_hermetic_evidence_without_pristine_mode() {
    let kitchen = Kitchen::new(KitchenConfig {
        hermetic_evidence: Some(dummy_hermetic_evidence()),
        pristine_mode: false,
        ..KitchenConfig::default()
    });
    let recipe = minimal_recipe();

    let error = match Cook::new(&kitchen, &recipe) {
        Ok(_) => panic!("expected hermetic evidence without pristine mode to be rejected"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("hermetic evidence"));
    assert!(error.to_string().contains("pristine mode"));
}

#[test]
fn test_simmer_rejects_command_local_source_date_epoch_override_in_hermetic_mode() {
    let kitchen = Kitchen::new(KitchenConfig {
        hermetic_evidence: Some(dummy_hermetic_evidence()),
        reproducibility: Some(ReproducibilityConfig::default()),
        pristine_mode: true,
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.build.make = Some("SOURCE_DATE_EPOCH=999 true".to_string());
    let mut cook = Cook::new(&kitchen, &recipe).unwrap();

    let error = cook.simmer().unwrap_err();

    assert!(error.to_string().contains("SOURCE_DATE_EPOCH"));
    assert!(error.to_string().contains("command-local"));
}

#[test]
fn test_simmer_rejects_shell_startup_env_in_hermetic_mode() {
    let cases = [("SHELLOPTS", "keyword"), ("BASHOPTS", "expand_aliases")];

    for (key, value) in cases {
        let kitchen = Kitchen::new(KitchenConfig {
            hermetic_evidence: Some(dummy_hermetic_evidence()),
            reproducibility: Some(ReproducibilityConfig::default()),
            pristine_mode: true,
            use_isolation: false,
            ..KitchenConfig::default()
        });
        let mut recipe = minimal_recipe();
        recipe
            .build
            .environment
            .insert(key.to_string(), value.to_string());
        recipe.build.make = Some("make SOURCE_DATE_EPOCH=999".to_string());
        let mut cook = Cook::new(&kitchen, &recipe).unwrap();

        let error = cook.simmer().unwrap_err();

        assert!(
            error.to_string().contains(key),
            "expected {key} rejection, got: {error}"
        );
    }
}

#[test]
fn test_simmer_rejects_make_override_env_in_hermetic_mode() {
    for (key, value, expected) in [
        ("MAKEOVERRIDES", "CFLAGS=bad", "CFLAGS"),
        ("MAKEFILES", "evil.mk", "MAKEFILES"),
    ] {
        let kitchen = Kitchen::new(KitchenConfig {
            hermetic_evidence: Some(dummy_hermetic_evidence()),
            reproducibility: Some(ReproducibilityConfig::default()),
            pristine_mode: true,
            use_isolation: false,
            ..KitchenConfig::default()
        });
        let mut recipe = minimal_recipe();
        recipe
            .build
            .environment
            .insert(key.to_string(), value.to_string());
        recipe.build.make = Some("true".to_string());
        let mut cook = Cook::new(&kitchen, &recipe).unwrap();

        let error = cook.simmer().unwrap_err();

        assert!(error.to_string().contains(key));
        assert!(error.to_string().contains(expected));
    }
}

#[test]
fn test_hermetic_check_phase_env_guard_fails_closed() {
    let kitchen = Kitchen::new(KitchenConfig {
        hermetic_evidence: Some(dummy_hermetic_evidence()),
        reproducibility: Some(ReproducibilityConfig::default()),
        pristine_mode: true,
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.build.check = Some("SOURCE_DATE_EPOCH=999 true".to_string());
    let mut cook = Cook::new(&kitchen, &recipe).unwrap();

    let error = cook.simmer().unwrap_err();

    assert!(error.to_string().contains("SOURCE_DATE_EPOCH"));
    assert!(error.to_string().contains("command-local"));
}

#[cfg(unix)]
#[test]
fn test_prep_isolated_local_path_source_rejects_nested_relative_symlink_escape() {
    let dir = tempfile::tempdir().unwrap();
    let recipe_dir = dir.path().join("recipe");
    let workspace = recipe_dir.join("src");
    let outside = recipe_dir.join("outside");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("marker.txt"), "escaped").unwrap();
    std::os::unix::fs::symlink("../outside/marker.txt", workspace.join("escape.txt")).unwrap();

    let kitchen = Kitchen::new(KitchenConfig {
        recipe_source_base_dir: Some(recipe_dir),
        use_isolation: true,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.source = SourceSection::Local(LocalSourceSection {
        path: PathBuf::from("./src"),
    });

    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    let error = cook.prep().unwrap_err();

    assert!(
        error
            .to_string()
            .contains("Local source symlink must stay within the source directory"),
        "expected nested symlink escape rejection, got: {error}"
    );
}

#[cfg(unix)]
#[test]
fn test_prep_isolated_local_path_source_rejects_absolute_symlink_escape() {
    let dir = tempfile::tempdir().unwrap();
    let recipe_dir = dir.path().join("recipe");
    let workspace = recipe_dir.join("src");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let escaped = outside.join("marker.txt");
    std::fs::write(&escaped, "escaped").unwrap();
    std::os::unix::fs::symlink(&escaped, workspace.join("escape.txt")).unwrap();

    let kitchen = Kitchen::new(KitchenConfig {
        recipe_source_base_dir: Some(recipe_dir),
        use_isolation: true,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.source = SourceSection::Local(LocalSourceSection {
        path: PathBuf::from("./src"),
    });

    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    let error = cook.prep().unwrap_err();

    assert!(
        error
            .to_string()
            .contains("Local source symlink must stay within the source directory"),
        "expected absolute symlink escape rejection, got: {error}"
    );
}

#[cfg(unix)]
#[test]
fn test_prep_local_path_source_rejects_symlink_escape() {
    let dir = tempfile::tempdir().unwrap();
    let recipe_dir = dir.path().join("recipe");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&recipe_dir).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("marker.txt"), "escaped").unwrap();
    std::os::unix::fs::symlink(&outside, recipe_dir.join("src")).unwrap();

    let kitchen = Kitchen::new(KitchenConfig {
        recipe_source_base_dir: Some(recipe_dir),
        use_isolation: false,
        ..KitchenConfig::default()
    });
    let mut recipe = minimal_recipe();
    recipe.source = SourceSection::Local(LocalSourceSection {
        path: PathBuf::from("./src"),
    });

    let mut cook = Cook::new(&kitchen, &recipe).unwrap();
    let error = cook.prep().unwrap_err();

    assert!(
        error
            .to_string()
            .contains("must stay within the recipe directory"),
        "expected symlink escape rejection, got: {error}"
    );
}

#[test]
fn test_chroot_command_translation_maps_destdir_substitutions() {
    let sysroot = Path::new("/tmp/conary-seed/sysroot");
    let command = "mkdir -p /tmp/conary-seed/sysroot/var/tmp/dest && touch /tmp/conary-seed/sysroot/var/tmp/dest/ok";

    assert_eq!(
        translate_command_for_chroot(command, sysroot),
        "mkdir -p /var/tmp/dest && touch /var/tmp/dest/ok"
    );
}

fn dummy_hermetic_evidence() -> HermeticBuildEvidence {
    HermeticBuildEvidence {
        schema_version: HERMETIC_EVIDENCE_SCHEMA,
        build_input: BuildInputIdentity {
            recipe: RecipeIdentity::ExplicitRecipe {
                path: "recipe.toml".to_string(),
                hash: "sha256:recipe".to_string(),
            },
            source: SourceIdentity::Archive {
                url: "https://example.invalid/test.tar.gz".to_string(),
                checksum: "sha256:source".to_string(),
            },
            additional_sources: Vec::new(),
            patches: Vec::new(),
            local_tree: None,
            builder_environment: BuilderEnvironmentIdentity {
                kind: BuilderEnvironmentKind::Pristine,
                sysroot_hash: Some(
                    "sha256:1111111111111111111111111111111111111111111111111111111111111111"
                        .to_string(),
                ),
                toolchain_hash: None,
                diagnostics: Vec::new(),
            },
        },
        dependency_lock: DependencyLock::default(),
        command_risk: BuildCommandRiskReport::no_findings(),
        reproducibility: ReproducibilityRecord {
            source_date_epoch: Some(0),
            path_remap_count: 2,
            env_keys: vec![
                "CFLAGS".to_string(),
                "CXXFLAGS".to_string(),
                "RUSTFLAGS".to_string(),
                "SOURCE_DATE_EPOCH".to_string(),
            ],
        },
        divergence: Default::default(),
        diagnostics: Vec::new(),
    }
}

/// A mapped build identity must preserve on-disk and packaged ownership for
/// managed and explicitly provided destinations, including in-source builds.
/// Run this test with root privileges to exercise the privileged projection.
#[cfg(unix)]
#[test]
fn test_isolated_build_step_preserves_root_ownership_authority() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let kitchen = Kitchen::new(KitchenConfig {
        source_cache: dir.path().join("source-cache"),
        use_isolation: true,
        memory_limit: 8 * 1024 * 1024 * 1024,
        ..KitchenConfig::default()
    });
    let recipe = minimal_recipe();

    let mut managed_cook = Cook::new(&kitchen, &recipe).unwrap();
    assert_isolated_build_step_root_authority(&mut managed_cook);

    let caller_dest = dir.path().join("caller-dest");
    std::fs::create_dir_all(&caller_dest).unwrap();
    let mut caller_cook = Cook::new_with_dest(&kitchen, &recipe, &caller_dest).unwrap();
    assert_isolated_build_step_root_authority(&mut caller_cook);
}

#[cfg(unix)]
fn assert_isolated_build_step_root_authority(cook: &mut Cook<'_>) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let input_contents = "root-authority-input\n";
    let input_path = cook.source_dir.join("input.txt");
    std::fs::write(&input_path, input_contents).unwrap();
    std::fs::set_permissions(&input_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let input_before = std::fs::metadata(&input_path).unwrap();

    let dest = cook.dest_dir.clone();
    let dest_before = std::fs::metadata(&dest).unwrap();

    let workdir = cook.source_dir.clone();
    let env = vec![("DESTDIR".to_string(), dest.display().to_string())];
    let command = "IFS= read -r value < input.txt \
        && printf '%s\\n' \"$value\" > build-artifact.txt \
        && printf '%s\\n' \"$value\" > \"$DESTDIR/output.txt\"";

    cook.run_build_step_isolated("install", command, &workdir, &env)
        .unwrap();

    let input_after = std::fs::metadata(&input_path).unwrap();
    assert_eq!(
        std::fs::read_to_string(&input_path).unwrap(),
        input_contents
    );
    assert_eq!(
        input_after.mode() & 0o7777,
        0o600,
        "isolated build must not loosen the mode-0600 input"
    );
    assert_eq!(input_after.uid(), input_before.uid());
    assert_eq!(input_after.gid(), input_before.gid());

    let artifact_path = workdir.join("build-artifact.txt");
    assert_eq!(
        std::fs::read_to_string(&artifact_path).unwrap(),
        input_contents,
        "in-source build artifact must be written by the isolated step"
    );

    let output_path = dest.join("output.txt");
    assert_eq!(
        std::fs::read_to_string(&output_path).unwrap(),
        input_contents,
        "dest output must contain the input value"
    );
    let output_metadata = std::fs::metadata(&output_path).unwrap();
    assert_eq!(
        output_metadata.uid(),
        0,
        "root-run isolated build must leave dest output owned by uid 0"
    );
    assert_eq!(
        output_metadata.gid(),
        0,
        "root-run isolated build must leave dest output owned by gid 0"
    );

    let dest_after = std::fs::metadata(&dest).unwrap();
    assert_eq!(dest_after.uid(), dest_before.uid());
    assert_eq!(dest_after.gid(), dest_before.gid());
    assert_eq!(dest_after.mode() & 0o7777, dest_before.mode() & 0o7777);

    let built = CcsBuilder::new(CcsManifest::new_minimal("root-authority", "1.0.0"), &dest)
        .unwrap()
        .build()
        .unwrap();
    let entry = built
        .files
        .iter()
        .find(|entry| entry.path == "/output.txt")
        .expect("CCS build must capture the dest output");
    assert_eq!(entry.node.user, PayloadIdentity::Numeric { id: 0 });
    assert_eq!(entry.node.group, PayloadIdentity::Numeric { id: 0 });
}
