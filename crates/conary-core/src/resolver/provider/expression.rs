// crates/conary-core/src/resolver/provider/expression.rs

//! Exact compilation of native Boolean dependency expressions for resolvo.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use resolvo::{
    Condition, ConditionId, ConditionalRequirement, LogicalOperator, Requirement, VersionSetId,
};

use crate::error::{Error, Result};
use crate::repository::versioning::{RepoVersionConstraint, VersionScheme};

use super::ConaryProvider;
use super::types::{ConaryConstraint, SolverAtom, SolverExpression};

const FALSE_REQUIREMENT_NAME: &str = "\0conary:false";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Literal {
    atom: SolverAtom,
    positive: bool,
}

#[derive(Debug, Clone)]
enum NormalExpression {
    Literal(Box<Literal>),
    And(Vec<NormalExpression>),
    Or(Vec<NormalExpression>),
}

impl ConaryProvider<'_> {
    pub(super) fn compile_dependency_requirements(&mut self) -> Result<()> {
        let dependencies = std::mem::take(&mut self.dependencies);
        self.compiled_dependencies.clear();
        self.compiled_requirement_groups.clear();

        for (solvable, deps) in &dependencies {
            let mut requirements = Vec::new();
            for dep in deps {
                requirements.extend(self.compile_expression(
                    &dep.expression,
                    dep.requirement_group.map(|group| (*solvable, group)),
                )?);
            }
            self.compiled_dependencies.insert(*solvable, requirements);
        }

        self.dependencies = dependencies;
        Ok(())
    }

    pub(crate) fn compile_root_requirements(
        &mut self,
        expressions: &[SolverExpression],
    ) -> Result<Vec<ConditionalRequirement>> {
        let requirements = expressions
            .iter()
            .map(|expression| self.compile_expression(expression, None))
            .collect::<Result<Vec<_>>>()
            .map(|requirements| requirements.into_iter().flatten().collect())?;
        self.validate_solver_invariants()?;
        Ok(requirements)
    }

    fn compile_expression(
        &mut self,
        expression: &SolverExpression,
        provenance: Option<(u32, super::types::RequirementGroupIdentity)>,
    ) -> Result<Vec<ConditionalRequirement>> {
        let normal = to_normal_form(expression, false);
        let clauses = to_cnf(&normal);
        let mut requirements = Vec::with_capacity(clauses.len());

        for clause in clauses {
            let Some(clause) = normalize_clause(clause) else {
                continue;
            };
            if let Some(requirement) = self.compile_clause(&clause, provenance)? {
                requirements.push(requirement);
            }
        }

        Ok(requirements)
    }

    fn compile_clause(
        &mut self,
        clause: &[Literal],
        provenance: Option<(u32, super::types::RequirementGroupIdentity)>,
    ) -> Result<Option<ConditionalRequirement>> {
        let mut positive = Vec::new();
        let mut negative = Vec::new();

        for literal in clause {
            if let ConaryConstraint::RpmRuntime(requirement) = &literal.atom.constraint {
                requirement
                    .ensure_supported()
                    .map_err(|error| Error::ResolutionError(error.to_string()))?;
                if literal.positive {
                    // A true literal satisfies the complete disjunction.
                    return Ok(None);
                }
                // A negated true literal is false and contributes nothing to
                // the surrounding disjunction.
                continue;
            }
            self.intern_constraint(&literal.atom.name, &literal.atom.constraint)?;
            let version_set = self.version_set_id(&literal.atom).ok_or_else(|| {
                Error::ResolutionError(format!(
                    "dependency atom '{}' was not interned",
                    literal.atom.name
                ))
            })?;
            if literal.positive {
                positive.push(version_set);
            } else {
                negative.push(self.intern_condition_literal(&literal.atom, version_set)?);
            }
        }

        positive.sort_by_key(|id| id.0);
        positive.dedup();
        negative.sort_unstable();
        negative.dedup();

        let requirement = match positive.as_slice() {
            [] => Requirement::Single(self.intern_false_requirement()?),
            [single] => Requirement::Single(*single),
            alternatives => {
                let union = self
                    .find_union_id(alternatives)
                    .map_or_else(|| self.intern_version_set_union(alternatives.to_vec()), Ok)?;
                Requirement::Union(union)
            }
        };

        if let Some((solvable, group)) = provenance {
            for version_set in &positive {
                self.compiled_requirement_groups
                    .entry((solvable, version_set.0))
                    .or_default()
                    .insert(group);
            }
            if positive.is_empty()
                && let Requirement::Single(version_set) = requirement
            {
                self.compiled_requirement_groups
                    .entry((solvable, version_set.0))
                    .or_default()
                    .insert(group);
            }
        }

        let condition = self.intern_conjunction(&negative)?;
        Ok(Some(ConditionalRequirement {
            condition,
            requirement,
        }))
    }

    fn version_set_id(&self, atom: &SolverAtom) -> Option<VersionSetId> {
        let name = self.name_to_id.get(&atom.name)?;
        self.version_set_cache
            .get(&(name.into_raw(), atom.constraint.clone()))
            .copied()
    }

    fn intern_false_requirement(&mut self) -> Result<VersionSetId> {
        let name = self.intern_name(FALSE_REQUIREMENT_NAME)?;
        self.intern_repo_version_set(
            name,
            VersionScheme::Conary,
            RepoVersionConstraint::Any,
            None,
        )
    }

    /// Intern the Boolean condition for one negated literal.
    ///
    /// A condition literal is true exactly when a selected solvable satisfies
    /// the atom the same way a positive requirement would. resolvo tracks
    /// presence per name, so a version set whose name is the atom's own name
    /// cannot express a capability or canonical provider that carries a
    /// different concrete name. Compute the matching solvables with the same
    /// discovery and filtering the SAT requirement path uses, group them by
    /// concrete name, and return an OR of per-name exact-solvable requirements.
    /// When every match already carries the atom's own name the original
    /// version set is exact; when nothing matches the condition is constant
    /// false.
    fn intern_condition_literal(
        &mut self,
        atom: &SolverAtom,
        version_set: VersionSetId,
    ) -> Result<ConditionId> {
        let Some(&name_id) = self.name_to_id.get(&atom.name) else {
            return self.intern_false_condition();
        };
        let candidates = self.candidates_for_name(name_id);
        let matching = self.matching_candidates(&candidates, version_set, false);
        if matching.is_empty() {
            return self.intern_false_condition();
        }
        let all_same_name = matching
            .iter()
            .all(|&solvable| self.get_solvable(solvable).name == atom.name);
        if all_same_name {
            return self.intern_condition(Condition::Requirement(version_set));
        }

        let mut groups: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
        for solvable in matching {
            let package = self.get_solvable(solvable);
            groups
                .entry(package.name.clone())
                .or_default()
                .insert(solvable.into_raw());
        }

        let mut conditions = Vec::with_capacity(groups.len());
        for (name, solvables) in groups {
            let name_id = self.intern_name(&name)?;
            let condition_set = self
                .intern_conary_version_set(name_id, ConaryConstraint::ExactSolvables(solvables))?;
            conditions.push(self.intern_condition(Condition::Requirement(condition_set))?);
        }
        self.intern_disjunction(&conditions)
    }

    /// The constant-false condition, encoded through the same unsatisfiable
    /// version-set name the requirement path uses.
    fn intern_false_condition(&mut self) -> Result<ConditionId> {
        let version_set = self.intern_false_requirement()?;
        self.intern_condition(Condition::Requirement(version_set))
    }

    fn intern_conjunction(&mut self, conditions: &[ConditionId]) -> Result<Option<ConditionId>> {
        let Some((&first, rest)) = conditions.split_first() else {
            return Ok(None);
        };
        let mut combined = first;
        for &condition in rest {
            combined = self.intern_condition(Condition::Binary(
                LogicalOperator::And,
                combined,
                condition,
            ))?;
        }
        Ok(Some(combined))
    }

    fn intern_disjunction(&mut self, conditions: &[ConditionId]) -> Result<ConditionId> {
        let Some((&first, rest)) = conditions.split_first() else {
            return self.intern_false_condition();
        };
        let mut combined = first;
        for &condition in rest {
            combined =
                self.intern_condition(Condition::Binary(LogicalOperator::Or, combined, condition))?;
        }
        Ok(combined)
    }

    fn intern_condition(&mut self, condition: Condition) -> Result<ConditionId> {
        if let Some(existing) = self.condition_cache.get(&condition) {
            return Ok(*existing);
        }
        let index = Self::pool_u32(self.conditions.len(), "condition")?;
        let id = ConditionId::new(index);
        self.conditions.push(condition.clone());
        self.condition_cache.insert(condition, id);
        Ok(id)
    }
}

fn to_normal_form(expression: &SolverExpression, negated: bool) -> NormalExpression {
    match expression {
        SolverExpression::Atom(atom) => NormalExpression::Literal(Box::new(Literal {
            atom: atom.clone(),
            positive: !negated,
        })),
        SolverExpression::And(operands) => {
            let operands = operands
                .iter()
                .map(|operand| to_normal_form(operand, negated))
                .collect();
            if negated {
                NormalExpression::Or(operands)
            } else {
                NormalExpression::And(operands)
            }
        }
        SolverExpression::Or(operands) => {
            let operands = operands
                .iter()
                .map(|operand| to_normal_form(operand, negated))
                .collect();
            if negated {
                NormalExpression::And(operands)
            } else {
                NormalExpression::Or(operands)
            }
        }
        SolverExpression::Not(operand) => to_normal_form(operand, !negated),
    }
}

fn to_cnf(expression: &NormalExpression) -> Vec<Vec<Literal>> {
    match expression {
        NormalExpression::Literal(literal) => vec![vec![literal.as_ref().clone()]],
        NormalExpression::And(operands) => operands.iter().flat_map(to_cnf).collect(),
        NormalExpression::Or(operands) => {
            let mut operands = operands.iter();
            let Some(first) = operands.next() else {
                return vec![Vec::new()];
            };
            operands.fold(to_cnf(first), |left, right| distribute(left, to_cnf(right)))
        }
    }
}

fn distribute(left: Vec<Vec<Literal>>, right: Vec<Vec<Literal>>) -> Vec<Vec<Literal>> {
    if left.is_empty() || right.is_empty() {
        return Vec::new();
    }
    left.into_iter()
        .flat_map(|left_clause| {
            right.iter().cloned().map(move |right_clause| {
                let mut clause = left_clause.clone();
                clause.extend(right_clause);
                clause
            })
        })
        .collect()
}

fn normalize_clause(clause: Vec<Literal>) -> Option<Vec<Literal>> {
    let mut positive = HashSet::new();
    let mut negative = HashSet::new();
    let mut normalized = Vec::new();

    for literal in clause {
        let opposite = if literal.positive {
            &negative
        } else {
            &positive
        };
        if opposite.contains(&literal.atom) {
            return None;
        }
        let same = if literal.positive {
            &mut positive
        } else {
            &mut negative
        };
        if same.insert(literal.atom.clone()) {
            normalized.push(literal);
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::dependency_model::RepositoryRequirementClause;
    use crate::repository::rpm_runtime::RpmRuntimeRequirement;

    fn atom(name: &str) -> SolverExpression {
        SolverExpression::atom(
            name.to_string(),
            ConaryConstraint::Repository {
                scheme: VersionScheme::Rpm,
                constraint: RepoVersionConstraint::Any,
                capability_kind: None,
                raw: None,
                architecture_qualifier: Default::default(),
                depending_architecture: "x86_64".to_string(),
            },
        )
    }

    fn runtime_atom() -> SolverExpression {
        let clause = RepositoryRequirementClause::versioned(
            "rpmlib(CompressedFileNames)".to_string(),
            "<= 3.0.4-1".to_string(),
        );
        let requirement = RpmRuntimeRequirement::from_clause(&clause, VersionScheme::Rpm)
            .unwrap()
            .unwrap();
        SolverExpression::atom(
            requirement.feature.capability().to_string(),
            ConaryConstraint::RpmRuntime(requirement),
        )
    }

    fn provider() -> (tempfile::TempDir, rusqlite::Connection) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("expression-test.db");
        crate::db::init(&path).unwrap();
        let connection = crate::db::open(&path).unwrap();
        (directory, connection)
    }

    #[test]
    fn converts_nested_boolean_expression_to_exact_cnf() {
        let expression = SolverExpression::Or(vec![
            atom("a"),
            SolverExpression::And(vec![atom("b"), atom("c")]),
        ]);
        let cnf = to_cnf(&to_normal_form(&expression, false));
        assert_eq!(cnf.len(), 2);
        assert_eq!(cnf[0].len(), 2);
        assert_eq!(cnf[1].len(), 2);
    }

    #[test]
    fn removes_tautological_clauses() {
        let expression =
            SolverExpression::Or(vec![atom("a"), SolverExpression::Not(Box::new(atom("a")))]);
        let cnf = to_cnf(&to_normal_form(&expression, false));
        assert!(normalize_clause(cnf[0].clone()).is_none());
    }

    #[test]
    fn mixed_runtime_or_expression_never_interns_a_fake_package_provider() {
        let (_directory, connection) = provider();
        let mut provider = ConaryProvider::new(&connection).unwrap();
        let expression = SolverExpression::Or(vec![atom("missing"), runtime_atom()]);

        let requirements = provider.compile_root_requirements(&[expression]).unwrap();

        assert!(requirements.is_empty());
        assert!(
            !provider
                .names
                .iter()
                .any(|name| name.starts_with("rpmlib("))
        );
    }

    #[test]
    fn runtime_true_is_removed_from_mixed_and_expression() {
        let (_directory, connection) = provider();
        let mut provider = ConaryProvider::new(&connection).unwrap();
        let expression = SolverExpression::And(vec![atom("bash"), runtime_atom()]);

        let requirements = provider.compile_root_requirements(&[expression]).unwrap();

        assert_eq!(requirements.len(), 1);
        assert!(provider.names.iter().any(|name| name == "bash"));
        assert!(
            !provider
                .names
                .iter()
                .any(|name| name.starts_with("rpmlib("))
        );
    }
}
