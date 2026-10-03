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

impl FunctionalAcceptanceV1 {
    /// Compare safe Adapter observations with this saved plan. No response body,
    /// cookie, captured value or physical endpoint belongs in the evidence.
    pub fn validate_observations(
        &self,
        observations: &[crate::port_operations::PortOperationObservation],
    ) -> Result<(), &'static str> {
        use crate::port_operations::{
            Method, Observation, PortOperationObservation, RequestTemplate,
        };
        if self.schema != "ato.functional-http-acceptance/1"
            || self.operations.len() > 4
            || observations.len() > 48
        {
            return Err("functional_observations_bounds");
        }
        fn take<'a>(
            items: &'a [PortOperationObservation],
            cursor: &mut usize,
            index: usize,
            port: &str,
            guard: bool,
        ) -> Result<&'a Observation, &'static str> {
            let item = items
                .get(*cursor)
                .ok_or("functional_observations_missing")?;
            if item.operation_index != index || item.port != port || item.guard != guard {
                return Err("functional_observations_scope");
            }
            *cursor += 1;
            Ok(&item.observation)
        }
        let mut cursor = 0;
        for (index, operation) in self.operations.iter().enumerate() {
            operation.validate()?;
            if let Some(guard) = &operation.when {
                let expected = RequestTemplate {
                    method: Method::Get,
                    path: guard.path.clone(),
                    ..Default::default()
                };
                if take(observations, &mut cursor, index, &operation.port, true)?
                    != &(Observation::RequestTemplate { template: expected })
                {
                    return Err("functional_observations_request");
                }
                let Observation::ResponseStatus { status } =
                    take(observations, &mut cursor, index, &operation.port, true)?
                else {
                    return Err("functional_observations_status");
                };
                if !guard.statuses.contains(status) {
                    continue;
                }
            }
            if take(observations, &mut cursor, index, &operation.port, false)?
                != &(Observation::RequestTemplate {
                    template: operation.request.clone(),
                })
            {
                return Err("functional_observations_request");
            }
            let Observation::ResponseStatus { status } =
                take(observations, &mut cursor, index, &operation.port, false)?
            else {
                return Err("functional_observations_status");
            };
            if !operation.accepted_statuses.contains(status) {
                return Err("functional_observations_status");
            }
            // The Adapter checks response headers before reading JSON bytes.
            for check in operation
                .response_checks
                .iter()
                .filter(|c| c.header_name.is_some())
                .chain(
                    operation
                        .response_checks
                        .iter()
                        .filter(|c| c.header_name.is_none()),
                )
            {
                if take(observations, &mut cursor, index, &operation.port, false)?
                    != &(Observation::ResponseCheck {
                        check: check.clone(),
                        matched: true,
                    })
                {
                    return Err("functional_observations_check");
                }
            }
        }
        if cursor != observations.len() {
            return Err("functional_observations_extra");
        }
        Ok(())
    }
}
