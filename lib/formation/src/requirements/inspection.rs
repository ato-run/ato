//! A read-only projection of canonical D/K, not another plan or identity.
//! Runtime/Surface observations and placement belong in attempt evidence.
use std::collections::BTreeMap;

use serde::Serialize;

use crate::authoring::{
    BoundContract, BoundDerivation, BoundInput, BoundPort, BoundState, EffectClass,
};
use crate::requirements::ExecutionRequirements;
use crate::variables::VariableRequirement;

#[derive(Debug, Serialize)]
pub struct DeclaredExecution<'a> {
    pub schema: &'static str,
    pub requirements: &'a ExecutionRequirements,
    pub inputs: &'a [BoundInput],
    pub runtimes: &'a BTreeMap<String, String>,
    pub effects: EffectClass,
    pub source_oci: Option<&'a crate::source_oci_plan::SourceOciRecipe>,
    pub components: Vec<Component<'a>>,
    pub ports: &'a [BoundPort],
    pub storage: &'a [BoundState],
    pub variables: &'a [VariableRequirement],
    pub verification: &'a BoundContract,
    pub runtime_verified: bool,
}

#[derive(Debug, Serialize)]
pub struct Component<'a> {
    pub id: &'a str,
    pub protocol: &'a str,
    pub operation: &'a str,
    pub runtimes: &'a BTreeMap<String, String>,
    pub storage: &'a [String],
    pub bindings: &'a [String],
    /// Name-only projection: credential-bearing command/env values never
    /// enter a general inspection response.
    pub environment_names: Vec<&'a str>,
}

/// Existing D owns resource/network/lifecycle/authority requirements, step
/// order, runtimes, Ports, storage and variable purposes. Existing K owns all
/// observations. This view copies neither object and cannot change either.
pub fn inspect<'a>(d: &'a BoundDerivation, k: &'a BoundContract) -> DeclaredExecution<'a> {
    DeclaredExecution {
        schema: "ato.declared-execution-inspection/1",
        requirements: &d.requirements,
        inputs: &d.inputs,
        runtimes: &d.runtimes,
        effects: d.effects,
        source_oci: d.source_oci.as_ref(),
        components: d
            .steps
            .iter()
            .map(|step| Component {
                id: &step.id,
                protocol: &step.protocol,
                operation: &step.op,
                runtimes: &step.runtimes,
                storage: &step.state,
                bindings: &step.bindings,
                environment_names: step.env.keys().map(String::as_str).collect(),
            })
            .collect(),
        ports: &d.ports,
        storage: &d.state,
        variables: &d.variable_bindings,
        verification: k,
        runtime_verified: false,
    }
}
