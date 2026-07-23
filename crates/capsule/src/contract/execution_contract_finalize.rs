//! `ato.execution-contract/v1` — G0-2 finalization gate (pure).
//!
//! This module is the RFC §4.6 **strict finalization gate**. An
//! [`ExecutionId`] is issued for an execution *only* when every RFC §4.2
//! required facet has a **measured** value that matches the expected
//! [`ExecutionContractV1`]. If any required facet is unmeasured, finalization
//! refuses (terminal) — it MUST NEVER copy an expected/lock value in to stand
//! in for a measurement.
//!
//! ## Measured-only observation (no lock-copied observation)
//!
//! [`ExecutionObservationV1`] is the facet-wise measured witness. It is
//! constructible **only** from measured facet values via [`ExecutionObservationV1::new`]
//! plus the `measured_*` setters — there is deliberately **no**
//! `From<ExecutionContractV1>`, no `clone`-from-contract constructor, and no
//! way to seed it from an expected/lock contract. An API that accepted an
//! expected-contract copy as its own "observation" would prove nothing
//! (RFC §4.5 forbids lock-copied observations). Every facet defaults to
//! *unmeasured*; a facet only becomes present when a caller (the CLI /
//! application measurement layer — never this pure layer) sets it from a
//! concrete materialized fact.
//!
//! ## Strict gate
//!
//! [`ExecutionObservationV1::finalize`] compares the measured observation
//! against the expected contract facet by facet:
//!
//! * a facet that was not measured ⇒ [`FinalizationError::UnmeasuredFacet`]
//!   naming the facet (terminal refusal — no fabrication);
//! * a facet whose measured value disagrees with the expected contract ⇒
//!   [`FinalizationError::FacetMismatch`] naming the facet;
//! * only when **every** required facet is present *and* matching is the
//!   `execution_id` issued.
//!
//! In G0-2 only three facets are measured today by the producer
//! (`source.digest`, `dependencies[].derivation_digest`/`output_digest`, and
//! `filesystem.readonly_layers`). Every other required facet has no measurement
//! producer yet, so a real observation lacks it and the gate refuses. The unit
//! tests supply *synthetic* full-measurement fixtures to exercise the matching
//! path; they never fabricate a measurement from the expected contract.
//!
//! ## Per-field opaque domain selection (reviewer condition)
//!
//! [`OpaqueContractDigestV1`] does **not** embed its domain — on the wire every
//! opaque facet digest is an identical `blake3:<hex>`. The gate therefore
//! selects the correct [`OpaqueContractDomainV1`] **server-side, per field**
//! (never a domain read from the wire) and recomputes the digest from the
//! measured/stored payload via [`opaque_subcontract_digest`]. A payload whose
//! digest is valid under field A's domain but placed under field B is rejected,
//! because field B recomputes it under B's domain and gets a different digest.
//! See [`verify_opaque_digest`].
//!
//! This module is pure: it depends only on the contract types, `serde_json`,
//! and BLAKE3 (via [`opaque_subcontract_digest`]). It performs **no** host I/O
//! and does no measuring itself — measuring stays in the CLI/application layer,
//! which passes measured values in.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::execution_contract::{
    ContentDigest, EnvironmentValuePayloadV1, EnvironmentVariableContract,
    ExecutionContractEnvelopeV1, ExecutionContractError, ExecutionContractV1, ExecutionId,
    ExternalStateContract, GuestPath, GuestSurfaceContract, OpaqueContractDigestV1,
    OpaqueContractDomainV1, ResolvedTargetContract, opaque_subcontract_digest,
};

/// Classifies an environment variable *name* as secret-bearing.
///
/// Secret **values** are never identity-bearing (RFC §4.3) and must never be
/// persisted as a non-secret env value payload or measured as one; they are
/// bound by name via `ResolvedLaunchContract::secret_bindings`. This is the
/// canonical classifier for the workspace — the CLI observation layer delegates
/// to it so the persisted lock and the measured observation apply exactly one
/// rule.
#[must_use]
pub fn is_sensitive_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("API_KEY")
        || upper.contains("PRIVATE_KEY")
}

/// Terminal errors from the strict finalization gate. Every variant is a
/// refusal to issue an `execution_id`; none is recoverable by substituting a
/// non-measured value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FinalizationError {
    /// A required RFC §4.2 facet had no measured value. The gate refuses rather
    /// than fabricate a value from the expected contract (RFC §4.6).
    #[error(
        "required facet '{0}' was not measured; refusing to issue execution_id \
         (finalization is terminal — a measurement must not be fabricated from the expected contract)"
    )]
    UnmeasuredFacet(&'static str),
    /// A measured facet disagreed with the expected contract.
    #[error("measured facet '{facet}' does not match the expected contract")]
    FacetMismatch { facet: String },
    /// A measured opaque sub-contract payload could not be canonicalized into a
    /// digest (fail-closed: an unserializable payload never yields a digest).
    #[error("failed to derive opaque digest for facet '{facet}': {source}")]
    OpaqueDigest {
        facet: String,
        source: ExecutionContractError,
    },
    /// A secret-bearing env variable name was measured/persisted as a non-secret
    /// value. Secret values never enter identity (RFC §4.3).
    #[error(
        "env variable '{0}' is secret-bearing and must never be measured or persisted as a \
         non-secret value (bind it by name via secret_bindings instead)"
    )]
    SecretEnvValue(String),
    /// An env variable bound as a secret via `secret_bindings` was measured as a
    /// non-secret value. `secret_bindings` is the authoritative secret set, so
    /// this catches names the heuristic misses (e.g. `DATABASE_URL`).
    #[error(
        "env variable '{0}' is bound as a secret via secret_bindings and must never be \
         measured or persisted as a non-secret value"
    )]
    SecretBoundEnvValue(String),
    /// The expected contract itself failed validation while computing the id.
    #[error(transparent)]
    Contract(#[from] ExecutionContractError),
}

/// A finalized execution: the measured facets agreed with the expected contract
/// on every RFC §4.2 facet, and this is the issued identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedExecution {
    /// The expected contract, now witnessed against measured facts facet by
    /// facet. It is byte-equal (facet-wise) to the measured observation, so its
    /// canonical `execution_id` is the measured identity.
    contract: ExecutionContractV1,
    /// The issued execution identity.
    execution_id: ExecutionId,
}

impl FinalizedExecution {
    #[must_use]
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    #[must_use]
    pub fn contract(&self) -> &ExecutionContractV1 {
        &self.contract
    }

    /// Build the non-identity [`ExecutionContractEnvelopeV1`] carrying this
    /// finalized identity. The envelope re-derives and stores the same id;
    /// [`ExecutionContractEnvelopeV1::verify`] recomputes it fail-closed.
    #[must_use]
    pub fn into_envelope(self) -> ExecutionContractEnvelopeV1 {
        ExecutionContractEnvelopeV1 {
            execution_contract: self.contract,
            execution_id: self.execution_id,
            resolved_refs: Default::default(),
            generated_at: None,
            provenance: Value::Null,
            diagnostics: Value::Null,
            evidence: Value::Null,
        }
    }
}

/// A single measured non-secret environment variable: its name and the
/// *normalized value payload* (self-describing JSON) whose digest is committed
/// under [`OpaqueContractDomainV1::EnvironmentValue`]. Never carries a secret
/// value (see [`is_sensitive_env_key`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasuredEnvValue {
    pub name: String,
    pub value_payload: EnvironmentValuePayloadV1,
}

/// A single measured build output: its name, immutable output `digest`, and the
/// normalized projection payload committed under
/// [`OpaqueContractDomainV1::BuildOutputProjection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasuredBuildOutput {
    pub name: String,
    pub digest: ContentDigest,
    pub projection_payload: Value,
}

/// The measured, facet-wise witness of an execution.
///
/// Every field is `Option` and starts unmeasured ([`ExecutionObservationV1::new`]).
/// There is intentionally no constructor that seeds this from an
/// [`ExecutionContractV1`]: a measured observation must come from concrete
/// materialization, never from the expected/lock contract.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExecutionObservationV1 {
    source_digest: Option<ContentDigest>,
    source_projection_payload: Option<Value>,
    target: Option<ResolvedTargetContract>,
    runtime_kind: Option<String>,
    runtime_digest: Option<ContentDigest>,
    runtime_dynamic_payload: Option<Value>,
    dependencies: Option<Vec<MeasuredDependency>>,
    build_outputs: Option<Vec<MeasuredBuildOutput>>,
    launch_argv: Option<Vec<String>>,
    launch_cwd: Option<GuestPath>,
    process_model_payload: Option<Value>,
    environment: Option<Vec<MeasuredEnvValue>>,
    environment_policy_payload: Option<Value>,
    secret_bindings: Option<Vec<String>>,
    filesystem_view_digest: Option<ContentDigest>,
    filesystem_topology_payload: Option<Value>,
    filesystem_readonly_layers: Option<Vec<ContentDigest>>,
    filesystem_writable_paths: Option<Vec<GuestPath>>,
    policy_network_payload: Option<Value>,
    policy_capability_payload: Option<Value>,
    policy_filesystem_payload: Option<Value>,
    guest_surface: Option<GuestSurfaceContract>,
    external_state: Option<Vec<ExternalStateContract>>,
}

/// A measured dependency identity (derivation + output digests are measured
/// today).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasuredDependency {
    pub name: String,
    pub derivation_digest: ContentDigest,
    pub output_digest: ContentDigest,
}

impl ExecutionObservationV1 {
    /// A fully-unmeasured observation. Finalization against any contract will
    /// refuse until the required facets are measured in.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ---- measured-fact setters (measured today) ----

    /// Measured `source.digest` — the materialized source projection digest.
    #[must_use]
    pub fn measured_source_digest(mut self, digest: ContentDigest) -> Self {
        self.source_digest = Some(digest);
        self
    }

    /// Measured `dependencies[]` derivation + immutable output identities.
    #[must_use]
    pub fn measured_dependencies(mut self, dependencies: Vec<MeasuredDependency>) -> Self {
        self.dependencies = Some(dependencies);
        self
    }

    /// Measured `filesystem.readonly_layers` — the immutable layer digests.
    #[must_use]
    pub fn measured_readonly_layers(mut self, layers: Vec<ContentDigest>) -> Self {
        self.filesystem_readonly_layers = Some(layers);
        self
    }

    // ---- measured-fact setters (no producer wired yet; present for the
    // synthetic full-measurement fixtures and for later producer PRs) ----

    #[must_use]
    pub fn measured_source_projection(mut self, payload: Value) -> Self {
        self.source_projection_payload = Some(payload);
        self
    }

    #[must_use]
    pub fn measured_target(mut self, target: ResolvedTargetContract) -> Self {
        self.target = Some(target);
        self
    }

    #[must_use]
    pub fn measured_runtime(mut self, kind: String, digest: ContentDigest) -> Self {
        self.runtime_kind = Some(kind);
        self.runtime_digest = Some(digest);
        self
    }

    #[must_use]
    pub fn measured_runtime_dynamic(mut self, payload: Value) -> Self {
        self.runtime_dynamic_payload = Some(payload);
        self
    }

    #[must_use]
    pub fn measured_build_outputs(mut self, outputs: Vec<MeasuredBuildOutput>) -> Self {
        self.build_outputs = Some(outputs);
        self
    }

    #[must_use]
    pub fn measured_launch(mut self, argv: Vec<String>, cwd: GuestPath) -> Self {
        self.launch_argv = Some(argv);
        self.launch_cwd = Some(cwd);
        self
    }

    #[must_use]
    pub fn measured_process_model(mut self, payload: Value) -> Self {
        self.process_model_payload = Some(payload);
        self
    }

    #[must_use]
    pub fn measured_environment(mut self, environment: Vec<MeasuredEnvValue>) -> Self {
        self.environment = Some(environment);
        self
    }

    #[must_use]
    pub fn measured_environment_policy(mut self, payload: Value) -> Self {
        self.environment_policy_payload = Some(payload);
        self
    }

    #[must_use]
    pub fn measured_secret_bindings(mut self, bindings: Vec<String>) -> Self {
        self.secret_bindings = Some(bindings);
        self
    }

    #[must_use]
    pub fn measured_filesystem_view(mut self, digest: ContentDigest) -> Self {
        self.filesystem_view_digest = Some(digest);
        self
    }

    #[must_use]
    pub fn measured_filesystem_topology(mut self, payload: Value) -> Self {
        self.filesystem_topology_payload = Some(payload);
        self
    }

    #[must_use]
    pub fn measured_writable_paths(mut self, paths: Vec<GuestPath>) -> Self {
        self.filesystem_writable_paths = Some(paths);
        self
    }

    #[must_use]
    pub fn measured_policy(
        mut self,
        network_payload: Value,
        capability_payload: Value,
        filesystem_payload: Value,
    ) -> Self {
        self.policy_network_payload = Some(network_payload);
        self.policy_capability_payload = Some(capability_payload);
        self.policy_filesystem_payload = Some(filesystem_payload);
        self
    }

    #[must_use]
    pub fn measured_guest_surface(mut self, surface: GuestSurfaceContract) -> Self {
        self.guest_surface = Some(surface);
        self
    }

    #[must_use]
    pub fn measured_external_state(mut self, state: Vec<ExternalStateContract>) -> Self {
        self.external_state = Some(state);
        self
    }

    /// Strict finalization gate (RFC §4.6). Issues an `execution_id` only when
    /// every required facet is present and matches `expected`; otherwise refuses
    /// with a typed terminal error naming the offending facet.
    ///
    /// The comparison is facet by facet. Opaque facets are verified by
    /// recomputing the digest from the measured payload under the field's own
    /// [`OpaqueContractDomainV1`] (selected here, server-side — never read from
    /// the wire), so a payload valid for one facet's domain cannot pass as
    /// another facet's commitment. When all facets agree, the measured facets
    /// are byte-equal (facet-wise) to `expected`, so the canonical
    /// `execution_id` of `expected` is exactly the measured identity.
    pub fn finalize(
        &self,
        expected: &ExecutionContractV1,
    ) -> Result<FinalizedExecution, FinalizationError> {
        // Typed measured-today / typed facets.
        let source_digest = self.require("source.digest", self.source_digest)?;
        if source_digest != expected.source.digest {
            return Err(mismatch("source.digest"));
        }

        self.verify_opaque(
            "source.projection_digest",
            self.source_projection_payload.as_ref(),
            OpaqueContractDomainV1::SourceProjection,
            expected.source.projection_digest,
        )?;

        let target = self.require_ref("target", self.target.as_ref())?;
        if *target != expected.target {
            return Err(mismatch("target"));
        }

        let runtime_kind = self.require_ref("runtime.kind", self.runtime_kind.as_ref())?;
        let runtime_digest = self.require("runtime.digest", self.runtime_digest)?;
        if *runtime_kind != expected.runtime.kind || runtime_digest != expected.runtime.digest {
            return Err(mismatch("runtime"));
        }

        self.verify_opaque(
            "runtime.dynamic_contract_digest",
            self.runtime_dynamic_payload.as_ref(),
            OpaqueContractDomainV1::RuntimeDynamic,
            expected.runtime.dynamic_contract_digest,
        )?;

        let dependencies = self.require_ref("dependencies", self.dependencies.as_ref())?;
        if dependencies.len() != expected.dependencies.len() {
            return Err(mismatch("dependencies"));
        }
        for (measured, expected_dep) in dependencies.iter().zip(&expected.dependencies) {
            if measured.name != expected_dep.name
                || measured.derivation_digest != expected_dep.derivation_digest
                || measured.output_digest != expected_dep.output_digest
            {
                return Err(mismatch("dependencies"));
            }
        }

        let build_outputs = self.require_ref("build_outputs", self.build_outputs.as_ref())?;
        if build_outputs.len() != expected.build_outputs.len() {
            return Err(mismatch("build_outputs"));
        }
        for (measured, expected_output) in build_outputs.iter().zip(&expected.build_outputs) {
            if measured.name != expected_output.name || measured.digest != expected_output.digest {
                return Err(mismatch("build_outputs"));
            }
            self.verify_opaque(
                "build_outputs[].projection_digest",
                Some(&measured.projection_payload),
                OpaqueContractDomainV1::BuildOutputProjection,
                expected_output.projection_digest,
            )?;
        }

        let launch_argv = self.require_ref("launch.argv", self.launch_argv.as_ref())?;
        let launch_cwd = self.require_ref("launch.cwd", self.launch_cwd.as_ref())?;
        if *launch_argv != expected.launch.argv || *launch_cwd != expected.launch.cwd {
            return Err(mismatch("launch.argv"));
        }

        self.verify_opaque(
            "launch.process_model_digest",
            self.process_model_payload.as_ref(),
            OpaqueContractDomainV1::ProcessModel,
            expected.launch.process_model_digest,
        )?;

        self.verify_environment(expected)?;

        self.verify_opaque(
            "launch.environment_policy_digest",
            self.environment_policy_payload.as_ref(),
            OpaqueContractDomainV1::EnvironmentPolicy,
            expected.launch.environment_policy_digest,
        )?;

        let secret_bindings =
            self.require_ref("launch.secret_bindings", self.secret_bindings.as_ref())?;
        if *secret_bindings != expected.launch.secret_bindings {
            return Err(mismatch("launch.secret_bindings"));
        }

        let view_digest = self.require("filesystem.view_digest", self.filesystem_view_digest)?;
        if view_digest != expected.filesystem.view_digest {
            return Err(mismatch("filesystem.view_digest"));
        }

        self.verify_opaque(
            "filesystem.topology_digest",
            self.filesystem_topology_payload.as_ref(),
            OpaqueContractDomainV1::FilesystemTopology,
            expected.filesystem.topology_digest,
        )?;

        let readonly_layers = self.require_ref(
            "filesystem.readonly_layers",
            self.filesystem_readonly_layers.as_ref(),
        )?;
        if *readonly_layers != expected.filesystem.readonly_layers {
            return Err(mismatch("filesystem.readonly_layers"));
        }

        let writable_paths = self.require_ref(
            "filesystem.writable_paths",
            self.filesystem_writable_paths.as_ref(),
        )?;
        if *writable_paths != expected.filesystem.writable_paths {
            return Err(mismatch("filesystem.writable_paths"));
        }

        self.verify_opaque(
            "policy.network_digest",
            self.policy_network_payload.as_ref(),
            OpaqueContractDomainV1::NetworkPolicy,
            expected.policy.network_digest,
        )?;
        self.verify_opaque(
            "policy.capability_digest",
            self.policy_capability_payload.as_ref(),
            OpaqueContractDomainV1::CapabilityPolicy,
            expected.policy.capability_digest,
        )?;
        self.verify_opaque(
            "policy.filesystem_digest",
            self.policy_filesystem_payload.as_ref(),
            OpaqueContractDomainV1::FilesystemPolicy,
            expected.policy.filesystem_digest,
        )?;

        let guest_surface = self.require_ref("guest_surface", self.guest_surface.as_ref())?;
        if *guest_surface != expected.guest_surface {
            return Err(mismatch("guest_surface"));
        }

        let external_state = self.require_ref("external_state", self.external_state.as_ref())?;
        if *external_state != expected.external_state {
            return Err(mismatch("external_state"));
        }

        // Every required facet is present and matches `expected`; the measured
        // facets are byte-equal (facet-wise) to it, so its canonical id is the
        // measured identity. This is computed from `expected` only after full
        // measured agreement — never as a stand-in for a missing measurement.
        let execution_id = expected.compute_execution_id()?;
        Ok(FinalizedExecution {
            contract: expected.clone(),
            execution_id,
        })
    }

    fn verify_environment(&self, expected: &ExecutionContractV1) -> Result<(), FinalizationError> {
        let environment = self.require_ref("launch.environment", self.environment.as_ref())?;
        if environment.len() != expected.launch.environment.len() {
            return Err(mismatch("launch.environment"));
        }
        // `secret_bindings` is the authoritative secret set: a measured env name
        // that is bound as a secret must never be measured as a non-secret value,
        // even when the name heuristic does not flag it (e.g. `DATABASE_URL`).
        let secret_bindings: BTreeSet<&str> = expected
            .launch
            .secret_bindings
            .iter()
            .map(String::as_str)
            .collect();
        for (measured, expected_var) in environment.iter().zip(&expected.launch.environment) {
            // A secret-bearing name must never be measured as a non-secret value.
            if is_sensitive_env_key(&measured.name) {
                return Err(FinalizationError::SecretEnvValue(measured.name.clone()));
            }
            if secret_bindings.contains(measured.name.as_str()) {
                return Err(FinalizationError::SecretBoundEnvValue(
                    measured.name.clone(),
                ));
            }
            if measured.name != expected_var.name {
                return Err(mismatch("launch.environment"));
            }
            verify_measured_env_value(measured, expected_var.value_digest)?;
        }
        Ok(())
    }

    fn verify_opaque(
        &self,
        facet: &'static str,
        payload: Option<&Value>,
        domain: OpaqueContractDomainV1,
        expected: OpaqueContractDigestV1,
    ) -> Result<(), FinalizationError> {
        let payload = self.require_ref(facet, payload)?;
        verify_opaque_digest(domain, payload, expected).map_err(|error| match error {
            FinalizationError::FacetMismatch { .. } => FinalizationError::FacetMismatch {
                facet: facet.to_string(),
            },
            FinalizationError::OpaqueDigest { source, .. } => FinalizationError::OpaqueDigest {
                facet: facet.to_string(),
                source,
            },
            other => other,
        })
    }

    fn require<T>(&self, facet: &'static str, value: Option<T>) -> Result<T, FinalizationError> {
        value.ok_or(FinalizationError::UnmeasuredFacet(facet))
    }

    fn require_ref<'a, T>(
        &self,
        facet: &'static str,
        value: Option<&'a T>,
    ) -> Result<&'a T, FinalizationError> {
        value.ok_or(FinalizationError::UnmeasuredFacet(facet))
    }
}

fn mismatch(facet: &str) -> FinalizationError {
    FinalizationError::FacetMismatch {
        facet: facet.to_string(),
    }
}

/// Recompute an opaque sub-contract digest from `payload` under `domain`
/// (selected server-side, per field) and compare it to `expected`.
///
/// This is the reviewer-mandated per-field domain check: because
/// [`OpaqueContractDigestV1`] does not carry its domain, a digest that is valid
/// under one field's domain but placed under another field is rejected here —
/// recomputing `payload` under the *other* field's domain yields a different
/// digest. Returns [`FinalizationError::FacetMismatch`] on disagreement and
/// [`FinalizationError::OpaqueDigest`] if the payload cannot be canonicalized.
pub fn verify_opaque_digest(
    domain: OpaqueContractDomainV1,
    payload: &impl Serialize,
    expected: OpaqueContractDigestV1,
) -> Result<(), FinalizationError> {
    let recomputed = opaque_subcontract_digest(domain, payload).map_err(|source| {
        FinalizationError::OpaqueDigest {
            facet: domain.as_str().to_string(),
            source,
        }
    })?;
    if recomputed != expected {
        return Err(FinalizationError::FacetMismatch {
            facet: domain.as_str().to_string(),
        });
    }
    Ok(())
}

/// Re-derive an [`EnvironmentVariableContract`]-committed value digest from a
/// measured/stored non-secret value payload under
/// [`OpaqueContractDomainV1::EnvironmentValue`].
pub fn environment_value_digest(
    payload: &EnvironmentValuePayloadV1,
) -> Result<OpaqueContractDigestV1, ExecutionContractError> {
    payload.validate()?;
    opaque_subcontract_digest(OpaqueContractDomainV1::EnvironmentValue, payload)
}

/// Verify a measured non-secret env value against the expected committed
/// `value_digest`, rejecting secret-bearing names outright.
pub fn verify_measured_env_value(
    measured: &MeasuredEnvValue,
    expected: OpaqueContractDigestV1,
) -> Result<(), FinalizationError> {
    if is_sensitive_env_key(&measured.name) {
        return Err(FinalizationError::SecretEnvValue(measured.name.clone()));
    }
    measured
        .value_payload
        .validate()
        .map_err(FinalizationError::Contract)?;
    verify_opaque_digest(
        OpaqueContractDomainV1::EnvironmentValue,
        &measured.value_payload,
        expected,
    )
    .map_err(|error| match error {
        FinalizationError::FacetMismatch { .. } => FinalizationError::FacetMismatch {
            facet: format!("launch.environment[{}].value_digest", measured.name),
        },
        other => other,
    })
}

/// Build the expected [`EnvironmentVariableContract`] for a measured non-secret
/// value (used when constructing an expected contract from the same payloads a
/// producer will store in the lock).
pub fn environment_variable_from_measured(
    measured: &MeasuredEnvValue,
) -> Result<EnvironmentVariableContract, FinalizationError> {
    if is_sensitive_env_key(&measured.name) {
        return Err(FinalizationError::SecretEnvValue(measured.name.clone()));
    }
    let value_digest =
        environment_value_digest(&measured.value_payload).map_err(FinalizationError::Contract)?;
    Ok(EnvironmentVariableContract {
        name: measured.name.clone(),
        value_digest,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::num::NonZeroU16;

    use serde_json::json;

    use super::*;
    use crate::execution_contract::{
        DigestAlgorithm, EXECUTION_CONTRACT_V1_SCHEMA, EnvironmentValuePayloadV1,
        ExecutionContractV1, ExternalStateAccess, ResolvedArtifactContract,
        ResolvedBuildOutputContract, ResolvedDependencyContract, ResolvedFilesystemContract,
        ResolvedLaunchContract, ResolvedPolicyContract, ResolvedSourceContract, SnapshotExclusion,
    };

    fn content(byte: u8) -> ContentDigest {
        ContentDigest::new(DigestAlgorithm::Blake3, [byte; 32])
    }

    fn guest(path: &str) -> GuestPath {
        GuestPath::parse(path).expect("canonical guest path")
    }

    fn opaque(domain: OpaqueContractDomainV1, payload: &Value) -> OpaqueContractDigestV1 {
        opaque_subcontract_digest(domain, payload).expect("digest")
    }

    // ---- Synthetic full measurement fixtures. The measured payloads are the
    // source of truth; the expected contract's opaque digests are DERIVED from
    // exactly those payloads. This never fabricates an observation from an
    // expected contract — it derives the expected contract from measurements.

    struct Fixtures {
        source_projection: Value,
        runtime_dynamic: Value,
        build_output_projection: Value,
        process_model: Value,
        environment_policy: Value,
        env_node_env: EnvironmentValuePayloadV1,
        topology: Value,
        network: Value,
        capability: Value,
        filesystem_policy: Value,
    }

    fn fixtures() -> Fixtures {
        Fixtures {
            source_projection: json!({"include": ["src/**"], "case": "sensitive"}),
            runtime_dynamic: json!({"loader": "esm", "abi": "napi-8"}),
            build_output_projection: json!({"place": "/opt/app", "mode": "0755"}),
            process_model: json!({"pid1": true, "supervised": []}),
            environment_policy: json!({"inherit": false, "required": ["NODE_ENV"]}),
            env_node_env: EnvironmentValuePayloadV1::utf8("production"),
            topology: json!({"mounts": [{"at": "/opt/app", "ro": true}]}),
            network: json!({"egress": "deny", "dns": "system"}),
            capability: json!({"caps": [], "devices": []}),
            filesystem_policy: json!({"root": "ro"}),
        }
    }

    fn expected_contract(fx: &Fixtures) -> ExecutionContractV1 {
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                digest: content(1),
                projection_digest: opaque(
                    OpaqueContractDomainV1::SourceProjection,
                    &fx.source_projection,
                ),
            },
            target: ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                abi: "gnu".to_string(),
                libc: Some("glibc-2.39".to_string()),
                observable_features: BTreeMap::new(),
            },
            runtime: ResolvedArtifactContract {
                kind: "node".to_string(),
                digest: content(2),
                dynamic_contract_digest: opaque(
                    OpaqueContractDomainV1::RuntimeDynamic,
                    &fx.runtime_dynamic,
                ),
            },
            dependencies: vec![ResolvedDependencyContract {
                name: "npm".to_string(),
                derivation_digest: content(3),
                output_digest: content(4),
            }],
            build_outputs: vec![ResolvedBuildOutputContract {
                name: "app".to_string(),
                digest: content(5),
                projection_digest: opaque(
                    OpaqueContractDomainV1::BuildOutputProjection,
                    &fx.build_output_projection,
                ),
            }],
            launch: ResolvedLaunchContract {
                argv: vec!["node".to_string(), "dist/server.js".to_string()],
                cwd: guest("/workspace"),
                process_model_digest: opaque(
                    OpaqueContractDomainV1::ProcessModel,
                    &fx.process_model,
                ),
                environment: vec![EnvironmentVariableContract {
                    name: "NODE_ENV".to_string(),
                    value_digest: environment_value_digest(&fx.env_node_env)
                        .expect("env value digest"),
                }],
                environment_policy_digest: opaque(
                    OpaqueContractDomainV1::EnvironmentPolicy,
                    &fx.environment_policy,
                ),
                secret_bindings: vec!["API_TOKEN".to_string()],
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: content(7),
                topology_digest: opaque(OpaqueContractDomainV1::FilesystemTopology, &fx.topology),
                readonly_layers: vec![content(8)],
                writable_paths: vec![guest("/tmp")],
            },
            policy: ResolvedPolicyContract {
                network_digest: opaque(OpaqueContractDomainV1::NetworkPolicy, &fx.network),
                capability_digest: opaque(OpaqueContractDomainV1::CapabilityPolicy, &fx.capability),
                filesystem_digest: opaque(
                    OpaqueContractDomainV1::FilesystemPolicy,
                    &fx.filesystem_policy,
                ),
            },
            guest_surface: GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "ato-guest/v1".to_string(),
                port: Some(NonZeroU16::new(8080).unwrap()),
                features: vec!["bindings".to_string(), "exec".to_string()],
            },
            external_state: vec![ExternalStateContract {
                name: "data".to_string(),
                target: guest("/data"),
                access: ExternalStateAccess::ReadWrite,
                schema: "1".to_string(),
                snapshot: SnapshotExclusion::Exclude,
            }],
        }
    }

    fn full_observation(fx: &Fixtures) -> ExecutionObservationV1 {
        ExecutionObservationV1::new()
            .measured_source_digest(content(1))
            .measured_source_projection(fx.source_projection.clone())
            .measured_target(ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                abi: "gnu".to_string(),
                libc: Some("glibc-2.39".to_string()),
                observable_features: BTreeMap::new(),
            })
            .measured_runtime("node".to_string(), content(2))
            .measured_runtime_dynamic(fx.runtime_dynamic.clone())
            .measured_dependencies(vec![MeasuredDependency {
                name: "npm".to_string(),
                derivation_digest: content(3),
                output_digest: content(4),
            }])
            .measured_build_outputs(vec![MeasuredBuildOutput {
                name: "app".to_string(),
                digest: content(5),
                projection_payload: fx.build_output_projection.clone(),
            }])
            .measured_launch(
                vec!["node".to_string(), "dist/server.js".to_string()],
                guest("/workspace"),
            )
            .measured_process_model(fx.process_model.clone())
            .measured_environment(vec![MeasuredEnvValue {
                name: "NODE_ENV".to_string(),
                value_payload: fx.env_node_env.clone(),
            }])
            .measured_environment_policy(fx.environment_policy.clone())
            .measured_secret_bindings(vec!["API_TOKEN".to_string()])
            .measured_filesystem_view(content(7))
            .measured_filesystem_topology(fx.topology.clone())
            .measured_readonly_layers(vec![content(8)])
            .measured_writable_paths(vec![guest("/tmp")])
            .measured_policy(
                fx.network.clone(),
                fx.capability.clone(),
                fx.filesystem_policy.clone(),
            )
            .measured_guest_surface(GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "ato-guest/v1".to_string(),
                port: Some(NonZeroU16::new(8080).unwrap()),
                features: vec!["bindings".to_string(), "exec".to_string()],
            })
            .measured_external_state(vec![ExternalStateContract {
                name: "data".to_string(),
                target: guest("/data"),
                access: ExternalStateAccess::ReadWrite,
                schema: "1".to_string(),
                snapshot: SnapshotExclusion::Exclude,
            }])
    }

    #[test]
    fn full_measured_observation_finalizes_to_expected_id() {
        let fx = fixtures();
        let expected = expected_contract(&fx);
        let finalized = full_observation(&fx)
            .finalize(&expected)
            .expect("full measurement finalizes");
        assert_eq!(
            *finalized.execution_id(),
            expected.compute_execution_id().unwrap()
        );
        // The envelope re-derives the same id fail-closed.
        finalized
            .into_envelope()
            .verify()
            .expect("envelope verifies");
    }

    #[test]
    fn empty_observation_refuses_naming_the_first_unmeasured_facet() {
        let fx = fixtures();
        let expected = expected_contract(&fx);
        let error = ExecutionObservationV1::new()
            .finalize(&expected)
            .expect_err("nothing measured must refuse");
        assert_eq!(error, FinalizationError::UnmeasuredFacet("source.digest"));
    }

    #[test]
    fn strict_gate_refuses_each_unmeasured_required_facet() {
        // Every facet named here has no measurement producer wired in G0-2, so a
        // real observation lacks it; dropping exactly one from an otherwise-full
        // synthetic observation must refuse and name it.
        let fx = fixtures();
        let expected = expected_contract(&fx);

        type DropFacet = Box<dyn Fn(ExecutionObservationV1) -> ExecutionObservationV1>;
        let cases: Vec<(&str, DropFacet)> = vec![
            (
                "source.projection_digest",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.source_projection_payload = None;
                    o
                }),
            ),
            (
                "target",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.target = None;
                    o
                }),
            ),
            (
                "runtime.dynamic_contract_digest",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.runtime_dynamic_payload = None;
                    o
                }),
            ),
            (
                "build_outputs",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.build_outputs = None;
                    o
                }),
            ),
            (
                "launch.process_model_digest",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.process_model_payload = None;
                    o
                }),
            ),
            (
                "launch.environment_policy_digest",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.environment_policy_payload = None;
                    o
                }),
            ),
            (
                "filesystem.topology_digest",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.filesystem_topology_payload = None;
                    o
                }),
            ),
            (
                "policy.network_digest",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.policy_network_payload = None;
                    o
                }),
            ),
            (
                "policy.capability_digest",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.policy_capability_payload = None;
                    o
                }),
            ),
            (
                "policy.filesystem_digest",
                Box::new(|o: ExecutionObservationV1| {
                    let mut o = o;
                    o.policy_filesystem_payload = None;
                    o
                }),
            ),
        ];

        for (facet, drop_facet) in cases {
            let observation = drop_facet(full_observation(&fx));
            let error = observation
                .finalize(&expected)
                .expect_err("dropping a required facet must refuse");
            assert_eq!(
                error,
                FinalizationError::UnmeasuredFacet(facet),
                "facet {facet}"
            );
        }
    }

    #[test]
    fn wrong_domain_digest_placed_in_another_field_is_rejected() {
        // A payload whose digest is valid under field A's domain (source
        // projection), placed as another field's expected digest (network
        // policy), must be rejected because the network field recomputes it
        // under the network domain and gets a different digest.
        let payload = json!({"include": ["src/**"]});
        let digest_under_source = opaque(OpaqueContractDomainV1::SourceProjection, &payload);

        // Correct domain passes.
        verify_opaque_digest(
            OpaqueContractDomainV1::SourceProjection,
            &payload,
            digest_under_source,
        )
        .expect("same domain matches");

        // Wrong domain (same wire form blake3:<hex>) is rejected.
        let error = verify_opaque_digest(
            OpaqueContractDomainV1::NetworkPolicy,
            &payload,
            digest_under_source,
        )
        .expect_err("wrong domain must be rejected");
        assert!(matches!(error, FinalizationError::FacetMismatch { .. }));
    }

    #[test]
    fn finalize_rejects_wrong_domain_opaque_payload_via_facet_mismatch() {
        // Same principle inside finalize: give the runtime dynamic facet a
        // payload digest computed under the source-projection domain by handing
        // finalize an expected contract whose runtime digest was mis-derived.
        let fx = fixtures();
        let mut expected = expected_contract(&fx);
        // Expected runtime digest computed under the WRONG (source projection)
        // domain over the runtime payload — finalize recomputes under the
        // runtime domain and must reject.
        expected.runtime.dynamic_contract_digest = opaque(
            OpaqueContractDomainV1::SourceProjection,
            &fx.runtime_dynamic,
        );
        let error = full_observation(&fx)
            .finalize(&expected)
            .expect_err("wrong-domain runtime digest must be rejected");
        assert_eq!(
            error,
            FinalizationError::FacetMismatch {
                facet: "runtime.dynamic_contract_digest".to_string()
            }
        );
    }

    #[test]
    fn measured_facet_mismatch_is_named() {
        let fx = fixtures();
        let expected = expected_contract(&fx);
        let observation = full_observation(&fx).measured_source_digest(content(0xff));
        let error = observation
            .finalize(&expected)
            .expect_err("source digest mismatch must fail");
        assert_eq!(
            error,
            FinalizationError::FacetMismatch {
                facet: "source.digest".to_string()
            }
        );
    }

    #[test]
    fn env_value_digest_verifies_and_round_trips() {
        let payload = EnvironmentValuePayloadV1::utf8("production");
        let measured = MeasuredEnvValue {
            name: "NODE_ENV".to_string(),
            value_payload: payload.clone(),
        };
        let digest = environment_value_digest(&payload).unwrap();
        verify_measured_env_value(&measured, digest).expect("verifies");

        // A tampered payload no longer matches the stored digest.
        let tampered = MeasuredEnvValue {
            name: "NODE_ENV".to_string(),
            value_payload: EnvironmentValuePayloadV1::utf8("development"),
        };
        assert!(verify_measured_env_value(&tampered, digest).is_err());
    }

    #[test]
    fn secret_env_names_are_excluded_from_value_measurement() {
        let digest = environment_value_digest(&EnvironmentValuePayloadV1::utf8("x")).unwrap();
        for name in ["OPENAI_API_KEY", "github_token", "DB_PASSWORD", "MY_SECRET"] {
            let measured = MeasuredEnvValue {
                name: name.to_string(),
                value_payload: EnvironmentValuePayloadV1::utf8("x"),
            };
            assert_eq!(
                verify_measured_env_value(&measured, digest),
                Err(FinalizationError::SecretEnvValue(name.to_string())),
                "{name}"
            );
        }
        // A non-secret name is accepted.
        assert!(!is_sensitive_env_key("PATH"));
    }

    #[test]
    fn secret_env_value_in_observation_refuses_finalization() {
        let fx = fixtures();
        let mut expected = expected_contract(&fx);
        // Inject a secret-named env var into both expected and observation; the
        // gate must refuse to measure a secret as a non-secret value.
        expected.launch.environment.insert(
            0,
            EnvironmentVariableContract {
                name: "API_TOKEN".to_string(),
                value_digest: opaque(OpaqueContractDomainV1::EnvironmentValue, &json!("t")),
            },
        );
        let observation = full_observation(&fx).measured_environment(vec![
            MeasuredEnvValue {
                name: "API_TOKEN".to_string(),
                value_payload: EnvironmentValuePayloadV1::utf8("t"),
            },
            MeasuredEnvValue {
                name: "NODE_ENV".to_string(),
                value_payload: fx.env_node_env.clone(),
            },
        ]);
        let error = observation
            .finalize(&expected)
            .expect_err("secret env value must refuse");
        assert_eq!(
            error,
            FinalizationError::SecretEnvValue("API_TOKEN".to_string())
        );
    }

    #[test]
    fn secret_bound_env_value_refuses_finalization_even_when_heuristic_misses() {
        // Blocker 3 layer 2: `secret_bindings` is the authoritative secret set.
        // A name the heuristic does NOT flag (DATABASE_URL, AWS_ACCESS_KEY_ID)
        // that is nonetheless bound as a secret must refuse finalization when
        // measured as a non-secret value.
        for name in ["DATABASE_URL", "AWS_ACCESS_KEY_ID"] {
            assert!(
                !is_sensitive_env_key(name),
                "{name} must dodge the heuristic"
            );
            let fx = fixtures();
            let mut expected = expected_contract(&fx);
            // Bind the name as a secret and (transiently, before validate) present
            // it as a committed env value so the length check aligns; the secret
            // cross-check must fire first.
            expected.launch.secret_bindings = vec!["API_TOKEN".to_string(), name.to_string()];
            expected.launch.secret_bindings.sort();
            expected.launch.environment.insert(
                0,
                EnvironmentVariableContract {
                    name: name.to_string(),
                    value_digest: environment_value_digest(&EnvironmentValuePayloadV1::utf8("x"))
                        .unwrap(),
                },
            );
            let observation = full_observation(&fx).measured_environment(vec![
                MeasuredEnvValue {
                    name: name.to_string(),
                    value_payload: EnvironmentValuePayloadV1::utf8("x"),
                },
                MeasuredEnvValue {
                    name: "NODE_ENV".to_string(),
                    value_payload: fx.env_node_env.clone(),
                },
            ]);
            assert_eq!(
                observation.finalize(&expected),
                Err(FinalizationError::SecretBoundEnvValue(name.to_string())),
                "{name}"
            );
        }
    }

    #[test]
    fn observation_has_no_contract_seeded_constructor() {
        // Compile-time/API-shape proof that an observation cannot be built from
        // an expected contract: the ONLY constructors are `new()`/`default()`
        // (fully unmeasured) plus per-facet measured setters. There is no
        // `From<ExecutionContractV1>`, no `from_contract`, and no clone-in path.
        // A "copy the expected contract in" attempt can at best measure the
        // typed facets it can read, but the expected contract stores opaque
        // DIGESTS (not payloads), so it cannot supply the payloads the opaque
        // facets require — finalization still refuses.
        let fx = fixtures();
        let expected = expected_contract(&fx);
        // Best-effort "seed from contract" using only what the contract exposes
        // (typed facets + digests-as-values is impossible: setters take
        // payloads, not digests). Measure the typed measured-today facets only.
        let seeded = ExecutionObservationV1::new()
            .measured_source_digest(expected.source.digest)
            .measured_dependencies(
                expected
                    .dependencies
                    .iter()
                    .map(|d| MeasuredDependency {
                        name: d.name.clone(),
                        derivation_digest: d.derivation_digest,
                        output_digest: d.output_digest,
                    })
                    .collect(),
            )
            .measured_readonly_layers(expected.filesystem.readonly_layers.clone());
        // Cannot reach a finalized id from a lock-copied seed: an opaque facet
        // is unmeasured.
        assert_eq!(
            seeded.finalize(&expected),
            Err(FinalizationError::UnmeasuredFacet(
                "source.projection_digest"
            ))
        );
    }

    #[test]
    fn finalize_demands_at_least_one_opaque_facet_the_contract_cannot_supply() {
        // Regression guard for the round-2 blocker. The measured-only guarantee
        // (RFC §4.5/§4.6) is HOLISTIC, not per-facet: `finalize` is
        // clone-unsatisfiable ONLY because `ExecutionContractV1` makes these
        // opaque digest fields NON-OPTIONAL and an opaque digest never carries
        // its preimage payload. A clone-in attacker — one who copied the
        // expected/lock contract and presents it as its own "observation" — can
        // read every TYPED facet from the contract, but the contract stores only
        // the DIGESTS of the opaque facets below, so the attacker can never
        // supply a payload for even one of them and `finalize` refuses.
        //
        // This invariant is implicit in the contract's field types. If a future
        // contract shape made every one of these opaque facets optional/absent,
        // the gate would silently become clone-satisfiable again (the exact
        // round-2 blocker). This test fails in that world: it measures every
        // facet the contract can supply and asserts `finalize` STILL refuses,
        // naming one of the required opaque facets.
        const REQUIRED_OPAQUE_FACETS: &[&str] = &[
            "source.projection_digest",
            "runtime.dynamic_contract_digest",
            "launch.process_model_digest",
            "launch.environment_policy_digest",
            "filesystem.topology_digest",
            "policy.network_digest",
            "policy.capability_digest",
            "policy.filesystem_digest",
        ];

        let fx = fixtures();
        let expected = expected_contract(&fx);

        // The maximal clone-in observation: measure every facet the expected
        // contract can supply from its own stored data. The opaque payloads are
        // deliberately absent (the contract holds only their digests), and so
        // are `build_outputs` / `environment`, whose measured forms structurally
        // require an opaque payload (projection / value) the contract lacks.
        let clone_in = ExecutionObservationV1::new()
            .measured_source_digest(expected.source.digest)
            .measured_target(expected.target.clone())
            .measured_runtime(expected.runtime.kind.clone(), expected.runtime.digest)
            .measured_dependencies(
                expected
                    .dependencies
                    .iter()
                    .map(|d| MeasuredDependency {
                        name: d.name.clone(),
                        derivation_digest: d.derivation_digest,
                        output_digest: d.output_digest,
                    })
                    .collect(),
            )
            .measured_launch(expected.launch.argv.clone(), expected.launch.cwd.clone())
            .measured_secret_bindings(expected.launch.secret_bindings.clone())
            .measured_filesystem_view(expected.filesystem.view_digest)
            .measured_readonly_layers(expected.filesystem.readonly_layers.clone())
            .measured_writable_paths(expected.filesystem.writable_paths.clone())
            .measured_guest_surface(expected.guest_surface.clone())
            .measured_external_state(expected.external_state.clone());

        let error = clone_in.finalize(&expected).expect_err(
            "clone-in must never finalize: a required opaque facet is unmeasurable from the contract",
        );
        match error {
            FinalizationError::UnmeasuredFacet(facet) => assert!(
                REQUIRED_OPAQUE_FACETS.contains(&facet),
                "finalize refused on {facet:?}, which is not one of the required opaque facets \
                 {REQUIRED_OPAQUE_FACETS:?}; the measured-only guarantee no longer rests on a \
                 required opaque facet the contract cannot supply",
            ),
            other => panic!(
                "expected an UnmeasuredFacet refusal on a required opaque facet, got {other:?}"
            ),
        }
    }
}
