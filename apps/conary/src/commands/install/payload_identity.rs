// apps/conary/src/commands/install/payload_identity.rs

//! Exact source-identity resolution against the selected target root.
//!
//! Native package formats may name payload owners instead of carrying numeric
//! IDs.  Those names are meaningful only in the target root after typed
//! pre-payload lifecycle events (notably sysusers) have run.  This module reads
//! only that root's account databases and rejects missing or ambiguous
//! authority.
//!
//! [`PlanIdentityMode::EventProjection`] is the one exception: the event-time
//! projection may resolve a name the root does not define yet to a typed
//! pending owner, because a pre-payload lifecycle event is expected to define
//! it before execution. [`PlanIdentityMode::Authoritative`] keeps the refusal.

use super::{InstallSemantics, PackageFormatType, PreparedSourceKind};
use anyhow::{Context, Result, bail};
use conary_core::payload::{PayloadIdentity, PayloadNode, ResolvedPayloadNode};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_IDENTITY_DATABASE_SIZE: u64 = 16 * 1024 * 1024;

/// Which owner-resolution contract a payload plan is built under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanIdentityMode {
    /// Every named owner must already be defined by the selected root. The
    /// resulting [`ResolvedPayloadNode`]s carry authoritative numeric
    /// ownership and may reach apply and mutation boundaries.
    Authoritative,
    /// A named owner a pre-payload lifecycle event may define is allowed to be
    /// pending. This is the event-time projection only: the resulting
    /// [`ProjectedPayloadNode`]s have no authoritative numeric ID and must
    /// never reach apply or mutation.
    EventProjection,
}

/// Which account database an identity name is resolved against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum IdentityKind {
    User,
    Group,
}

impl IdentityKind {
    fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Group => "group",
        }
    }
}

/// One side of a payload node's projected ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProjectedOwner {
    /// The selected root defines the name, or the node carries a numeric ID.
    Resolved(u64),
    /// The selected root does not define `name` yet; a pre-payload lifecycle
    /// event must define it before execution. No numeric ID is invented.
    Pending { name: String },
}

/// A payload node resolved under [`PlanIdentityMode::EventProjection`].
///
/// This is deliberately distinct from [`ResolvedPayloadNode`]: a projected
/// node may carry a pending owner, so it must never be accepted by an apply or
/// mutation boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectedPayloadNode {
    pub source: PayloadNode,
    pub user: ProjectedOwner,
    pub group: ProjectedOwner,
}

impl ProjectedPayloadNode {
    /// The authoritative resolution, or `None` when either owner is pending.
    pub(super) fn resolved(&self) -> Option<ResolvedPayloadNode> {
        let (ProjectedOwner::Resolved(uid), ProjectedOwner::Resolved(gid)) =
            (&self.user, &self.group)
        else {
            return None;
        };
        Some(ResolvedPayloadNode {
            source: self.source.clone(),
            uid: *uid,
            gid: *gid,
        })
    }

    /// Every owner this node leaves for a pre-payload lifecycle event to
    /// define, in a deterministic order.
    pub(super) fn pending_owners(&self) -> impl Iterator<Item = (IdentityKind, &str)> {
        [
            (IdentityKind::User, &self.user),
            (IdentityKind::Group, &self.group),
        ]
        .into_iter()
        .filter_map(|(kind, owner)| match owner {
            ProjectedOwner::Pending { name } => Some((kind, name.as_str())),
            ProjectedOwner::Resolved(_) => None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PayloadIdentityError {
    #[error(
        "target root {} does not define payload {} name(s): {}",
        root.display(),
        kind.label(),
        names
    )]
    MissingNames {
        root: PathBuf,
        kind: IdentityKind,
        names: String,
    },
}

pub(crate) fn resolve_native_payload_nodes(
    root: &Path,
    nodes: impl IntoIterator<Item = PayloadNode>,
    format: PackageFormatType,
) -> Result<Vec<ResolvedPayloadNode>> {
    resolve_payload_nodes(root, nodes, InstallSemantics::native_package(format))
}

pub(super) fn resolve_payload_nodes(
    root: &Path,
    nodes: impl IntoIterator<Item = PayloadNode>,
    semantics: InstallSemantics,
) -> Result<Vec<ResolvedPayloadNode>> {
    let nodes = nodes.into_iter().collect::<Vec<_>>();
    let named_users = named_identities(nodes.iter().map(|node| &node.user), semantics);
    let named_groups = named_identities(nodes.iter().map(|node| &node.group), semantics);
    let (users, groups) = load_identity_databases(
        root,
        !named_users.is_empty(),
        !named_groups.is_empty(),
        PlanIdentityMode::Authoritative,
    )?;

    require_all_names(&named_users, &users, IdentityKind::User, root)?;
    require_all_names(&named_groups, &groups, IdentityKind::Group, root)?;

    nodes
        .into_iter()
        .map(|source| {
            source.validate()?;
            let uid = resolve_identity(&source.user, &users, "user", semantics)?;
            let gid = resolve_identity(&source.group, &groups, "group", semantics)?;
            let resolved = ResolvedPayloadNode { source, uid, gid };
            resolved.validate()?;
            Ok(resolved)
        })
        .collect()
}

/// Resolve payload nodes for the event-time projection.
///
/// Unlike [`resolve_payload_nodes`], a named owner the selected root does not
/// define is not an error: it becomes a typed [`ProjectedOwner::Pending`] for a
/// pre-payload lifecycle event to define before execution. A name the root does
/// define resolves to the same numeric ID in both modes.
pub(super) fn project_payload_nodes(
    root: &Path,
    nodes: impl IntoIterator<Item = PayloadNode>,
    semantics: InstallSemantics,
) -> Result<Vec<ProjectedPayloadNode>> {
    let nodes = nodes.into_iter().collect::<Vec<_>>();
    let named_users = named_identities(nodes.iter().map(|node| &node.user), semantics);
    let named_groups = named_identities(nodes.iter().map(|node| &node.group), semantics);
    let (users, groups) = load_identity_databases(
        root,
        !named_users.is_empty(),
        !named_groups.is_empty(),
        PlanIdentityMode::EventProjection,
    )?;

    nodes
        .into_iter()
        .map(|source| {
            source.validate()?;
            let user = projected_identity(&source.user, &users, semantics);
            let group = projected_identity(&source.group, &groups, semantics);
            Ok(ProjectedPayloadNode {
                source,
                user,
                group,
            })
        })
        .collect()
}

fn load_identity_databases(
    root: &Path,
    needs_users: bool,
    needs_groups: bool,
    mode: PlanIdentityMode,
) -> Result<(BTreeMap<String, u64>, BTreeMap<String, u64>)> {
    let users = if needs_users {
        parse_identity_database(root, IdentityDatabaseKind::Passwd, mode)?
    } else {
        BTreeMap::new()
    };
    let groups = if needs_groups {
        parse_identity_database(root, IdentityDatabaseKind::Group, mode)?
    } else {
        BTreeMap::new()
    };
    Ok((users, groups))
}

fn projected_identity(
    identity: &PayloadIdentity,
    names: &BTreeMap<String, u64>,
    semantics: InstallSemantics,
) -> ProjectedOwner {
    if let Some(id) = source_defined_numeric_identity(identity, semantics) {
        return ProjectedOwner::Resolved(id);
    }
    match identity {
        PayloadIdentity::Numeric { id } => ProjectedOwner::Resolved(*id),
        PayloadIdentity::Named { name } => names.get(name).copied().map_or_else(
            || ProjectedOwner::Pending { name: name.clone() },
            ProjectedOwner::Resolved,
        ),
    }
}

fn named_identities<'a>(
    identities: impl IntoIterator<Item = &'a PayloadIdentity>,
    semantics: InstallSemantics,
) -> BTreeSet<&'a str> {
    identities
        .into_iter()
        .filter_map(|identity| match identity {
            PayloadIdentity::Named { name }
                if source_defined_numeric_identity(identity, semantics).is_none() =>
            {
                Some(name.as_str())
            }
            PayloadIdentity::Numeric { .. } => None,
            PayloadIdentity::Named { .. } => None,
        })
        .collect()
}

/// RPM's pinned source ABI resolves its distinguished UID-0 user and GID-0
/// group without consulting account databases. Converted RPM CCS packages
/// retain `NativePackage { Rpm }` semantics, so this rule survives transport.
fn source_defined_numeric_identity(
    identity: &PayloadIdentity,
    semantics: InstallSemantics,
) -> Option<u64> {
    let is_rpm = matches!(
        semantics.source,
        PreparedSourceKind::NativePackage {
            format: PackageFormatType::Rpm
        }
    );
    match identity {
        PayloadIdentity::Named { name } if is_rpm && name == "root" => Some(0),
        _ => None,
    }
}

fn require_all_names(
    required: &BTreeSet<&str>,
    resolved: &BTreeMap<String, u64>,
    kind: IdentityKind,
    root: &Path,
) -> std::result::Result<(), PayloadIdentityError> {
    let missing = required
        .iter()
        .copied()
        .filter(|name| !resolved.contains_key(*name))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(PayloadIdentityError::MissingNames {
            root: root.to_path_buf(),
            kind,
            names: missing.join(", "),
        });
    }
    Ok(())
}

fn resolve_identity(
    identity: &PayloadIdentity,
    names: &BTreeMap<String, u64>,
    kind: &str,
    semantics: InstallSemantics,
) -> Result<u64> {
    if let Some(id) = source_defined_numeric_identity(identity, semantics) {
        return Ok(id);
    }
    match identity {
        PayloadIdentity::Numeric { id } => Ok(*id),
        PayloadIdentity::Named { name } => names
            .get(name)
            .copied()
            .with_context(|| format!("payload {kind} name {name:?} was not resolved")),
    }
}

#[derive(Debug, Clone, Copy)]
enum IdentityDatabaseKind {
    Passwd,
    Group,
}

impl IdentityDatabaseKind {
    fn relative_path(self) -> &'static str {
        match self {
            Self::Passwd => "etc/passwd",
            Self::Group => "etc/group",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Passwd => "passwd",
            Self::Group => "group",
        }
    }

    fn expected_fields(self) -> usize {
        match self {
            Self::Passwd => 7,
            Self::Group => 4,
        }
    }

    fn id_field(self) -> usize {
        match self {
            Self::Passwd => 2,
            Self::Group => 2,
        }
    }
}

fn parse_identity_database(
    root: &Path,
    kind: IdentityDatabaseKind,
    mode: PlanIdentityMode,
) -> Result<BTreeMap<String, u64>> {
    let Some(path) = safe_identity_database_path(root, kind, mode)? else {
        return Ok(BTreeMap::new());
    };
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("target-root {} database is missing", kind.label()))?;
    if !metadata.file_type().is_file() {
        bail!(
            "target-root {} database is not a regular file: {}",
            kind.label(),
            path.display()
        );
    }
    if metadata.len() > MAX_IDENTITY_DATABASE_SIZE {
        bail!(
            "target-root {} database exceeds {} bytes: {}",
            kind.label(),
            MAX_IDENTITY_DATABASE_SIZE,
            path.display()
        );
    }
    let bytes =
        fs::read(&path).with_context(|| format!("failed to read target-root {}", kind.label()))?;
    let text = std::str::from_utf8(&bytes)
        .with_context(|| format!("target-root {} database is not UTF-8", kind.label()))?;
    parse_identity_records(text, kind)
        .map_err(|error| anyhow::anyhow!("invalid target-root {} database: {error}", kind.label()))
}

/// Locate one account database, returning `None` when the selected root has
/// not materialized it yet.
///
/// Only [`PlanIdentityMode::EventProjection`] tolerates absence: a missing
/// `/etc`, `passwd`, or `group` means every named owner is pending. In
/// [`PlanIdentityMode::Authoritative`] absence is a typed refusal, and a
/// malformed or symlinked database is a refusal in both modes.
fn safe_identity_database_path(
    root: &Path,
    kind: IdentityDatabaseKind,
    mode: PlanIdentityMode,
) -> Result<Option<PathBuf>> {
    let root_metadata = fs::symlink_metadata(root)
        .with_context(|| format!("target root does not exist: {}", root.display()))?;
    if !root_metadata.file_type().is_dir() {
        bail!("target root is not a directory: {}", root.display());
    }

    let etc = root.join("etc");
    match fs::symlink_metadata(&etc) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => bail!(
            "target-root /etc is not a real directory: {}",
            etc.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if mode == PlanIdentityMode::EventProjection {
                return Ok(None);
            }
            bail!("target root has no /etc directory: {}", root.display());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect target-root /etc: {}", etc.display()));
        }
    }

    let path = root.join(kind.relative_path());
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "target-root {} database must not be a symlink: {}",
                kind.label(),
                path.display()
            );
        }
        Ok(_) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if mode == PlanIdentityMode::EventProjection {
                return Ok(None);
            }
            bail!(
                "target-root {} database is missing: {}",
                kind.label(),
                path.display()
            );
        }
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect target-root {}", kind.label()))
        }
    }
}

fn parse_identity_records(text: &str, kind: IdentityDatabaseKind) -> Result<BTreeMap<String, u64>> {
    let mut identities = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('+') || line.starts_with('-') {
            bail!(
                "{} line {} uses an unsupported external identity directive",
                kind.label(),
                line_number
            );
        }
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != kind.expected_fields() {
            bail!(
                "{} line {} has {} fields, expected {}",
                kind.label(),
                line_number,
                fields.len(),
                kind.expected_fields()
            );
        }
        let name = fields[0];
        if name.is_empty() {
            bail!("{} line {} has an empty name", kind.label(), line_number);
        }
        let id_text = fields[kind.id_field()];
        if id_text.is_empty() || !id_text.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!(
                "{} line {} has a non-decimal numeric ID",
                kind.label(),
                line_number
            );
        }
        let id = id_text.parse::<u32>().with_context(|| {
            format!(
                "{} line {} numeric ID is outside the target platform range",
                kind.label(),
                line_number
            )
        })?;
        if identities.insert(name.to_string(), u64::from(id)).is_some() {
            bail!("{} name {name:?} is defined more than once", kind.label());
        }
    }
    Ok(identities)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::payload::{PayloadIdentity, PayloadNode};
    use conary_core::repository::versioning::VersionScheme;

    fn named_node(user: &str, group: &str) -> PayloadNode {
        let mut node = PayloadNode::regular(0o755);
        node.user = PayloadIdentity::Named {
            name: user.to_string(),
        };
        node.group = PayloadIdentity::Named {
            name: group.to_string(),
        };
        node
    }

    fn rpm_semantics() -> InstallSemantics {
        InstallSemantics::native_package(PackageFormatType::Rpm)
    }

    fn conary_semantics() -> InstallSemantics {
        InstallSemantics::ccs(VersionScheme::Conary)
    }

    #[test]
    fn resolves_names_only_from_selected_target_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("etc")).unwrap();
        fs::write(
            root.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\nsvc:x:417:819:svc:/:/sbin/nologin\n",
        )
        .unwrap();
        fs::write(
            root.path().join("etc/group"),
            "root:x:0:\nsvc-group:x:819:\n",
        )
        .unwrap();

        let resolved = resolve_payload_nodes(
            root.path(),
            [named_node("svc", "svc-group")],
            rpm_semantics(),
        )
        .unwrap();

        assert_eq!(resolved[0].uid, 417);
        assert_eq!(resolved[0].gid, 819);
        assert_eq!(
            resolved[0].source.user,
            PayloadIdentity::Named {
                name: "svc".to_string()
            }
        );
    }

    #[test]
    fn numeric_payload_does_not_require_account_databases() {
        let root = tempfile::tempdir().unwrap();
        let mut node = PayloadNode::regular(0o644);
        node.user = PayloadIdentity::Numeric { id: 123 };
        node.group = PayloadIdentity::Numeric { id: 456 };

        let resolved = resolve_payload_nodes(root.path(), [node], conary_semantics()).unwrap();

        assert_eq!(resolved[0].uid, 123);
        assert_eq!(resolved[0].gid, 456);
    }

    #[test]
    fn missing_name_rejects_the_full_payload() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("etc")).unwrap();
        fs::write(
            root.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\n",
        )
        .unwrap();
        fs::write(
            root.path().join("etc/group"),
            "root:x:0:\nsvc-group:x:819:\n",
        )
        .unwrap();

        let error = resolve_payload_nodes(
            root.path(),
            [named_node("missing", "svc-group")],
            rpm_semantics(),
        )
        .unwrap_err();

        match error.downcast_ref::<PayloadIdentityError>() {
            Some(PayloadIdentityError::MissingNames { kind, names, .. }) => {
                assert_eq!(*kind, IdentityKind::User);
                assert_eq!(names, "missing");
            }
            other => panic!("expected a typed missing-name refusal, got {other:?}"),
        }
    }

    #[test]
    fn projection_defers_names_the_root_does_not_define_and_resolves_known_ones() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("etc")).unwrap();
        fs::write(
            root.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\nsvc:x:417:819:svc:/:/sbin/nologin\n",
        )
        .unwrap();
        fs::write(
            root.path().join("etc/group"),
            "root:x:0:\nsvc-group:x:819:\n",
        )
        .unwrap();

        let projected = project_payload_nodes(
            root.path(),
            [
                named_node("svc", "svc-group"),
                named_node("late-user", "late-group"),
            ],
            rpm_semantics(),
        )
        .unwrap();

        // A name the root defines resolves normally in projection mode.
        assert_eq!(projected[0].user, ProjectedOwner::Resolved(417));
        assert_eq!(projected[0].group, ProjectedOwner::Resolved(819));
        assert_eq!(projected[0].resolved().unwrap().uid, 417);
        // A name it does not define becomes a typed pending owner, and the
        // node keeps no invented numeric ID.
        assert_eq!(
            projected[1].user,
            ProjectedOwner::Pending {
                name: "late-user".to_string()
            }
        );
        assert_eq!(
            projected[1].group,
            ProjectedOwner::Pending {
                name: "late-group".to_string()
            }
        );
        assert!(projected[1].resolved().is_none());
        assert_eq!(
            projected[1].pending_owners().collect::<Vec<_>>(),
            vec![
                (IdentityKind::User, "late-user"),
                (IdentityKind::Group, "late-group"),
            ]
        );
    }

    #[test]
    fn projection_tolerates_an_absent_account_database() {
        let root = tempfile::tempdir().unwrap();

        let projected =
            project_payload_nodes(root.path(), [named_node("late", "late")], rpm_semantics())
                .unwrap();

        assert_eq!(
            projected[0].user,
            ProjectedOwner::Pending {
                name: "late".to_string()
            }
        );
        assert_eq!(
            projected[0].group,
            ProjectedOwner::Pending {
                name: "late".to_string()
            }
        );
    }

    #[test]
    fn duplicate_name_is_ambiguous_and_rejected() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("etc")).unwrap();
        fs::write(
            root.path().join("etc/passwd"),
            "svc:x:417:819:svc:/:/sbin/nologin\nsvc:x:418:819:svc:/:/sbin/nologin\n",
        )
        .unwrap();
        fs::write(root.path().join("etc/group"), "svc-group:x:819:\n").unwrap();

        let error = resolve_payload_nodes(
            root.path(),
            [named_node("svc", "svc-group")],
            rpm_semantics(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("defined more than once"));
    }

    #[test]
    fn symlinked_identity_database_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("etc")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.path().join("etc/passwd")).unwrap();
        fs::write(root.path().join("etc/group"), "svc-group:x:819:\n").unwrap();

        let error = resolve_payload_nodes(
            root.path(),
            [named_node("svc", "svc-group")],
            rpm_semantics(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("must not be a symlink"));
    }

    #[test]
    fn rpm_distinguished_root_resolves_without_account_databases() {
        let root = tempfile::tempdir().unwrap();

        let resolved =
            resolve_payload_nodes(root.path(), [named_node("root", "root")], rpm_semantics())
                .unwrap();

        assert_eq!(resolved[0].uid, 0);
        assert_eq!(resolved[0].gid, 0);
        assert_eq!(
            resolved[0].source.user,
            PayloadIdentity::Named {
                name: "root".to_string()
            }
        );
    }

    #[test]
    fn rpm_non_root_name_still_requires_target_account_database() {
        let root = tempfile::tempdir().unwrap();

        let error = resolve_payload_nodes(
            root.path(),
            [named_node("service", "root")],
            rpm_semantics(),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("target root has no /etc directory")
        );
    }

    #[test]
    fn non_rpm_named_root_does_not_inherit_rpm_uid_zero_semantics() {
        let root = tempfile::tempdir().unwrap();

        let error = resolve_payload_nodes(
            root.path(),
            [named_node("root", "root")],
            conary_semantics(),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("target root has no /etc directory")
        );
    }
}
