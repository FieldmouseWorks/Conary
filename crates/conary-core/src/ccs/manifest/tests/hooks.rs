// crates/conary-core/src/ccs/manifest/tests/hooks.rs

#![cfg(test)]

use super::super::*;

#[test]
fn hooks_classify_script_service_and_declarative_entries() {
    let mut hooks = Hooks::default();
    assert!(!hooks.has_script_hooks());
    assert!(!hooks.has_service_hooks());
    assert!(!hooks.has_declarative_hooks());
    assert!(!hooks.has_irreversible_hooks_for_try_root(HookExecutionRoot::HostRoot));

    hooks.directories.push(DirectoryHook {
        path: "/var/lib/conary-test".to_string(),
        mode: "0755".to_string(),
        owner: "root".to_string(),
        group: "root".to_string(),
        cleanup: None,
        reversible: None,
    });
    assert!(hooks.has_declarative_hooks());
    assert!(!hooks.has_irreversible_hooks_for_try_root(HookExecutionRoot::TryRoot));
    assert!(!hooks.has_irreversible_hooks_for_try_root(HookExecutionRoot::GenerationRoot));
    assert!(hooks.has_irreversible_hooks_for_try_root(HookExecutionRoot::HostRoot));

    hooks.services.push(Service {
        name: "conary-test.service".to_string(),
        action: ServiceAction::Restart,
        reversible: None,
    });
    assert!(hooks.has_service_hooks());
    assert!(hooks.has_irreversible_hooks_for_try_root(HookExecutionRoot::TryRoot));

    hooks.post_install = Some(ScriptHook {
        script: "echo post-install".to_string(),
        interpreter: "/bin/sh".to_string(),
        reversible: None,
    });
    assert!(hooks.has_script_hooks());
    assert!(hooks.has_irreversible_hooks_for_try_root(HookExecutionRoot::GenerationRoot));
}

#[test]
fn omitted_reversible_fields_use_current_hook_semantics() {
    let toml = r#"
[package]
name = "hook-defaults"
version = "1.0.0"
version_scheme = "conary"
release = "1"
kind = "package"
description = "hook defaults"

[[hooks.users]]
name = "hookuser"
system = true

[[hooks.services]]
name = "hook-defaults.service"
action = "restart"

[hooks.post_install]
script = "echo post-install"
interpreter = "/bin/sh"
"#;

    let manifest = CcsManifest::parse(toml).expect("parse manifest without reversible fields");

    assert_eq!(manifest.hooks.users[0].reversible, None);
    assert_eq!(manifest.hooks.services[0].reversible, None);
    assert_eq!(
        manifest
            .hooks
            .post_install
            .as_ref()
            .expect("post-install hook")
            .reversible,
        None
    );
    assert!(
        manifest
            .hooks
            .has_irreversible_hooks_for_try_root(HookExecutionRoot::TryRoot)
    );

    let encoded = manifest.to_toml().expect("serialize manifest");
    assert!(!encoded.contains("reversible"));

    let declarative_only = CcsManifest::parse(
        r#"
[package]
name = "declarative-defaults"
version = "1.0.0"
version_scheme = "conary"
release = "1"
kind = "package"
description = "declarative defaults"

[[hooks.groups]]
name = "hookgroup"
system = true
"#,
    )
    .expect("parse declarative manifest");

    assert!(
        !declarative_only
            .hooks
            .has_irreversible_hooks_for_try_root(HookExecutionRoot::TryRoot)
    );
    assert!(
        !declarative_only
            .hooks
            .has_irreversible_hooks_for_try_root(HookExecutionRoot::GenerationRoot)
    );
    assert!(
        declarative_only
            .hooks
            .has_irreversible_hooks_for_try_root(HookExecutionRoot::HostRoot)
    );
}

#[test]
fn script_hook_requires_an_explicit_interpreter() {
    let toml = r#"
[package]
name = "hook-interpreter-required"
version = "1.0.0"
version_scheme = "conary"
release = "1"
kind = "package"

[hooks.post_install]
script = "echo post-install"
"#;

    let error = CcsManifest::parse(toml).unwrap_err();
    assert!(matches!(error, ManifestError::ParseError(_)));
}

fn interpreter_manifest(interpreter: &str) -> String {
    format!(
        r#"
[package]
name = "hook-interpreter-path"
version = "1.0.0"
version_scheme = "conary"
release = "1"
kind = "package"
description = "Hook interpreter path validation fixture"

[hooks.post_install]
script = "echo post-install"
interpreter = "{interpreter}"
"#
    )
}

#[test]
fn script_hook_rejects_relative_and_traversing_interpreters() {
    // Positive control: the same fixture with an absolute, normalized
    // interpreter parses, so each rejection below comes from the path rule.
    CcsManifest::parse(&interpreter_manifest("/bin/sh")).unwrap();
    for interpreter in ["bin/sh", "/usr/../bin/sh"] {
        let error = CcsManifest::parse(&interpreter_manifest(interpreter)).unwrap_err();
        assert!(
            matches!(error, ManifestError::Invalid(_)),
            "expected invalid interpreter diagnostic for {interpreter}, got {error}"
        );
    }
}

#[test]
fn script_hook_rejects_an_interpreter_no_executor_implements() {
    // Positive control: the same fixture with the implemented interpreter
    // parses, so the rejection below comes from the implemented-set rule.
    CcsManifest::parse(&interpreter_manifest("/bin/sh")).unwrap();

    match CcsManifest::parse(&interpreter_manifest("/usr/bin/python3")).unwrap_err() {
        ManifestError::Invalid(message) => assert_eq!(
            message,
            "hooks.post_install.interpreter: CCS hook interpreter /usr/bin/python3 is not implemented (supported: /bin/sh)"
        ),
        other => panic!("expected an invalid-manifest error, got {other:?}"),
    }
}
