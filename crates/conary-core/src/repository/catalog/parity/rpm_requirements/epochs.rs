// crates/conary-core/src/repository/catalog/parity/rpm_requirements/epochs.rs

//! Native EVR spelling for every operand and its derived atom index.

use crate::error::{Error, Result};
use crate::repository::catalog::CatalogRequirementAtomV1;
use crate::repository::dependency_model::RepositoryRequirementExpression as Expression;
use crate::repository::rpm_dependency::canonicalize_native_rpm_evr;
use crate::repository::versioning::{RepoVersionConstraint, VersionScheme, parse_repo_constraint};

pub(super) fn project(
    expression: &mut Expression,
    atoms: &mut [CatalogRequirementAtomV1],
) -> Result<bool> {
    let mut changed = project_expression(expression)?;
    for atom in atoms {
        changed |= project_constraint(&mut atom.version_constraint)?;
    }
    Ok(changed)
}

fn project_expression(expression: &mut Expression) -> Result<bool> {
    match expression {
        Expression::Atom(clause) => project_constraint(&mut clause.version_constraint),
        Expression::And(operands) | Expression::Or(operands) => {
            let mut changed = false;
            for operand in operands {
                changed |= project_expression(operand)?;
            }
            Ok(changed)
        }
        Expression::If {
            requirement,
            condition,
            otherwise,
        }
        | Expression::Unless {
            requirement,
            condition,
            otherwise,
        } => {
            let mut changed = project_expression(requirement)?;
            changed |= project_expression(condition)?;
            if let Some(otherwise) = otherwise {
                changed |= project_expression(otherwise)?;
            }
            Ok(changed)
        }
        Expression::With { left, right } | Expression::Without { left, right } => {
            let mut changed = project_expression(left)?;
            changed |= project_expression(right)?;
            Ok(changed)
        }
    }
}

fn project_constraint(constraint: &mut Option<String>) -> Result<bool> {
    let Some(source) = constraint else {
        return Ok(false);
    };
    let parsed = parse_repo_constraint(VersionScheme::Rpm, source)
        .map_err(|error| Error::ParseError(error.to_string()))?;
    let (operator, version) = match parsed {
        RepoVersionConstraint::Exact(version) => ("=", version),
        RepoVersionConstraint::GreaterThan(version) => (">", version),
        RepoVersionConstraint::GreaterOrEqual(version) => (">=", version),
        RepoVersionConstraint::LessThan(version) => ("<", version),
        RepoVersionConstraint::LessOrEqual(version) => ("<=", version),
        _ => {
            return Err(Error::ParseError(
                "non-RPM comparison in RPM requirement".into(),
            ));
        }
    };
    let native = canonicalize_native_rpm_evr(&version).map_err(Error::ParseError)?;
    if native == version {
        return Ok(false);
    }
    *source = format!("{operator} {native}");
    Ok(true)
}
