// crates/conary-core/src/repository/rpm_dependency/render.rs

//! Lossless canonical RPM dependency spelling shared by native parity projections.

use crate::repository::dependency_model::RepositoryRequirementExpression;

pub(crate) fn canonical_rpm_dependency_text(
    expression: &RepositoryRequirementExpression,
) -> String {
    match expression {
        RepositoryRequirementExpression::Atom(clause) => {
            clause.version_constraint.as_ref().map_or_else(
                || clause.name.clone(),
                |constraint| format!("{} {constraint}", clause.name),
            )
        }
        RepositoryRequirementExpression::And(operands) => {
            canonical_rpm_boolean_text(operands, "and")
        }
        RepositoryRequirementExpression::Or(operands) => canonical_rpm_boolean_text(operands, "or"),
        RepositoryRequirementExpression::If {
            requirement,
            condition,
            otherwise,
        } => canonical_rpm_conditional_text(requirement, condition, otherwise.as_deref(), "if"),
        RepositoryRequirementExpression::Unless {
            requirement,
            condition,
            otherwise,
        } => canonical_rpm_conditional_text(requirement, condition, otherwise.as_deref(), "unless"),
        RepositoryRequirementExpression::With { left, right } => {
            let mut text = format!("({} with ", canonical_rpm_dependency_text(left));
            canonical_rpm_with_right_text(right, &mut text);
            text.push(')');
            text
        }
        RepositoryRequirementExpression::Without { left, right } => format!(
            "({} without {})",
            canonical_rpm_dependency_text(left),
            canonical_rpm_dependency_text(right)
        ),
    }
}

fn canonical_rpm_boolean_text(
    operands: &[RepositoryRequirementExpression],
    operator: &str,
) -> String {
    let separator = format!(" {operator} ");
    format!(
        "({})",
        operands
            .iter()
            .map(canonical_rpm_dependency_text)
            .collect::<Vec<_>>()
            .join(&separator)
    )
}

fn canonical_rpm_conditional_text(
    requirement: &RepositoryRequirementExpression,
    condition: &RepositoryRequirementExpression,
    otherwise: Option<&RepositoryRequirementExpression>,
    operator: &str,
) -> String {
    let mut text = format!(
        "({} {operator} {}",
        canonical_rpm_dependency_text(requirement),
        canonical_rpm_dependency_text(condition)
    );
    if let Some(otherwise) = otherwise {
        text.push_str(" else ");
        text.push_str(&canonical_rpm_dependency_text(otherwise));
    }
    text.push(')');
    text
}

fn canonical_rpm_with_right_text(expression: &RepositoryRequirementExpression, text: &mut String) {
    if let RepositoryRequirementExpression::With { left, right } = expression {
        text.push_str(&canonical_rpm_dependency_text(left));
        text.push_str(" with ");
        canonical_rpm_with_right_text(right, text);
    } else {
        text.push_str(&canonical_rpm_dependency_text(expression));
    }
}
