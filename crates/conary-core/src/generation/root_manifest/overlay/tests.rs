// crates/conary-core/src/generation/root_manifest/overlay/tests.rs

#![cfg(test)]

use super::*;
use crate::filesystem::CasStore;
use crate::payload::{PayloadContentAuthority, PayloadIdentity, PayloadNode};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Capture and decode one frozen transaction upper without reading unchanged
/// lower content.
///
/// Callers seed the upper root metadata from `prior` before mounting. A root
/// metadata difference can then be represented without guessing whether it
/// was transaction-owned.
fn decode_selected_root_overlay_upper(
    upper: &Path,
    prior: &CapturedSelectedRoot,
    cas: &dyn PrivateCasWriter,
    profile: &SelectedRootOverlayProfile,
) -> crate::Result<SelectedRootManifestDelta> {
    profile.validate()?;
    prior.generation.validate()?;
    prior.state.validate()?;
    let (root, entries) =
        scan_selected_root_overlay_upper(upper, cas, "selected-root-overlay-v1", profile)?;
    decode_captured_upper(root, entries, prior, profile)
}

fn decode_captured_upper(
    root: ResolvedPayloadNode,
    entries: Vec<GenerationRootEntry>,
    prior: &CapturedSelectedRoot,
    profile: &SelectedRootOverlayProfile,
) -> crate::Result<SelectedRootManifestDelta> {
    let decoded = decode_upper_operations(root, entries, profile, |path| {
        Ok(prior_directory_exists(prior, path))
    })?;
    let mut delta = decoded.into_delta(&prior.generation.root);
    expand_prior_hardlink_authority(
        prior,
        &delta.removals,
        &delta.opaque_directories,
        &delta.copied_up_origins,
        &mut delta.upserts,
    )?;
    let delta = delta.finish()?;
    // The complete-capture test helper validates the decoded delta by
    // materializing it against the retained roots.
    delta.apply(prior)?;
    Ok(delta)
}

fn prior_directory_exists(prior: &CapturedSelectedRoot, path: &str) -> bool {
    prior
        .generation
        .entries
        .iter()
        .chain(&prior.state.entries)
        .any(|entry| {
            entry.path == path && matches!(entry.node.source.kind, PayloadNodeKind::Directory)
        })
}

fn expand_prior_hardlink_authority(
    prior: &CapturedSelectedRoot,
    removals: &[String],
    opaque_directories: &[String],
    copied_up_origins: &BTreeSet<String>,
    upserts: &mut BTreeMap<String, GenerationRootEntry>,
) -> crate::Result<()> {
    let mut groups = BTreeMap::<String, Vec<GenerationRootEntry>>::new();
    for entry in prior.generation.entries.iter().chain(&prior.state.entries) {
        let identity = match &entry.node.source.kind {
            PayloadNodeKind::Regular {
                hardlink_identity: Some(identity),
            }
            | PayloadNodeKind::Hardlink { identity, .. } => identity,
            _ => continue,
        };
        groups
            .entry(identity.clone())
            .or_default()
            .push(entry.clone());
    }
    expand_prior_hardlink_groups(
        groups.into_values().collect(),
        removals,
        opaque_directories,
        copied_up_origins,
        upserts,
    )
}

#[test]
fn profile_is_explicit_and_rejects_hardlink_breaking_index_off() {
    let profile = SelectedRootOverlayProfile::trusted();
    assert_eq!(
        profile.mount_options().unwrap(),
        [
            "index=on",
            "redirect_dir=nofollow",
            "metacopy=off",
            "xino=off",
            "nfs_export=off",
            "verity=off",
        ]
    );
    let mut invalid = profile;
    invalid.index = false;
    assert!(invalid.validate().is_err());
}

#[test]
fn upper_root_encoding_round_trips_payload_private_namespace() {
    let profile = user_profile();
    let mut root = directory_node();
    root.source
        .xattrs
        .insert("user.overlay.payload-owned".into(), b"exact".to_vec());
    let encoded = encode_selected_root_overlay_upper_node(&root, &profile).unwrap();
    assert_eq!(
        encoded
            .source
            .xattrs
            .get("user.overlay.overlay.payload-owned"),
        Some(&b"exact".to_vec())
    );
    assert_eq!(
        decode_captured_upper(encoded, Vec::new(), &captured(Vec::new()), &profile)
            .unwrap()
            .root,
        Some(root)
    );
}

#[test]
fn upper_capture_prunes_ephemeral_subtrees_before_delta_decode() {
    let workspace = tempfile::tempdir().unwrap();
    let upper = workspace.path().join("upper");
    std::fs::create_dir_all(upper.join("usr/lib")).unwrap();
    std::fs::create_dir_all(upper.join("run/conary-private")).unwrap();
    std::fs::write(upper.join("usr/lib/changed"), b"publish").unwrap();
    std::fs::write(upper.join("run/conary-private/ignored"), b"ephemeral").unwrap();
    let cas = CasStore::new(workspace.path().join("objects")).unwrap();
    let prior = captured(vec![directory_entry("/usr"), directory_entry("/usr/lib")]);

    let delta = decode_selected_root_overlay_upper(&upper, &prior, &cas, &user_profile()).unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::ensure_current(&conn).unwrap();
    let snapshot = SelectedRootSnapshot::capture(&conn, &prior).unwrap();
    let indexed =
        decode_selected_root_overlay_upper_indexed(&upper, &conn, snapshot, &cas, &user_profile())
            .unwrap();

    assert!(
        delta
            .upserts
            .iter()
            .any(|entry| entry.path == "/usr/lib/changed")
    );
    assert!(
        delta
            .upserts
            .iter()
            .all(|entry| !entry.path.starts_with("/run"))
    );
    assert_eq!(indexed, delta);
}

#[test]
fn decodes_documented_whiteouts_and_opaque_markers() {
    let prior = captured(vec![
        directory_entry("/opt"),
        directory_entry("/opt/tree"),
        regular_entry("/opt/tree/lower", content('a', 1)),
        regular_entry("/opt/remove", content('b', 2)),
    ]);
    let profile = user_profile();
    let mut opaque = directory_entry("/opt/tree");
    opaque
        .node
        .source
        .xattrs
        .insert("user.overlay.opaque".into(), b"y".to_vec());
    let mut whiteout = regular_entry("/opt/remove", content('0', 0));
    whiteout
        .node
        .source
        .xattrs
        .insert("user.overlay.whiteout".into(), Vec::new());
    let delta =
        decode_captured_upper(directory_node(), vec![opaque, whiteout], &prior, &profile).unwrap();

    assert_eq!(delta.removals, ["/opt/remove"]);
    assert_eq!(delta.opaque_directories, ["/opt/tree"]);
    let applied = delta.apply(&prior).unwrap();
    assert!(entry(&applied, "/opt/remove").is_none());
    assert!(entry(&applied, "/opt/tree/lower").is_none());
}

#[test]
fn opaque_x_only_enables_whiteout_scanning() {
    let prior = captured(vec![
        directory_entry("/opt"),
        directory_entry("/opt/tree"),
        regular_entry("/opt/tree/lower", content('a', 1)),
    ]);
    let mut directory = directory_entry("/opt/tree");
    directory
        .node
        .source
        .xattrs
        .insert("user.overlay.opaque".into(), b"x".to_vec());
    let delta =
        decode_captured_upper(directory_node(), vec![directory], &prior, &user_profile()).unwrap();
    assert!(delta.opaque_directories.is_empty());
    assert!(entry(&delta.apply(&prior).unwrap(), "/opt/tree/lower").is_some());
}

#[test]
fn opaque_y_on_new_upper_only_directory_is_redundant() {
    let profile = user_profile();
    let mut directory = directory_entry("/usr");
    directory
        .node
        .source
        .xattrs
        .insert("user.overlay.opaque".into(), b"y".to_vec());

    let delta = decode_captured_upper(
        directory_node(),
        vec![directory],
        &captured(Vec::new()),
        &profile,
    )
    .unwrap();

    assert!(delta.opaque_directories.is_empty());
    assert_eq!(delta.upserts[0].path, "/usr");
    assert!(
        delta
            .apply(&captured(Vec::new()))
            .unwrap()
            .generation
            .entries
            .iter()
            .any(|entry| entry.path == "/usr")
    );
}

#[test]
fn unescapes_payload_xattrs_and_rejects_unknown_private_markers() {
    let prior = captured(vec![directory_entry("/usr")]);
    let mut escaped = directory_entry("/usr");
    escaped
        .node
        .source
        .xattrs
        .insert("user.overlay.overlay.metacopy".into(), b"payload".to_vec());
    let delta =
        decode_captured_upper(directory_node(), vec![escaped], &prior, &user_profile()).unwrap();
    assert_eq!(
        delta.upserts[0]
            .node
            .source
            .xattrs
            .get("user.overlay.metacopy")
            .map(Vec::as_slice),
        Some(b"payload".as_slice())
    );

    let mut unknown = directory_entry("/usr");
    unknown
        .node
        .source
        .xattrs
        .insert("user.overlay.future".into(), b"1".to_vec());
    assert!(
        decode_captured_upper(directory_node(), vec![unknown], &prior, &user_profile(),).is_err()
    );
}

#[test]
fn strips_index_origin_from_upper_root_metadata() {
    let prior = captured(vec![directory_entry("/usr")]);
    let mut upper_root = directory_node();
    upper_root
        .source
        .xattrs
        .insert("user.overlay.origin".into(), b"root-handle".to_vec());
    let delta = decode_captured_upper(upper_root, Vec::new(), &prior, &user_profile()).unwrap();
    assert!(delta.root.is_none());
}

#[test]
fn rejects_redirect_metacopy_and_protected_inode_markers() {
    let prior = captured(vec![directory_entry("/usr")]);
    for marker in ["redirect", "metacopy", "protattr"] {
        let mut entry = directory_entry("/usr");
        entry
            .node
            .source
            .xattrs
            .insert(format!("user.overlay.{marker}"), b"value".to_vec());
        assert!(
            decode_captured_upper(directory_node(), vec![entry], &prior, &user_profile(),).is_err()
        );
    }
}

#[test]
fn copied_up_prior_hardlink_expands_complete_group_authority() {
    let identity = "prior:hardlink:1";
    let mut primary = regular_entry("/usr/lib/a", content('a', 4));
    primary.node.source.kind = PayloadNodeKind::Regular {
        hardlink_identity: Some(identity.into()),
    };
    let mut alias = primary.clone();
    alias.path = "/usr/lib/b".into();
    alias.content = None;
    alias.node.source.kind = PayloadNodeKind::Hardlink {
        target: primary.path.clone(),
        identity: identity.into(),
    };
    let prior = captured(vec![
        directory_entry("/usr"),
        directory_entry("/usr/lib"),
        primary,
        alias,
    ]);
    let mut changed = regular_entry("/usr/lib/b", content('c', 7));
    changed
        .node
        .source
        .xattrs
        .insert("user.overlay.origin".into(), b"opaque-handle".to_vec());
    let delta =
        decode_captured_upper(directory_node(), vec![changed], &prior, &user_profile()).unwrap();
    let applied = delta.apply(&prior).unwrap();
    assert_eq!(
        entry(&applied, "/usr/lib/a")
            .and_then(|entry| entry.content.as_ref())
            .map(|content| content.size),
        Some(7)
    );
    assert!(matches!(
        &entry(&applied, "/usr/lib/b").unwrap().node.source.kind,
        PayloadNodeKind::Hardlink { target, identity: found }
            if target == "/usr/lib/a" && found == identity
    ));
}

#[test]
fn indexed_decoder_matches_complete_projection_for_affected_hardlink_group() {
    let identity = "prior:hardlink:indexed";
    let mut primary = regular_entry("/usr/lib/a", content('a', 4));
    primary.node.source.kind = PayloadNodeKind::Regular {
        hardlink_identity: Some(identity.into()),
    };
    let mut alias = primary.clone();
    alias.path = "/usr/lib/b".into();
    alias.content = None;
    alias.node.source.kind = PayloadNodeKind::Hardlink {
        target: primary.path.clone(),
        identity: identity.into(),
    };
    let prior = captured(vec![
        directory_entry("/usr"),
        directory_entry("/usr/lib"),
        primary,
        alias,
    ]);
    let mut changed = regular_entry("/usr/lib/b", content('c', 7));
    changed
        .node
        .source
        .xattrs
        .insert("user.overlay.origin".into(), b"opaque-handle".to_vec());

    let expected = decode_captured_upper(
        directory_node(),
        vec![changed.clone()],
        &prior,
        &user_profile(),
    )
    .unwrap()
    .apply(&prior)
    .unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::ensure_current(&conn).unwrap();
    let snapshot = SelectedRootSnapshot::capture(&conn, &prior).unwrap();
    let decoded =
        decode_upper_operations(directory_node(), vec![changed], &user_profile(), |path| {
            Ok(snapshot
                .entry(&conn, path)?
                .is_some_and(|entry| matches!(entry.node.source.kind, PayloadNodeKind::Directory)))
        })
        .unwrap();
    let mut pending = decoded.into_delta(&snapshot.root(&conn).unwrap());
    let groups = indexed::affected_hardlink_groups(
        &conn,
        snapshot,
        &pending.removals,
        &pending.opaque_directories,
        &pending.copied_up_origins,
        &pending.upserts,
    )
    .unwrap();
    expand_prior_hardlink_groups(
        groups,
        &pending.removals,
        &pending.opaque_directories,
        &pending.copied_up_origins,
        &mut pending.upserts,
    )
    .unwrap();
    let actual = snapshot
        .apply_delta(&conn, &pending.finish().unwrap())
        .unwrap()
        .materialize(&conn)
        .unwrap();

    assert_eq!(actual, expected);
}

#[test]
fn removing_prior_hardlink_primary_reanchors_survivors() {
    let identity = "prior:hardlink:1";
    let mut primary = regular_entry("/usr/lib/a", content('a', 4));
    primary.node.source.kind = PayloadNodeKind::Regular {
        hardlink_identity: Some(identity.into()),
    };
    let mut alias = primary.clone();
    alias.path = "/usr/lib/b".into();
    alias.content = None;
    alias.node.source.kind = PayloadNodeKind::Hardlink {
        target: primary.path.clone(),
        identity: identity.into(),
    };
    let prior = captured(vec![
        directory_entry("/usr"),
        directory_entry("/usr/lib"),
        primary,
        alias,
    ]);
    let mut whiteout = regular_entry("/usr/lib/a", content('0', 0));
    whiteout
        .node
        .source
        .xattrs
        .insert("user.overlay.whiteout".into(), Vec::new());
    let applied = decode_captured_upper(directory_node(), vec![whiteout], &prior, &user_profile())
        .unwrap()
        .apply(&prior)
        .unwrap();
    assert!(entry(&applied, "/usr/lib/a").is_none());
    assert!(matches!(
        &entry(&applied, "/usr/lib/b").unwrap().node.source.kind,
        PayloadNodeKind::Regular {
            hardlink_identity: None
        }
    ));
}

#[test]
fn removing_only_prior_alias_clears_primary_group_identity() {
    let identity = "prior:hardlink:1";
    let mut primary = regular_entry("/usr/lib/a", content('a', 4));
    primary.node.source.kind = PayloadNodeKind::Regular {
        hardlink_identity: Some(identity.into()),
    };
    let mut alias = primary.clone();
    alias.path = "/usr/lib/b".into();
    alias.content = None;
    alias.node.source.kind = PayloadNodeKind::Hardlink {
        target: primary.path.clone(),
        identity: identity.into(),
    };
    let prior = captured(vec![
        directory_entry("/usr"),
        directory_entry("/usr/lib"),
        primary,
        alias,
    ]);
    let mut whiteout = regular_entry("/usr/lib/b", content('0', 0));
    whiteout
        .node
        .source
        .xattrs
        .insert("user.overlay.whiteout".into(), Vec::new());
    let applied = decode_captured_upper(directory_node(), vec![whiteout], &prior, &user_profile())
        .unwrap()
        .apply(&prior)
        .unwrap();
    assert!(entry(&applied, "/usr/lib/b").is_none());
    assert!(matches!(
        &entry(&applied, "/usr/lib/a").unwrap().node.source.kind,
        PayloadNodeKind::Regular {
            hardlink_identity: None
        }
    ));
}

#[test]
fn new_alias_joins_copied_up_prior_hardlink_group() {
    let prior_identity = "prior:hardlink:1";
    let upper_identity = "selected-root-overlay-v1:hardlink:1";
    let mut primary = regular_entry("/usr/lib/a", content('a', 4));
    primary.node.source.kind = PayloadNodeKind::Regular {
        hardlink_identity: Some(prior_identity.into()),
    };
    let mut alias = primary.clone();
    alias.path = "/usr/lib/b".into();
    alias.content = None;
    alias.node.source.kind = PayloadNodeKind::Hardlink {
        target: primary.path.clone(),
        identity: prior_identity.into(),
    };
    let prior = captured(vec![
        directory_entry("/usr"),
        directory_entry("/usr/lib"),
        primary,
        alias,
    ]);
    let mut copied = regular_entry("/usr/lib/a", content('c', 7));
    copied.node.source.kind = PayloadNodeKind::Regular {
        hardlink_identity: Some(upper_identity.into()),
    };
    copied
        .node
        .source
        .xattrs
        .insert("user.overlay.origin".into(), b"opaque-handle".to_vec());
    let mut new_alias = copied.clone();
    new_alias.path = "/usr/lib/c".into();
    new_alias.content = None;
    new_alias.node.source.kind = PayloadNodeKind::Hardlink {
        target: copied.path.clone(),
        identity: upper_identity.into(),
    };
    let applied = decode_captured_upper(
        directory_node(),
        vec![copied, new_alias],
        &prior,
        &user_profile(),
    )
    .unwrap()
    .apply(&prior)
    .unwrap();
    for path in ["/usr/lib/b", "/usr/lib/c"] {
        assert!(matches!(
            &entry(&applied, path).unwrap().node.source.kind,
            PayloadNodeKind::Hardlink { target, identity }
                if target == "/usr/lib/a" && identity == prior_identity
        ));
    }
}

#[test]
fn independent_primary_replacement_reanchors_old_alias() {
    let identity = "prior:hardlink:1";
    let mut primary = regular_entry("/usr/lib/a", content('a', 4));
    primary.node.source.kind = PayloadNodeKind::Regular {
        hardlink_identity: Some(identity.into()),
    };
    let mut alias = primary.clone();
    alias.path = "/usr/lib/b".into();
    alias.content = None;
    alias.node.source.kind = PayloadNodeKind::Hardlink {
        target: primary.path.clone(),
        identity: identity.into(),
    };
    let prior = captured(vec![
        directory_entry("/usr"),
        directory_entry("/usr/lib"),
        primary,
        alias,
    ]);
    let replacement = regular_entry("/usr/lib/a", content('c', 9));
    let applied =
        decode_captured_upper(directory_node(), vec![replacement], &prior, &user_profile())
            .unwrap()
            .apply(&prior)
            .unwrap();
    assert_eq!(
        entry(&applied, "/usr/lib/a")
            .and_then(|entry| entry.content.as_ref())
            .map(|content| content.size),
        Some(9)
    );
    assert_eq!(
        entry(&applied, "/usr/lib/b")
            .and_then(|entry| entry.content.as_ref())
            .map(|content| content.size),
        Some(4)
    );
    for path in ["/usr/lib/a", "/usr/lib/b"] {
        assert!(matches!(
            &entry(&applied, path).unwrap().node.source.kind,
            PayloadNodeKind::Regular {
                hardlink_identity: None
            }
        ));
    }
}

#[test]
fn filesystem_decoder_reads_only_upper_content() {
    let temp = tempfile::tempdir().unwrap();
    let upper = temp.path().join("upper");
    std::fs::create_dir_all(upper.join("usr/bin")).unwrap();
    std::fs::set_permissions(&upper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(upper.join("usr/bin/changed"), b"changed only").unwrap();
    let prior = captured(vec![
        directory_entry("/usr"),
        directory_entry("/usr/bin"),
        regular_entry("/usr/bin/unchanged", content('a', 1_000_000)),
    ]);
    let cas = CasStore::new(temp.path().join("objects")).unwrap();
    let delta = decode_selected_root_overlay_upper(&upper, &prior, &cas, &user_profile()).unwrap();
    assert_eq!(delta.upserts.len(), 3);
    assert!(delta.upserts.iter().any(|entry| {
        entry.path == "/usr/bin/changed"
            && entry
                .content
                .as_ref()
                .is_some_and(|content| content.size == 12)
    }));
    assert!(entry(&delta.apply(&prior).unwrap(), "/usr/bin/unchanged").is_some());
}

/// A hand-built upper whose shared whiteout inode is hard-linked across two
/// publication domains, matching the OverlayFS shared-whiteout encoding. This
/// is the unprivileged regression proof for the real mount tests below.
#[test]
fn upper_scan_excludes_shared_xattr_whiteouts_from_hardlink_grouping() {
    let temp = tempfile::tempdir().unwrap();
    let upper = temp.path().join("upper");
    std::fs::create_dir_all(upper.join("usr/bin")).unwrap();
    std::fs::create_dir_all(upper.join("var/lib")).unwrap();
    let whiteout = upper.join("usr/bin/removed");
    std::fs::write(&whiteout, b"").unwrap();
    xattr::set(&whiteout, "user.overlay.whiteout", b"").unwrap();
    std::fs::hard_link(&whiteout, upper.join("var/lib/removed")).unwrap();
    let cas = CasStore::new(temp.path().join("objects")).unwrap();
    let profile = user_profile();

    let (root, entries) =
        scan_selected_root_overlay_upper(&upper, &cas, "overlay-whiteout-test", &profile).unwrap();
    let decoded = decode_upper_operations(root, entries, &profile, |_| Ok(false)).unwrap();

    assert_eq!(decoded.removals, ["/usr/bin/removed", "/var/lib/removed"]);
}

#[test]
fn overlay_upper_scan_assigns_same_domain_regular_hardlinks() {
    let temp = tempfile::tempdir().unwrap();
    let upper = temp.path().join("upper");
    std::fs::create_dir_all(upper.join("usr/bin")).unwrap();
    std::fs::write(upper.join("usr/bin/primary"), b"shared").unwrap();
    std::fs::hard_link(upper.join("usr/bin/primary"), upper.join("usr/bin/alias")).unwrap();
    let cas = CasStore::new(temp.path().join("objects")).unwrap();
    let profile = user_profile();

    let (_, entries) =
        scan_selected_root_overlay_upper(&upper, &cas, "overlay-hardlink-test", &profile).unwrap();
    let grouped = entries
        .iter()
        .filter(|entry| entry_hardlink_identity(entry).is_some())
        .count();
    assert_eq!(grouped, 2);
    assert_eq!(
        entries
            .iter()
            .filter_map(entry_hardlink_identity)
            .collect::<BTreeSet<_>>()
            .len(),
        1
    );
}

#[test]
fn overlay_upper_scan_refuses_genuine_cross_domain_regular_hardlinks() {
    let temp = tempfile::tempdir().unwrap();
    let upper = temp.path().join("upper");
    std::fs::create_dir_all(upper.join("usr/bin")).unwrap();
    std::fs::create_dir_all(upper.join("var/lib")).unwrap();
    std::fs::write(upper.join("usr/bin/primary"), b"shared").unwrap();
    std::fs::hard_link(upper.join("usr/bin/primary"), upper.join("var/lib/alias")).unwrap();
    let cas = CasStore::new(temp.path().join("objects")).unwrap();
    let profile = user_profile();

    let error = scan_selected_root_overlay_upper(&upper, &cas, "overlay-hardlink-test", &profile)
        .unwrap_err();

    assert!(matches!(error, crate::Error::NotImplemented(_)), "{error}");
}

#[test]
fn mounted_overlay_capture_interprets_cross_domain_shared_whiteouts_as_removals() {
    const NAME: &str = "generation::root_manifest::overlay::tests::mounted_overlay_capture_interprets_cross_domain_shared_whiteouts_as_removals";
    if !run_overlay_mount_test(NAME) {
        return;
    }
    let mut fixture = OverlayMountFixture::new(
        &[
            ("usr/bin/removed", b"usr".as_slice()),
            ("var/lib/removed", b"var".as_slice()),
        ],
        user_profile(),
    );
    std::fs::remove_file(fixture.merged.join("usr/bin/removed")).unwrap();
    std::fs::remove_file(fixture.merged.join("var/lib/removed")).unwrap();
    fixture.freeze();
    fixture.assert_shared_whiteout_inode("usr/bin/removed", "var/lib/removed");

    assert_eq!(fixture.removals(), ["/usr/bin/removed", "/var/lib/removed"]);
}

#[test]
fn mounted_overlay_capture_interprets_same_domain_shared_whiteouts_as_removals() {
    const NAME: &str = "generation::root_manifest::overlay::tests::mounted_overlay_capture_interprets_same_domain_shared_whiteouts_as_removals";
    if !run_overlay_mount_test(NAME) {
        return;
    }
    let mut fixture = OverlayMountFixture::new(
        &[
            ("usr/bin/first", b"first".as_slice()),
            ("usr/lib/second", b"second".as_slice()),
        ],
        user_profile(),
    );
    std::fs::remove_file(fixture.merged.join("usr/bin/first")).unwrap();
    std::fs::remove_file(fixture.merged.join("usr/lib/second")).unwrap();
    fixture.freeze();
    fixture.assert_shared_whiteout_inode("usr/bin/first", "usr/lib/second");

    assert_eq!(fixture.removals(), ["/usr/bin/first", "/usr/lib/second"]);
}

/// One real selected-root OverlayFS mount over a private tmpfs upper.
///
/// Field order is drop order: the overlay is unmounted, then the tmpfs, and
/// only then is the workspace directory removed.
struct OverlayMountFixture {
    mounted: Option<MountedSelectedRootOverlay>,
    _scratch: MountedScratchTmpfs,
    temp: tempfile::TempDir,
    upper: PathBuf,
    merged: PathBuf,
    profile: SelectedRootOverlayProfile,
}

impl OverlayMountFixture {
    fn new(lower_files: &[(&str, &[u8])], profile: SelectedRootOverlayProfile) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let merged = temp.path().join("merged");
        std::fs::create_dir(&lower).unwrap();
        std::fs::create_dir(&merged).unwrap();
        for (path, bytes) in lower_files {
            let path = lower.join(path.trim_start_matches('/'));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        // A container runtime root is normally itself an OverlayFS mount that
        // cannot host an OverlayFS upper. Keep the upper on a private tmpfs the
        // same way the selected-root session does.
        let scratch = MountedScratchTmpfs::mount(&temp.path().join("scratch"))
            .expect("private tmpfs for a selected-root OverlayFS upper");
        let upper = scratch.path().join("upper");
        let work = scratch.path().join("work");
        std::fs::create_dir(&upper).unwrap();
        std::fs::create_dir(&work).unwrap();
        let mounted = MountedSelectedRootOverlay::mount(&lower, &upper, &work, &merged, &profile)
            .expect("real selected-root OverlayFS mount");
        Self {
            mounted: Some(mounted),
            _scratch: scratch,
            temp,
            upper,
            merged,
            profile,
        }
    }

    fn freeze(&mut self) {
        self.mounted
            .take()
            .expect("fixture is mounted")
            .freeze(&self.upper)
            .expect("freeze selected-root OverlayFS upper");
    }

    /// Prove the kernel used the shared-whiteout inode the reproducer depends
    /// on, for either the char-device or the xattr whiteout encoding.
    fn assert_shared_whiteout_inode(&self, left: &str, right: &str) {
        let left = std::fs::symlink_metadata(self.upper.join(left)).unwrap();
        let right = std::fs::symlink_metadata(self.upper.join(right)).unwrap();
        assert_eq!(
            (left.dev(), left.ino()),
            (right.dev(), right.ino()),
            "OverlayFS did not hard-link one shared whiteout inode"
        );
    }

    fn removals(&self) -> Vec<String> {
        let cas = CasStore::new(self.temp.path().join("objects")).unwrap();
        let (root, entries) =
            scan_selected_root_overlay_upper(&self.upper, &cas, "upper-v1", &self.profile).unwrap();
        decode_upper_operations(root, entries, &self.profile, |_| Ok(false))
            .unwrap()
            .removals
    }
}

/// Namespace gate for real OverlayFS mount proofs.
///
/// Mirrors `scriptlet::test_support::run_selected_root_namespace_test`: when
/// already root inside a usable mount namespace the body runs, otherwise the
/// current test binary is re-executed under a private user and mount
/// namespace. The parent returns `false`, so the test is a no-op there.
fn run_overlay_mount_test(test_name: &str) -> bool {
    const CHILD_MARKER: &str = "CONARY_OVERLAY_MOUNT_TEST_MARKER";

    if nix::unistd::geteuid().is_root() && mount_namespace_available() {
        if let Some(marker) = std::env::var_os(CHILD_MARKER) {
            std::fs::write(marker, test_name).expect("record overlay mount test execution");
        }
        return true;
    }
    assert!(
        std::env::var_os(CHILD_MARKER).is_none(),
        "overlay mount test child lacks a usable mount namespace"
    );

    let namespace_args = [
        "--user",
        "--map-root-user",
        "--mount",
        "--propagation",
        "private",
    ];
    let probe = Command::new("unshare")
        .args(namespace_args)
        .arg("/bin/true")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !probe.is_ok_and(|status| status.success()) {
        return false;
    }

    let marker = tempfile::NamedTempFile::new().expect("overlay mount test execution marker");
    let status = Command::new("unshare")
        .args(namespace_args)
        .arg(std::env::current_exe().expect("current test executable"))
        .arg(test_name)
        .args(["--exact", "--nocapture"])
        .env(CHILD_MARKER, marker.path())
        .status()
        .expect("run overlay mount proof in a user and mount namespace");
    assert!(
        status.success(),
        "overlay mount namespace child failed for {test_name}"
    );
    assert_eq!(
        std::fs::read_to_string(marker.path()).expect("read mount test execution marker"),
        test_name,
        "overlay mount namespace child matched no exact test"
    );
    false
}

fn mount_namespace_available() -> bool {
    let mut command = Command::new("/bin/true");
    // Safety: this child-only probe performs no work after unsharing.
    unsafe {
        command.pre_exec(|| {
            nix::sched::unshare(nix::sched::CloneFlags::CLONE_NEWNS).map_err(std::io::Error::from)
        });
    }
    command.status().is_ok()
}

fn user_profile() -> SelectedRootOverlayProfile {
    SelectedRootOverlayProfile {
        xattr_namespace: OverlayXattrNamespace::User,
        ..SelectedRootOverlayProfile::trusted()
    }
}

fn captured(entries: Vec<GenerationRootEntry>) -> CapturedSelectedRoot {
    let (generation, state): (Vec<_>, Vec<_>) = entries.into_iter().partition(|entry| {
        super::super::classify_root_path(&entry.path).unwrap()
            == super::super::RootPathDomain::Immutable
    });
    CapturedSelectedRoot {
        generation: super::super::GenerationRootManifest {
            version: super::super::GENERATION_ROOT_MANIFEST_VERSION,
            root: directory_node(),
            entries: sorted(generation),
        },
        state: super::super::MutableStateManifest {
            version: super::super::GENERATION_ROOT_MANIFEST_VERSION,
            entries: sorted(state),
        },
    }
}

fn sorted(mut entries: Vec<GenerationRootEntry>) -> Vec<GenerationRootEntry> {
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries
}

fn entry<'a>(captured: &'a CapturedSelectedRoot, path: &str) -> Option<&'a GenerationRootEntry> {
    captured
        .generation
        .entries
        .iter()
        .chain(&captured.state.entries)
        .find(|entry| entry.path == path)
}

fn directory_entry(path: &str) -> GenerationRootEntry {
    GenerationRootEntry {
        path: path.into(),
        node: directory_node(),
        content: None,
    }
}

fn regular_entry(path: &str, content: PayloadContentAuthority) -> GenerationRootEntry {
    GenerationRootEntry {
        path: path.into(),
        node: ResolvedPayloadNode::from_numeric_source(PayloadNode::regular(0o644)).unwrap(),
        content: Some(content),
    }
}

fn directory_node() -> ResolvedPayloadNode {
    let mut node = PayloadNode::regular(0o755);
    node.kind = PayloadNodeKind::Directory;
    node.mode = libc::S_IFDIR | 0o755;
    node.user = PayloadIdentity::Numeric { id: 0 };
    node.group = PayloadIdentity::Numeric { id: 0 };
    ResolvedPayloadNode::from_numeric_source(node).unwrap()
}

fn content(byte: char, size: u64) -> PayloadContentAuthority {
    PayloadContentAuthority {
        sha256: byte.to_string().repeat(64),
        size,
    }
}
