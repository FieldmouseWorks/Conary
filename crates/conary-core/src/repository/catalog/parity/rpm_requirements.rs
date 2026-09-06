// crates/conary-core/src/repository/catalog/parity/rpm_requirements.rs

//! RPM's native solver projection, leaving authenticated source records intact.

use std::collections::BTreeSet;

use crate::error::{Error, Result};
use crate::repository::catalog::CatalogRequirementGroupV1;
use crate::repository::dependency_model::{
    RepositoryRequirementExpression, RepositoryRequirementKind,
};
use crate::repository::rpm_dependency::{
    canonical_rpm_dependency_text, parse_source_rpm_dependency,
};
use crate::repository::versioning::VersionScheme;

mod epochs;
mod packageand;

pub(super) fn native_requirement_groups(
    scheme: VersionScheme,
    mut groups: Vec<CatalogRequirementGroupV1>,
) -> Result<Vec<CatalogRequirementGroupV1>> {
    if scheme != VersionScheme::Rpm {
        return Ok(groups);
    }
    for group in &mut groups {
        if let Some(native_text) = &group.native_text {
            let kind = RepositoryRequirementKind::from_str_exact(&group.kind)
                .ok_or_else(|| Error::ParseError("unknown RPM requirement kind".into()))?;
            let mut expression: RepositoryRequirementExpression =
                serde_json::from_str(&group.expression_json).map_err(|error| {
                    Error::ParseError(format!("decode RPM requirement expression: {error}"))
                })?;
            let parsed =
                parse_source_rpm_dependency(kind, native_text).map_err(Error::ParseError)?;
            if parsed != expression {
                return Err(Error::ConflictError(
                    "RPM requirement native text disagrees with its typed expression".into(),
                ));
            }
            let mut changed = packageand::project(kind, &mut expression, &mut group.atoms)?;
            changed |= epochs::project(&mut expression, &mut group.atoms)?;
            if changed {
                group.expression_json = serde_json::to_string(&expression)
                    .map_err(|error| Error::ParseError(error.to_string()))?;
                group.canonicalize()?;
            }
            group.native_text = Some(canonical_rpm_dependency_text(&expression));
        }
    }

    // libsolv 0.7.36 ext/repo_rpmmd.c:adddep and src/repo.c:repo_addid_dep
    // unify an exact dependency ID on the prerequisite side of the marker,
    // including when its ordinary declaration appears later in primary.xml.
    // Require equality of every other fact before dropping the ordinary copy.
    let prerequisites = groups
        .iter()
        .filter(|group| {
            RepositoryRequirementKind::from_str_exact(&group.kind)
                == Some(RepositoryRequirementKind::PreDepends)
        })
        .map(|group| {
            let mut ordinary = group.clone();
            ordinary.kind = RepositoryRequirementKind::Depends.as_str().into();
            key(&ordinary)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let mut projected = Vec::with_capacity(groups.len());
    for group in groups {
        let bytes = key(&group)?;
        if RepositoryRequirementKind::from_str_exact(&group.kind)
            == Some(RepositoryRequirementKind::Depends)
            && prerequisites.contains(&bytes)
        {
            continue;
        }
        projected.push((bytes, group));
    }
    // Canonical spelling can change the canonical row ordering.
    projected.sort_by(|left, right| left.0.cmp(&right.0));
    projected.dedup_by(|left, right| left.0 == right.0);
    Ok(projected.into_iter().map(|(_, group)| group).collect())
}

fn key(group: &CatalogRequirementGroupV1) -> Result<Vec<u8>> {
    crate::json::canonical_json(group)
        .map_err(|error| Error::ParseError(format!("encode RPM requirement projection: {error}")))
}
