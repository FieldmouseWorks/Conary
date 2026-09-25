// apps/conary/src/commands/install/transaction/tests.rs

#![cfg(test)]

use crate::commands::ccs::selected_ccs_resolution_capabilities;
use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};
use conary_core::repository::dependency_model::{ProvidedCapability, RepositoryCapabilityKind};
use std::collections::HashMap;

fn selected_names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

fn file_provide_names(capabilities: &[ProvidedCapability]) -> Vec<&str> {
    capabilities
        .iter()
        .filter(|capability| capability.kind == RepositoryCapabilityKind::File)
        .map(|capability| capability.name.as_str())
        .collect()
}

/// Sign a CCS fixture and return its verified parsed package.
fn verified_ccs_package(
    temp_dir: &std::path::Path,
    name: &str,
    entries: &[(&str, &str)],
    declared_file_provides: &[&str],
    declared_capabilities: &[&str],
) -> conary_core::ccs::CcsPackage {
    let mut manifest = CcsManifest::new_minimal(name, "1.0.0");
    manifest.provides.files = declared_file_provides
        .iter()
        .map(|path| (*path).to_string())
        .collect();
    manifest.provides.capabilities = declared_capabilities
        .iter()
        .map(|capability| (*capability).to_string())
        .collect();

    let mut files = Vec::new();
    let mut blobs = HashMap::new();
    let mut component_files: HashMap<String, Vec<conary_core::ccs::FileEntry>> = HashMap::new();
    let mut total_size = 0;
    for (path, component) in entries {
        let content = format!("payload for {path}").into_bytes();
        let sha256 = conary_core::hash::sha256(&content);
        let size = content.len() as u64;
        total_size += size;
        let file = ccs_regular_file(
            (*path).to_string(),
            sha256.clone(),
            size,
            0o100755,
            (*component).to_string(),
        );
        component_files
            .entry((*component).to_string())
            .or_default()
            .push(file.clone());
        blobs.insert(sha256, content);
        files.push(file);
    }
    let components = component_files
        .into_iter()
        .map(|(name, files)| {
            let size = files
                .iter()
                .map(|file| file.content.as_ref().map_or(0, |content| content.size))
                .sum();
            (
                name.clone(),
                ComponentData {
                    name,
                    files,
                    hash: "test-component".to_string(),
                    size,
                },
            )
        })
        .collect();

    let result = BuildResult {
        manifest,
        components,
        files: files.clone(),
        payloads: conary_core::ccs::builder::payloads_from_bounded_memory_for_tests(&files, blobs)
            .unwrap(),
        total_size,
        chunked: false,
        chunk_stats: None,
    };
    let package_path = temp_dir.join(format!("{name}.ccs"));
    let trust_policy_path = write_signed_test_package(&result, &package_path);
    let policy = conary_core::ccs::TrustPolicy::from_file(&trust_policy_path).unwrap();
    let verified = conary_core::ccs::verify::verify_package(&package_path, &policy).unwrap();
    conary_core::ccs::CcsPackage::from_verified_archive(package_path.to_str().unwrap(), &verified)
        .unwrap()
}

#[test]
fn selected_component_keeps_shipped_file_provide() {
    let temp = tempfile::tempdir().unwrap();
    let package = verified_ccs_package(
        temp.path(),
        "selected-shipped",
        &[("/bin/sh", "runtime")],
        &["/bin/sh"],
        &[],
    );

    let capabilities =
        selected_ccs_resolution_capabilities(&package, &selected_names(&["runtime"])).unwrap();

    assert!(file_provide_names(&capabilities).contains(&"/bin/sh"));
}

#[test]
fn unselected_component_drops_shipped_file_provide() {
    let temp = tempfile::tempdir().unwrap();
    let package = verified_ccs_package(
        temp.path(),
        "unselected-shipped",
        &[("/bin/sh", "runtime")],
        &["/bin/sh"],
        &[],
    );

    let unselected =
        selected_ccs_resolution_capabilities(&package, &selected_names(&["docs"])).unwrap();
    assert!(!file_provide_names(&unselected).contains(&"/bin/sh"));

    // Positive control: the same fixture keeps the provide once the shipping
    // component is selected.
    let selected =
        selected_ccs_resolution_capabilities(&package, &selected_names(&["runtime"])).unwrap();
    assert!(file_provide_names(&selected).contains(&"/bin/sh"));
}

#[test]
fn declared_file_provide_absent_from_payload_is_retained() {
    let temp = tempfile::tempdir().unwrap();
    let package = verified_ccs_package(
        temp.path(),
        "unshipped-declared",
        &[("/usr/bin/sh", "runtime")],
        &["/bin/sh"],
        &[],
    );

    let capabilities =
        selected_ccs_resolution_capabilities(&package, &selected_names(&["docs"])).unwrap();

    assert!(
        file_provide_names(&capabilities).contains(&"/bin/sh"),
        "a source-format declared path the package does not ship must survive selection"
    );
}

#[test]
fn non_file_capabilities_are_never_filtered() {
    let temp = tempfile::tempdir().unwrap();
    let package = verified_ccs_package(
        temp.path(),
        "non-file-capability",
        &[("/bin/sh", "runtime")],
        &["/bin/sh"],
        &["virtual-api"],
    );

    let capabilities =
        selected_ccs_resolution_capabilities(&package, &selected_names(&["docs"])).unwrap();

    assert!(
        capabilities.iter().any(|capability| {
            capability.kind == RepositoryCapabilityKind::Virtual && capability.name == "virtual-api"
        }),
        "non-File capabilities must survive a selection that drops a File provide"
    );
}

#[test]
fn file_provide_and_entry_path_match_through_the_lexical_parser() {
    let temp = tempfile::tempdir().unwrap();
    let package = verified_ccs_package(
        temp.path(),
        "parser-match",
        &[("bin/sh", "runtime")],
        &["/bin/sh"],
        &[],
    );

    let selected =
        selected_ccs_resolution_capabilities(&package, &selected_names(&["runtime"])).unwrap();
    assert!(file_provide_names(&selected).contains(&"/bin/sh"));

    let unselected =
        selected_ccs_resolution_capabilities(&package, &selected_names(&["docs"])).unwrap();
    assert!(!file_provide_names(&unselected).contains(&"/bin/sh"));
}

fn write_signed_test_package(
    result: &conary_core::ccs::BuildResult,
    package_path: &std::path::Path,
) -> std::path::PathBuf {
    let signing_key =
        conary_core::ccs::signing::SigningKeyPair::generate().with_key_id("test-authority");
    conary_core::ccs::builder::write_signed_current_ccs_package(
        result,
        package_path,
        &signing_key,
        false,
    )
    .unwrap();
    let policy_path = package_path.with_extension("trust-policy.toml");
    std::fs::write(
        &policy_path,
        format!(
            "trusted_keys = [\"{}\"]\nrequire_timestamp = false\n",
            signing_key.public_key_base64()
        ),
    )
    .unwrap();
    policy_path
}

fn ccs_regular_file(
    path: impl Into<String>,
    sha256: impl Into<String>,
    size: u64,
    mode: u32,
    component: impl Into<String>,
) -> conary_core::ccs::FileEntry {
    let mut node = conary_core::payload::PayloadNode::regular(mode & 0o7777);
    node.user = conary_core::payload::PayloadIdentity::Numeric {
        id: u64::from(unsafe { libc::geteuid() }),
    };
    node.group = conary_core::payload::PayloadIdentity::Numeric {
        id: u64::from(unsafe { libc::getegid() }),
    };
    conary_core::ccs::FileEntry {
        path: path.into(),
        node,
        content: Some(conary_core::payload::PayloadContentAuthority {
            sha256: sha256.into(),
            size,
        }),
        component: component.into(),
        chunks: None,
    }
}
