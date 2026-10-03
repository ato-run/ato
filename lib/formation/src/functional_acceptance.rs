//! Separately approved HTTP Adapter actions; they are never part of K or D.
use crate::{
    authoring::BoundDerivation,
    port_operations::{RuntimePortOperation, validate_bound},
    requirements::ExecutionRequirements,
    variables::VariableRequirement,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionalAcceptanceV1 {
    pub schema: String,
    pub variables: Vec<VariableRequirement>,
    pub operations: Vec<RuntimePortOperation>,
}
impl FunctionalAcceptanceV1 {
    /// Canonical D remains unchanged. Additional bindings are only for these
    /// approved operations, and cannot replace a Source-declared binding.
    pub fn validate(
        &self,
        d: &BoundDerivation,
        ceiling: &ExecutionRequirements,
    ) -> Result<Vec<VariableRequirement>, &'static str> {
        if self.schema != "ato.functional-http-acceptance/1"
            || self.variables.len() > 16
            || self.operations.len() > 4
        {
            return Err("functional_acceptance_bounds");
        }
        ceiling
            .validate()
            .map_err(|_| "functional_ceiling_invalid")?;
        d.requirements
            .within(ceiling)
            .map_err(|_| "functional_ceiling_exceeded")?;
        let mut variables = d.variable_bindings.clone();
        for variable in &self.variables {
            crate::variables::validate(std::slice::from_ref(variable))
                .map_err(|_| "functional_variable_invalid")?;
            if variable.artifact_embedding || variables.iter().any(|v| v.name == variable.name) {
                return Err("functional_variable_conflict");
            }
            variables.push(variable.clone());
        }
        if variables.len() > 32 {
            return Err("functional_variable_bounds");
        }
        validate_bound(&self.operations, &d.ports, &d.steps, &variables, ceiling)?;
        Ok(variables)
    }
}
