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
//! ## Mint and gate — one definition
//!
//! A measurement has two consumers, and they must agree:
//!
//! * [`ExecutionObservationV1::into_contract`] **mints** the
//!   [`ExecutionContractV1`] the measurement determines (RFC §4.6's "finalize
//!   `lock.execution_contract`", before "compute Execution Identity"). This is
//!   the only way a contract is ever brought into existence — there is no
//!   constructor taking authored or lock values — so a contract can only ever be
//!   what some concrete measurement said.
//! * [`ExecutionObservationV1::finalize`] **verifies** an expected contract, and
//!   is expressed on top of the mint: it compares `expected` against the
//!   contract this measurement determines. A separate comparison-only
//!   implementation would be a second definition of that contract, and the two
//!   drifting apart is exactly how a producer ends up publishing one identity
//!   while a verifier checked another.
//!
//! Both refuse the same way:
//!
//! * a facet that was not measured ⇒ [`FinalizationError::UnmeasuredFacet`]
//!   naming the FIRST missing facet in RFC §4.2 order (terminal refusal — no
//!   fabrication, and no default that would put an unmeasured value into an
//!   identity);
//! * a facet whose measured value disagrees with the expected contract ⇒
//!   [`FinalizationError::FacetMismatch`] naming the facet;
//! * only when **every** required facet is present *and* matching is the
//!   `execution_id` issued.
//!
//! Because measurement is complete before any comparison, an unmeasured facet is
//! reported ahead of a mismatch in an earlier one. That precedence is deliberate:
//! "you did not measure this" is a statement about the observation itself, while
//! a mismatch is only meaningful once the observation is complete.
//!
//! Only a few facets have a real measurement producer today (`source.digest`,
//! `dependencies` in the zero-dependency case, `filesystem.readonly_layers` for
//! a single rootfs layer, and `launch.environment[].value_digest`), so a real
//! observation is still incomplete and both halves refuse. The unit tests supply
//! *synthetic* full-measurement fixtures to exercise the complete path; they
//! never fabricate a measurement from an expected contract.
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
    ContentDigest, EXECUTION_CONTRACT_V1_SCHEMA, EnvironmentValuePayloadV1,
    EnvironmentVariableContract, ExecutionContractEnvelopeV1, ExecutionContractError,
    ExecutionContractV1, ExecutionId, ExternalStateContract, GuestPath, GuestSurfaceContract,
    OpaqueContractDigestV1, OpaqueContractDomainV1, ResolvedArtifactContract,
    ResolvedBuildOutputContract, ResolvedDependencyContract, ResolvedFilesystemContract,
    ResolvedLaunchContract, ResolvedPolicyContract, ResolvedSourceContract, ResolvedTargetContract,
    VerifiedExecutionId, opaque_subcontract_digest,
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

    /// Obtain a [`VerifiedExecutionId`] from this finalized execution. A completed
    /// strict finalization has already proven, facet by facet, that the measured
    /// values equal the expected contract, so the issued `execution_id` is the
    /// canonical hash by construction. This routes through the proof-preserving
    /// [`VerifiedExecutionId::verify_contract_id`] seam anyway — recomputing the
    /// id from the held contract and comparing — so the wrapper can never be
    /// minted with an id that disagrees with its contract. The recomputation is
    /// expected to always succeed here; a failure would signal a corrupted
    /// [`FinalizedExecution`]. This is one of the only two ways to obtain a
    /// [`VerifiedExecutionId`].
    pub fn verified_execution_id(&self) -> Result<VerifiedExecutionId, ExecutionContractError> {
        VerifiedExecutionId::verify_contract_id(&self.contract, &self.execution_id)
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
            capsule_program_id: None,
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

    /// The [`ExecutionContractV1`] this measurement DETERMINES (RFC §4.6's
    /// "finalize `lock.execution_contract`" step, before "compute Execution
    /// Identity").
    ///
    /// This is the minting half of the gate, and the ONLY way an execution
    /// contract is ever brought into existence: there is no constructor that
    /// takes authored or lock values, so a contract can only be what some
    /// concrete measurement said. Every facet must be measured — the same
    /// [`FinalizationError::UnmeasuredFacet`] refusal [`Self::finalize`] makes,
    /// for the same reason: a contract with a fabricated facet would be an
    /// identity claim about something nobody looked at.
    ///
    /// Opaque facets are committed by recomputing the digest from the measured
    /// payload under the field's OWN [`OpaqueContractDomainV1`], selected here
    /// per field and never read from the wire.
    ///
    /// [`Self::finalize`] is expressed on top of this, so "the contract a
    /// measurement determines" has exactly one definition. Two would eventually
    /// disagree, and a producer that minted one contract while the verifier
    /// checked another is precisely the failure this whole gate exists to
    /// prevent.
    pub fn into_contract(&self) -> Result<ExecutionContractV1, FinalizationError> {
        // Field order IS error order: Rust evaluates struct-literal fields in
        // source order, and the facets are written in RFC §4.2 order, so the
        // facet an incomplete observation is refused on is the FIRST one it is
        // missing — not whichever field a helper happened to touch first.
        Ok(ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                digest: self.require("source.digest", self.source_digest)?,
                projection_digest: self.commit(
                    "source.projection_digest",
                    OpaqueContractDomainV1::SourceProjection,
                    self.source_projection_payload.as_ref(),
                )?,
            },
            target: self.require_ref("target", self.target.as_ref())?.clone(),
            runtime: ResolvedArtifactContract {
                kind: self
                    .require_ref("runtime.kind", self.runtime_kind.as_ref())?
                    .clone(),
                digest: self.require("runtime.digest", self.runtime_digest)?,
                dynamic_contract_digest: self.commit(
                    "runtime.dynamic_contract_digest",
                    OpaqueContractDomainV1::RuntimeDynamic,
                    self.runtime_dynamic_payload.as_ref(),
                )?,
            },
            dependencies: self
                .require_ref("dependencies", self.dependencies.as_ref())?
                .iter()
                .map(|measured| ResolvedDependencyContract {
                    name: measured.name.clone(),
                    derivation_digest: measured.derivation_digest,
                    output_digest: measured.output_digest,
                })
                .collect(),
            build_outputs: self.contract_build_outputs()?,
            launch: self.contract_launch()?,
            filesystem: ResolvedFilesystemContract {
                view_digest: self.require("filesystem.view_digest", self.filesystem_view_digest)?,
                topology_digest: self.commit(
                    "filesystem.topology_digest",
                    OpaqueContractDomainV1::FilesystemTopology,
                    self.filesystem_topology_payload.as_ref(),
                )?,
                readonly_layers: self
                    .require_ref(
                        "filesystem.readonly_layers",
                        self.filesystem_readonly_layers.as_ref(),
                    )?
                    .clone(),
                writable_paths: self
                    .require_ref(
                        "filesystem.writable_paths",
                        self.filesystem_writable_paths.as_ref(),
                    )?
                    .clone(),
            },
            policy: ResolvedPolicyContract {
                network_digest: self.commit(
                    "policy.network_digest",
                    OpaqueContractDomainV1::NetworkPolicy,
                    self.policy_network_payload.as_ref(),
                )?,
                capability_digest: self.commit(
                    "policy.capability_digest",
                    OpaqueContractDomainV1::CapabilityPolicy,
                    self.policy_capability_payload.as_ref(),
                )?,
                filesystem_digest: self.commit(
                    "policy.filesystem_digest",
                    OpaqueContractDomainV1::FilesystemPolicy,
                    self.policy_filesystem_payload.as_ref(),
                )?,
            },
            guest_surface: self
                .require_ref("guest_surface", self.guest_surface.as_ref())?
                .clone(),
            external_state: self
                .require_ref("external_state", self.external_state.as_ref())?
                .clone(),
        })
    }

    /// Mint the [`ExecutionContractEnvelopeV1`] this measurement determines:
    /// the contract from [`Self::into_contract`], carrying the canonical
    /// `execution_id` of that very contract.
    ///
    /// This exists so no caller ever assembles an envelope field-by-field. The
    /// envelope's whole job is to bind a contract to an id, and its
    /// [`ExecutionContractEnvelopeV1::verify`] recomputes that id and refuses a
    /// mismatch — so an envelope built by hand is an envelope that can be built
    /// wrong, and the only place the wrongness surfaces is at the reader, after
    /// it has been persisted and shipped. Minting it here makes the id a
    /// function of the contract by construction: there is no argument to pass
    /// that could disagree.
    ///
    /// Note this is a MINT, not a verification: unlike [`Self::finalize`] there
    /// is no `expected` to check against, because there is nothing to check
    /// against yet — this is the call that brings the identity into existence.
    /// Its only gate is completeness, which [`Self::into_contract`] enforces
    /// facet by facet.
    pub fn into_minted_envelope(&self) -> Result<ExecutionContractEnvelopeV1, FinalizationError> {
        let contract = self.into_contract()?;
        let execution_id = contract.compute_execution_id()?;
        Ok(FinalizedExecution {
            contract,
            execution_id,
        }
        .into_envelope())
    }

    fn contract_build_outputs(
        &self,
    ) -> Result<Vec<ResolvedBuildOutputContract>, FinalizationError> {
        let measured = self.require_ref("build_outputs", self.build_outputs.as_ref())?;
        let mut outputs = Vec::with_capacity(measured.len());
        for output in measured {
            outputs.push(ResolvedBuildOutputContract {
                name: output.name.clone(),
                digest: output.digest,
                projection_digest: self.commit(
                    "build_outputs[].projection_digest",
                    OpaqueContractDomainV1::BuildOutputProjection,
                    Some(&output.projection_payload),
                )?,
            });
        }
        Ok(outputs)
    }

    fn contract_launch(&self) -> Result<ResolvedLaunchContract, FinalizationError> {
        let argv = self
            .require_ref("launch.argv", self.launch_argv.as_ref())?
            .clone();
        let cwd = self
            .require_ref("launch.cwd", self.launch_cwd.as_ref())?
            .clone();
        let process_model_digest = self.commit(
            "launch.process_model_digest",
            OpaqueContractDomainV1::ProcessModel,
            self.process_model_payload.as_ref(),
        )?;
        let measured_env = self.require_ref("launch.environment", self.environment.as_ref())?;
        // The NAME heuristic first, inside the mapping: a secret-bearing name is
        // wrong on its own terms, independently of what the bindings say.
        let environment = measured_env
            .iter()
            .map(environment_variable_from_measured)
            .collect::<Result<Vec<_>, _>>()?;
        let environment_policy_digest = self.commit(
            "launch.environment_policy_digest",
            OpaqueContractDomainV1::EnvironmentPolicy,
            self.environment_policy_payload.as_ref(),
        )?;
        let secret_bindings = self
            .require_ref("launch.secret_bindings", self.secret_bindings.as_ref())?
            .clone();
        // `secret_bindings` is the AUTHORITATIVE secret set, so it catches what
        // the heuristic misses (`DATABASE_URL`): a value bound as a secret must
        // never be committed as a non-secret one. Secret VALUES are never
        // identity-bearing (RFC §4.3) — the name binding is.
        let bound: BTreeSet<&str> = secret_bindings.iter().map(String::as_str).collect();
        if let Some(name) = measured_env
            .iter()
            .map(|measured| &measured.name)
            .find(|name| bound.contains(name.as_str()))
        {
            return Err(FinalizationError::SecretBoundEnvValue(name.clone()));
        }
        Ok(ResolvedLaunchContract {
            argv,
            cwd,
            process_model_digest,
            environment,
            environment_policy_digest,
            secret_bindings,
        })
    }

    /// The first facet that is MEASURED and disagrees with `expected`, ignoring
    /// facets that are not measured yet.
    ///
    /// Deliberately tolerant of an incomplete observation: its whole purpose is
    /// to surface drift that is already provable, before completeness is
    /// demanded.
    fn first_measured_disagreement(
        &self,
        expected: &ExecutionContractV1,
    ) -> Result<Option<String>, FinalizationError> {
        if let Some(digest) = self.source_digest
            && digest != expected.source.digest
        {
            return Ok(Some("source.digest".to_string()));
        }
        if let Some(payload) = self.source_projection_payload.as_ref()
            && self.commit(
                "source.projection_digest",
                OpaqueContractDomainV1::SourceProjection,
                Some(payload),
            )? != expected.source.projection_digest
        {
            return Ok(Some("source.projection_digest".to_string()));
        }
        if let Some(target) = self.target.as_ref()
            && *target != expected.target
        {
            return Ok(Some("target".to_string()));
        }
        if let Some(kind) = self.runtime_kind.as_ref()
            && *kind != expected.runtime.kind
        {
            return Ok(Some("runtime".to_string()));
        }
        if let Some(digest) = self.runtime_digest
            && digest != expected.runtime.digest
        {
            return Ok(Some("runtime".to_string()));
        }
        if let Some(dependencies) = self.dependencies.as_ref() {
            let measured: Vec<ResolvedDependencyContract> = dependencies
                .iter()
                .map(|d| ResolvedDependencyContract {
                    name: d.name.clone(),
                    derivation_digest: d.derivation_digest,
                    output_digest: d.output_digest,
                })
                .collect();
            if measured != expected.dependencies {
                return Ok(Some("dependencies".to_string()));
            }
        }
        if let Some(argv) = self.launch_argv.as_ref()
            && *argv != expected.launch.argv
        {
            return Ok(Some("launch.argv".to_string()));
        }
        if let Some(cwd) = self.launch_cwd.as_ref()
            && *cwd != expected.launch.cwd
        {
            return Ok(Some("launch.argv".to_string()));
        }
        if let Some(bindings) = self.secret_bindings.as_ref()
            && *bindings != expected.launch.secret_bindings
        {
            return Ok(Some("launch.secret_bindings".to_string()));
        }
        if let Some(digest) = self.filesystem_view_digest
            && digest != expected.filesystem.view_digest
        {
            return Ok(Some("filesystem.view_digest".to_string()));
        }
        if let Some(layers) = self.filesystem_readonly_layers.as_ref()
            && *layers != expected.filesystem.readonly_layers
        {
            return Ok(Some("filesystem.readonly_layers".to_string()));
        }
        if let Some(paths) = self.filesystem_writable_paths.as_ref()
            && *paths != expected.filesystem.writable_paths
        {
            return Ok(Some("filesystem.writable_paths".to_string()));
        }
        if let Some(surface) = self.guest_surface.as_ref()
            && *surface != expected.guest_surface
        {
            return Ok(Some("guest_surface".to_string()));
        }
        if let Some(state) = self.external_state.as_ref()
            && *state != expected.external_state
        {
            return Ok(Some("external_state".to_string()));
        }
        Ok(None)
    }

    /// Commit one measured opaque payload under the field's own domain.
    fn commit(
        &self,
        facet: &'static str,
        domain: OpaqueContractDomainV1,
        payload: Option<&Value>,
    ) -> Result<OpaqueContractDigestV1, FinalizationError> {
        let payload = self.require_ref(facet, payload)?;
        opaque_subcontract_digest(domain, payload).map_err(|source| {
            FinalizationError::OpaqueDigest {
                facet: facet.to_string(),
                source,
            }
        })
    }

    /// Strict finalization gate (RFC §4.6). Issues an `execution_id` only when
    /// every required facet is present and matches `expected`; otherwise refuses
    /// with a typed terminal error naming the offending facet.
    ///
    /// Expressed on top of [`Self::into_contract`] on purpose: the gate compares
    /// `expected` against **the contract this measurement determines**, so the
    /// thing being verified is the same thing a producer would have minted. A
    /// second, comparison-only implementation would be a second definition of
    /// that contract, and the two drifting apart is precisely how a builder ends
    /// up verifying one identity and publishing another.
    ///
    /// Note the consequence for error ORDER: every facet must be measured before
    /// any comparison happens, so an unmeasured facet is reported ahead of a
    /// mismatch in an earlier facet. That is the honest precedence — "you did not
    /// measure this" is a statement about the observation itself, while a
    /// mismatch is only meaningful once the observation is complete.
    pub fn finalize(
        &self,
        expected: &ExecutionContractV1,
    ) -> Result<FinalizedExecution, FinalizationError> {
        // Secret refusals come first, and in this order: a secret-bearing NAME is
        // wrong on its own terms, independently of what any binding set says.
        if let Some(measured) = self.environment.as_ref()
            && let Some(name) = measured
                .iter()
                .map(|variable| &variable.name)
                .find(|name| is_sensitive_env_key(name))
        {
            return Err(FinalizationError::SecretEnvValue(name.clone()));
        }

        // Then the cross-check, ahead of every other refusal. A name bound as a
        // secret in `expected` that arrives as a measured non-secret value is a
        // security violation, not a difference of opinion about a field — and
        // reporting it as a `launch.secret_bindings` mismatch would describe the
        // symptom while hiding what actually happened.
        let expected_secrets: BTreeSet<&str> = expected
            .launch
            .secret_bindings
            .iter()
            .map(String::as_str)
            .collect();
        if let Some(measured) = self.environment.as_ref()
            && let Some(name) = measured
                .iter()
                .map(|variable| &variable.name)
                .find(|name| expected_secrets.contains(name.as_str()))
        {
            return Err(FinalizationError::SecretBoundEnvValue(name.clone()));
        }

        // A DISAGREEMENT outranks incompleteness. Both are refusals, but they
        // mean opposite things: an unmeasured facet says "ask again when the
        // build is further along", while a measured facet that disagrees is
        // PROOF of drift — and a caller that treats the first as "not yet" would
        // silently swallow the second. Checking what is measured before
        // requiring everything is what keeps caught drift caught.
        //
        // This scan does not define the contract — `into_contract` does, and
        // still does. It answers a different question: does anything I already
        // measured disagree with what was expected?
        if let Some(facet) = self.first_measured_disagreement(expected)? {
            return Err(mismatch(&facet));
        }
        let measured = self.into_contract()?;
        if let Some(facet) = first_facet_difference(&measured, expected) {
            return Err(mismatch(&facet));
        }
        // Every facet of the measured contract equals `expected`'s, so the two
        // canonicalize identically and `expected`'s id IS the measured identity.
        // Computed from `expected` only after full measured agreement — never as
        // a stand-in for a missing measurement.
        let execution_id = expected.compute_execution_id()?;
        Ok(FinalizedExecution {
            contract: expected.clone(),
            execution_id,
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

/// The FIRST facet on which two contracts differ, named exactly as the gate
/// reports it, or `None` when they are equal.
///
/// The walk order is the RFC §4.2 facet order, so the name a caller sees is the
/// earliest thing that actually went wrong rather than whichever field a derived
/// `PartialEq` happened to reach first. Environment variables are named
/// individually (`launch.environment[NAME].value_digest`) because "the
/// environment differs" is not actionable when there are twenty of them.
fn first_facet_difference(
    measured: &ExecutionContractV1,
    expected: &ExecutionContractV1,
) -> Option<String> {
    if measured.source.digest != expected.source.digest {
        return Some("source.digest".to_string());
    }
    if measured.source.projection_digest != expected.source.projection_digest {
        return Some("source.projection_digest".to_string());
    }
    if measured.target != expected.target {
        return Some("target".to_string());
    }
    if measured.runtime.kind != expected.runtime.kind
        || measured.runtime.digest != expected.runtime.digest
    {
        return Some("runtime".to_string());
    }
    if measured.runtime.dynamic_contract_digest != expected.runtime.dynamic_contract_digest {
        return Some("runtime.dynamic_contract_digest".to_string());
    }
    if measured.dependencies != expected.dependencies {
        return Some("dependencies".to_string());
    }
    if measured.build_outputs.len() != expected.build_outputs.len() {
        return Some("build_outputs".to_string());
    }
    for (measured_output, expected_output) in
        measured.build_outputs.iter().zip(&expected.build_outputs)
    {
        if measured_output.name != expected_output.name
            || measured_output.digest != expected_output.digest
        {
            return Some("build_outputs".to_string());
        }
        if measured_output.projection_digest != expected_output.projection_digest {
            return Some("build_outputs[].projection_digest".to_string());
        }
    }
    if measured.launch.argv != expected.launch.argv || measured.launch.cwd != expected.launch.cwd {
        return Some("launch.argv".to_string());
    }
    if measured.launch.process_model_digest != expected.launch.process_model_digest {
        return Some("launch.process_model_digest".to_string());
    }
    if measured.launch.environment.len() != expected.launch.environment.len() {
        return Some("launch.environment".to_string());
    }
    for (measured_var, expected_var) in measured
        .launch
        .environment
        .iter()
        .zip(&expected.launch.environment)
    {
        if measured_var.name != expected_var.name {
            return Some("launch.environment".to_string());
        }
        if measured_var.value_digest != expected_var.value_digest {
            return Some(format!(
                "launch.environment[{}].value_digest",
                measured_var.name
            ));
        }
    }
    if measured.launch.environment_policy_digest != expected.launch.environment_policy_digest {
        return Some("launch.environment_policy_digest".to_string());
    }
    if measured.launch.secret_bindings != expected.launch.secret_bindings {
        return Some("launch.secret_bindings".to_string());
    }
    if measured.filesystem.view_digest != expected.filesystem.view_digest {
        return Some("filesystem.view_digest".to_string());
    }
    if measured.filesystem.topology_digest != expected.filesystem.topology_digest {
        return Some("filesystem.topology_digest".to_string());
    }
    if measured.filesystem.readonly_layers != expected.filesystem.readonly_layers {
        return Some("filesystem.readonly_layers".to_string());
    }
    if measured.filesystem.writable_paths != expected.filesystem.writable_paths {
        return Some("filesystem.writable_paths".to_string());
    }
    if measured.policy.network_digest != expected.policy.network_digest {
        return Some("policy.network_digest".to_string());
    }
    if measured.policy.capability_digest != expected.policy.capability_digest {
        return Some("policy.capability_digest".to_string());
    }
    if measured.policy.filesystem_digest != expected.policy.filesystem_digest {
        return Some("policy.filesystem_digest".to_string());
    }
    if measured.guest_surface != expected.guest_surface {
        return Some("guest_surface".to_string());
    }
    if measured.external_state != expected.external_state {
        return Some("external_state".to_string());
    }
    None
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

    /// The minted contract IS what the gate verifies — the property the whole
    /// mint/verify split exists to guarantee.
    ///
    /// A producer that minted one contract while the gate checked another is the
    /// failure mode this module is built to prevent, and it cannot be caught by
    /// testing either half alone. So: mint from a measurement, then verify the
    /// SAME measurement against what it minted, and require agreement.
    #[test]
    fn a_minted_contract_is_exactly_what_the_gate_verifies() {
        let fx = fixtures();
        let observation = full_observation(&fx);
        let minted = observation.into_contract().expect("full measurement mints");

        let finalized = observation
            .finalize(&minted)
            .expect("the gate accepts the contract this very measurement minted");
        assert_eq!(
            *finalized.execution_id(),
            minted.compute_execution_id().unwrap(),
            "the minted contract's canonical id is the issued identity"
        );
        // And it is the same contract the hand-derived expectation describes, so
        // minting introduced no facet of its own.
        assert_eq!(minted, expected_contract(&fx));
    }

    /// The minted envelope's id is the canonical hash of the contract it
    /// carries — the invariant that makes hand-assembling an envelope
    /// unnecessary, and therefore something no caller should do.
    #[test]
    fn a_minted_envelope_carries_the_id_of_the_contract_it_holds() {
        let fx = fixtures();
        let observation = full_observation(&fx);

        let envelope = observation
            .into_minted_envelope()
            .expect("full measurement mints an envelope");

        envelope
            .verify()
            .expect("a minted envelope verifies against its own contract");
        assert_eq!(envelope.execution_contract, expected_contract(&fx));
        assert_eq!(
            envelope.execution_id,
            expected_contract(&fx).compute_execution_id().unwrap()
        );
        // Nothing beyond the identity is claimed: the envelope's non-identity
        // fields are a producer's to fill in later, not the mint's to invent.
        assert!(envelope.capsule_program_id.is_none());
        assert!(envelope.generated_at.is_none());
    }

    /// Minting an ENVELOPE is gated exactly as minting a contract is: the
    /// completeness refusal must not be softened by the extra hashing step.
    #[test]
    fn minting_an_envelope_refuses_an_incomplete_measurement() {
        assert_eq!(
            ExecutionObservationV1::new()
                .into_minted_envelope()
                .unwrap_err(),
            FinalizationError::UnmeasuredFacet("source.digest")
        );
    }

    /// Minting demands every facet, and names the FIRST one missing in RFC §4.2
    /// order.
    ///
    /// A mint that filled a gap with a default would put a value into an
    /// identity nobody measured — the same fabrication `finalize` refuses, one
    /// step earlier and with nothing to compare against that would catch it.
    #[test]
    fn minting_refuses_an_incomplete_measurement_naming_the_first_facet() {
        assert_eq!(
            ExecutionObservationV1::new().into_contract().unwrap_err(),
            FinalizationError::UnmeasuredFacet("source.digest")
        );

        let fx = fixtures();
        // One facet short, deep in the walk: the refusal must name THAT facet,
        // not the first one it happens to touch.
        let mut observation = full_observation(&fx);
        observation.external_state = None;
        assert_eq!(
            observation.into_contract().unwrap_err(),
            FinalizationError::UnmeasuredFacet("external_state")
        );
    }

    /// **EVERY** measured field is load-bearing: dropping any one of them, from
    /// an otherwise-complete observation, refuses the mint.
    ///
    /// This is the exhaustive form of the guarantee, and it is exhaustive by
    /// CONSTRUCTION rather than by a hand-maintained list: the destructuring
    /// below binds every field of `ExecutionObservationV1` with no `..` rest
    /// pattern, so adding a facet to the observation **fails to compile here**
    /// until it is given a case. A hand-written list is exactly how a new facet
    /// would quietly acquire an implicit default — the one thing minting must
    /// never do.
    #[test]
    fn every_unmeasured_facet_refuses_the_mint_and_none_has_an_implicit_default() {
        let fx = fixtures();

        // Compile-time completeness gate. If this stops compiling, a facet was
        // added: give it a case below rather than widening the pattern.
        let ExecutionObservationV1 {
            source_digest: _,
            source_projection_payload: _,
            target: _,
            runtime_kind: _,
            runtime_digest: _,
            runtime_dynamic_payload: _,
            dependencies: _,
            build_outputs: _,
            launch_argv: _,
            launch_cwd: _,
            process_model_payload: _,
            environment: _,
            environment_policy_payload: _,
            secret_bindings: _,
            filesystem_view_digest: _,
            filesystem_topology_payload: _,
            filesystem_readonly_layers: _,
            filesystem_writable_paths: _,
            policy_network_payload: _,
            policy_capability_payload: _,
            policy_filesystem_payload: _,
            guest_surface: _,
            external_state: _,
        } = full_observation(&fx);

        type DropFacet = fn(&mut ExecutionObservationV1);
        let cases: &[(&str, DropFacet)] = &[
            ("source.digest", |o| o.source_digest = None),
            ("source.projection_digest", |o| {
                o.source_projection_payload = None
            }),
            ("target", |o| o.target = None),
            ("runtime.kind", |o| o.runtime_kind = None),
            ("runtime.digest", |o| o.runtime_digest = None),
            ("runtime.dynamic_contract_digest", |o| {
                o.runtime_dynamic_payload = None
            }),
            ("dependencies", |o| o.dependencies = None),
            ("build_outputs", |o| o.build_outputs = None),
            ("launch.argv", |o| o.launch_argv = None),
            ("launch.cwd", |o| o.launch_cwd = None),
            ("launch.process_model_digest", |o| {
                o.process_model_payload = None
            }),
            ("launch.environment", |o| o.environment = None),
            ("launch.environment_policy_digest", |o| {
                o.environment_policy_payload = None
            }),
            ("launch.secret_bindings", |o| o.secret_bindings = None),
            ("filesystem.view_digest", |o| {
                o.filesystem_view_digest = None
            }),
            ("filesystem.topology_digest", |o| {
                o.filesystem_topology_payload = None
            }),
            ("filesystem.readonly_layers", |o| {
                o.filesystem_readonly_layers = None
            }),
            ("filesystem.writable_paths", |o| {
                o.filesystem_writable_paths = None
            }),
            ("policy.network_digest", |o| o.policy_network_payload = None),
            ("policy.capability_digest", |o| {
                o.policy_capability_payload = None
            }),
            ("policy.filesystem_digest", |o| {
                o.policy_filesystem_payload = None
            }),
            ("guest_surface", |o| o.guest_surface = None),
            ("external_state", |o| o.external_state = None),
        ];

        for (facet, drop) in cases {
            let mut observation = full_observation(&fx);
            drop(&mut observation);
            let refusal = observation
                .into_contract()
                .expect_err("a facet with no measurement must never be filled in with a default");
            assert_eq!(
                refusal,
                FinalizationError::UnmeasuredFacet(facet),
                "dropping {facet} must refuse, naming that facet"
            );
            // And the gate refuses it too — an incomplete observation can never
            // be promoted to a complete contract by going through `finalize`
            // with an expected contract standing by to supply the gap.
            assert!(matches!(
                observation.finalize(&expected_contract(&fx)),
                Err(FinalizationError::UnmeasuredFacet(_))
            ));
        }
    }

    /// A measured value whose NAME is secret-bearing never becomes a committed
    /// non-secret value, on the mint path too.
    ///
    /// The gate's version of this check compares against an expected contract;
    /// the mint has none, so if this were only enforced there, the first
    /// producer to mint a lock would be the one path with no check at all.
    #[test]
    fn minting_refuses_a_secret_bearing_env_name() {
        let fx = fixtures();
        let observation = full_observation(&fx).measured_environment(vec![MeasuredEnvValue {
            name: "API_TOKEN".to_string(),
            value_payload: EnvironmentValuePayloadV1::utf8("x"),
        }]);
        assert_eq!(
            observation.into_contract().unwrap_err(),
            FinalizationError::SecretEnvValue("API_TOKEN".to_string())
        );

        // And a name the heuristic misses, caught by the authoritative binding
        // set the measurement itself carries.
        let observation = full_observation(&fx)
            .measured_environment(vec![MeasuredEnvValue {
                name: "DATABASE_URL".to_string(),
                value_payload: EnvironmentValuePayloadV1::utf8("x"),
            }])
            .measured_secret_bindings(vec!["DATABASE_URL".to_string()]);
        assert_eq!(
            observation.into_contract().unwrap_err(),
            FinalizationError::SecretBoundEnvValue("DATABASE_URL".to_string())
        );
    }

    #[test]
    fn verified_execution_id_is_obtainable_from_the_finalized_execution() {
        // A completed strict finalization is one of the two sanctioned sources of
        // a VerifiedExecutionId: the measured facets already proved the id is the
        // canonical hash, so the wrapper is available without re-verification.
        let fx = fixtures();
        let expected = expected_contract(&fx);
        let finalized = full_observation(&fx)
            .finalize(&expected)
            .expect("full measurement finalizes");

        let verified = finalized
            .verified_execution_id()
            .expect("finalized execution re-verifies its own id");
        assert_eq!(verified.as_execution_id(), finalized.execution_id());
        assert_eq!(
            *verified.as_execution_id(),
            expected.compute_execution_id().unwrap()
        );
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
