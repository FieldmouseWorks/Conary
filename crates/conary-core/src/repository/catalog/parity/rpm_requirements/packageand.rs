// crates/conary-core/src/repository/catalog/parity/rpm_requirements/packageand.rs

//! libsolv 0.7.36 `repo_fix_supplements`' packageand dependency grammar.
//!
//! Source: https://github.com/openSUSE/libsolv/blob/0.7.36/src/suse.c#L177-L230

use crate::error::{Error, Result};
use crate::repository::catalog::CatalogRequirementAtomV1;
use crate::repository::dependency_model::{
    RepositoryRequirementClause, RepositoryRequirementExpression as Expression,
    RepositoryRequirementKind,
};

pub(super) fn project(
    kind: RepositoryRequirementKind,
    expression: &mut Expression,
    atoms: &mut Vec<CatalogRequirementAtomV1>,
) -> Result<bool> {
    if kind != RepositoryRequirementKind::Supplements {
        return Ok(false);
    }
    let Expression::Atom(clause) = expression else {
        return Ok(false);
    };
    // libsolv rewrites only an unversioned atom that fits its 1024-byte buffer.
    if clause.version_constraint.is_some() || clause.name.len() >= 1024 {
        return Ok(false);
    }
    let Some(body) = clause
        .name
        .strip_prefix("packageand(")
        .and_then(|name| name.strip_suffix(')'))
    else {
        return Ok(false);
    };
    let mut names = Vec::new();
    let mut fields = body.split(':').peekable();
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        // In this grammar `pattern:` qualifies the next field; other colons
        // separate package names. An empty qualified field is still retained.
        let name = if field == "pattern" && fields.peek().is_some() {
            format!(
                "pattern:{}",
                fields.next().expect("peeked packageand field")
            )
        } else {
            field.to_string()
        };
        names.push(name);
    }
    if names.is_empty() {
        return Ok(false);
    }
    let [source_atom] = atoms.as_slice() else {
        return Err(Error::ConflictError(
            "packageand source atom index is not singular".into(),
        ));
    };
    if source_atom.capability != clause.name || source_atom.version_constraint.is_some() {
        return Err(Error::ConflictError(
            "packageand source atom index disagrees with its expression".into(),
        ));
    }
    let projected_atoms = names
        .iter()
        .map(|name| {
            let mut atom = source_atom.clone();
            atom.capability.clone_from(name);
            atom
        })
        .collect();
    // libsolv builds a left-associated REL_AND tree, which the RPM producer
    // decodes as the ordered And operand vector used by the canonical grammar.
    let mut operands = names
        .into_iter()
        .map(|name| Expression::Atom(RepositoryRequirementClause::name_only(name)))
        .collect::<Vec<_>>();
    *expression = if operands.len() == 1 {
        operands.remove(0)
    } else {
        Expression::And(operands)
    };
    *atoms = projected_atoms;
    Ok(true)
}
