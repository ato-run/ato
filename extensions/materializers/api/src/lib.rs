//! Shared public contract for encoding and restoring physical realizations.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use ato_adapter_api::{AdapterRegistry, WorkspaceCapturePolicy};
use ato_computation::{ComputationRef, ContentRef};
use ato_objects::{ObjectStore, RecordEnvelope, RecordEnvelopeV2};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    Compatible,
    Incompatible,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreCapability {
    Supported,
    VerifyOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializationPathKind {
    VmSnapshot,
    ReconstructionReplay,
    WorkspaceSnapshot,
    Other,
}

/// Physical runner facts used for fail-closed Materializer compatibility.
/// Product labels such as runner class are deliberately excluded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunnerCapabilities {
    pub architecture: String,
    pub host_os: String,
    pub backends: BTreeSet<String>,
    pub backend_versions: BTreeMap<String, String>,
    pub guest_os: BTreeSet<String>,
    pub snapshot_formats: BTreeSet<String>,
    pub cpu_features: BTreeSet<String>,
    pub memory_mib: u64,
    pub device_features: BTreeSet<String>,
    pub network_features: BTreeSet<String>,
    pub vsock_features: BTreeSet<String>,
}

pub struct MaterializerContext<'a> {
    pub objects: &'a dyn ObjectStore,
    pub adapters: &'a AdapterRegistry,
    pub records: &'a [RecordEnvelope],
    pub records_v2: &'a [RecordEnvelopeV2],
    pub replay_anchor: Option<&'a ComputationRef>,
    pub record_frontier_ref: Option<&'a ContentRef>,
    pub workspace: &'a Path,
    pub workspace_policy: &'a WorkspaceCapturePolicy,
    pub realization: Option<&'a dyn RealizationDriver>,
    /// Realization acceptance requirements selected by the enclosing Capsule.
    /// Materializers may carry these descriptors but never interpret them.
    pub contracts: &'a [ContractDescriptor],
    pub runner_capabilities: Option<&'a RunnerCapabilities>,
}

/// Extension-defined acceptance assertion for a candidate Realization.
///
/// This descriptor is not part of Computation identity. Its payload schema and
/// semantics belong exclusively to the verifier identified by `verifier_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractDescriptor {
    pub verifier_id: String,
    pub payload: Value,
}

impl ContractDescriptor {
    pub fn new(verifier_id: impl Into<String>, payload: Value) -> Result<Self, MaterializerError> {
        let verifier_id = validate_id(&verifier_id.into())?;
        Ok(Self {
            verifier_id,
            payload,
        })
    }

    pub fn validate(&self) -> Result<(), MaterializerError> {
        validate_id(&self.verifier_id).map(|_| ())
    }
}

pub struct ContractContext<'a> {
    pub objects: &'a dyn ObjectStore,
    pub workspace: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractResult {
    pub verifier_id: String,
    pub target: ComputationRef,
    pub summary: String,
}

/// Contract semantics are supplied by extensions, never by Player or Core.
pub trait ContractVerifier: Send + Sync {
    fn id(&self) -> &str;

    fn verify(
        &self,
        descriptor: &ContractDescriptor,
        candidate: &mut dyn Realization,
        context: &ContractContext<'_>,
    ) -> Result<ContractResult, MaterializerError>;
}

#[derive(Default)]
pub struct ContractVerifierRegistry {
    verifiers: BTreeMap<String, Arc<dyn ContractVerifier>>,
}

impl ContractVerifierRegistry {
    pub fn register(
        &mut self,
        verifier: Arc<dyn ContractVerifier>,
    ) -> Result<(), MaterializerError> {
        let id = validate_id(verifier.id())?;
        if self.verifiers.insert(id.clone(), verifier).is_some() {
            return Err(MaterializerError::DuplicateContractVerifier(id));
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&dyn ContractVerifier, MaterializerError> {
        self.verifiers
            .get(id)
            .map(Arc::as_ref)
            .ok_or_else(|| MaterializerError::UnknownContractVerifier(id.to_owned()))
    }

    pub fn can_verify(&self, descriptor: &ContractDescriptor) -> bool {
        descriptor.validate().is_ok() && self.verifiers.contains_key(&descriptor.verifier_id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.verifiers.keys().map(String::as_str)
    }
}

/// A running or runnable physical realization. Returning a computation
/// reference is insufficient: the Materializer must return the thing that
/// actually owns the realized runtime.
pub trait Realization: Send {
    fn target(&self) -> &ComputationRef;
    /// Activates only candidate-internal endpoints. External Surfaces MUST stay
    /// hidden until `publish` is called after every Contract passes.
    fn activate(&mut self) -> Result<(), MaterializerError>;
    fn publish(&mut self) -> Result<(), MaterializerError>;
    fn wait(&mut self) -> Result<(), MaterializerError>;
    fn quiesce(&mut self) -> Result<(), MaterializerError>;

    fn run(mut self: Box<Self>) -> Result<(), MaterializerError> {
        if let Err(error) = self.activate() {
            return Err(cleanup_rejection(&mut *self, "activate", error));
        }
        if let Err(error) = self.publish() {
            return Err(cleanup_rejection(&mut *self, "publish", error));
        }
        let result = self.wait();
        let quiesce = self.quiesce();
        combine_execution_and_cleanup(result, quiesce)
    }
}

/// A candidate that has completed replay/restore, passed every Contract, and
/// only then published its external Surface.
pub struct AcceptedRealization {
    inner: Box<dyn Realization>,
    contract_results: Vec<ContractResult>,
    cleaned: bool,
}

impl AcceptedRealization {
    pub fn target(&self) -> &ComputationRef {
        self.inner.target()
    }

    pub fn contract_results(&self) -> &[ContractResult] {
        &self.contract_results
    }

    /// Explicitly tears down an accepted hosted Realization without waiting
    /// for the underlying workload to exit on its own. Connected workers use
    /// this when the control plane requests that a lease stop. The operation
    /// remains idempotent through the same `cleaned` guard used by `Drop`.
    pub fn quiesce(mut self) -> Result<(), MaterializerError> {
        let result = self.inner.quiesce();
        self.cleaned = true;
        result
    }

    pub fn run(mut self) -> Result<(), MaterializerError> {
        let waited = self.inner.wait();
        let cleanup = self.inner.quiesce();
        self.cleaned = true;
        combine_execution_and_cleanup(waited, cleanup)
    }
}

impl Drop for AcceptedRealization {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.inner.quiesce();
            self.cleaned = true;
        }
    }
}

/// Moves a restored candidate through the hidden activation -> Contract ->
/// Surface publication boundary. Any failure cleans up the candidate before it
/// is returned to the caller.
pub fn accept_candidate(
    mut candidate: Box<dyn Realization>,
    contracts: &[ContractDescriptor],
    verifiers: &ContractVerifierRegistry,
    context: &ContractContext<'_>,
) -> Result<AcceptedRealization, MaterializerError> {
    if let Err(error) = candidate.activate() {
        return Err(cleanup_rejection(&mut *candidate, "activate", error));
    }

    let mut results = Vec::with_capacity(contracts.len());
    for descriptor in contracts {
        if let Err(error) = descriptor.validate() {
            return Err(cleanup_rejection(&mut *candidate, "contract", error));
        }
        let verifier = match verifiers.get(&descriptor.verifier_id) {
            Ok(verifier) => verifier,
            Err(error) => return Err(cleanup_rejection(&mut *candidate, "contract", error)),
        };
        match verifier.verify(descriptor, &mut *candidate, context) {
            Ok(result)
                if result.verifier_id == descriptor.verifier_id
                    && &result.target == candidate.target() =>
            {
                results.push(result);
            }
            Ok(_) => {
                return Err(cleanup_rejection(
                    &mut *candidate,
                    "contract",
                    MaterializerError::Operation(
                        "Contract verifier returned a result for a different verifier or target"
                            .to_owned(),
                    ),
                ));
            }
            Err(error) => return Err(cleanup_rejection(&mut *candidate, "contract", error)),
        }
    }

    if let Err(error) = candidate.publish() {
        return Err(cleanup_rejection(&mut *candidate, "publish", error));
    }
    Ok(AcceptedRealization {
        inner: candidate,
        contract_results: results,
        cleaned: false,
    })
}

fn cleanup_rejection<R: Realization + ?Sized>(
    candidate: &mut R,
    phase: &'static str,
    error: MaterializerError,
) -> MaterializerError {
    let reason = error.to_string();
    let cleanup = candidate.quiesce().err().map(|error| error.to_string());
    MaterializerError::CandidateRejected {
        phase,
        reason,
        cleanup,
    }
}

fn combine_execution_and_cleanup(
    execution: Result<(), MaterializerError>,
    cleanup: Result<(), MaterializerError>,
) -> Result<(), MaterializerError> {
    match (execution, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(MaterializerError::CandidateRejected {
            phase: "wait",
            reason: error.to_string(),
            cleanup: Some(cleanup.to_string()),
        }),
    }
}

/// Mutable reconstruction owned by a Materializer while it applies evidence.
pub trait ReplayRuntime: Send {
    fn apply(&mut self, record: &RecordEnvelope) -> Result<(), MaterializerError>;
    fn finish(
        self: Box<Self>,
        target: &ComputationRef,
    ) -> Result<Box<dyn Realization>, MaterializerError>;
}

/// Operation-based replay runtime. Record application is intentionally
/// separate from legacy computation-head chaining.
pub trait OperationReplayRuntime: Send {
    fn apply(&mut self, record: &RecordEnvelopeV2) -> Result<(), MaterializerError>;
    fn finish(
        self: Box<Self>,
        target: &ComputationRef,
    ) -> Result<Box<dyn Realization>, MaterializerError>;
}

/// Product-specific realization implementation injected into generic
/// Materializers. Only this boundary may materialize a computation into a Run.
pub trait RealizationDriver: Send + Sync {
    fn begin(&self, anchor: &ComputationRef) -> Result<Box<dyn ReplayRuntime>, MaterializerError>;

    fn preflight_operations(&self, _records: &[RecordEnvelopeV2]) -> Result<(), MaterializerError> {
        Err(MaterializerError::OperationReplayUnsupported)
    }

    fn begin_operations(
        &self,
        _anchor: &ComputationRef,
    ) -> Result<Box<dyn OperationReplayRuntime>, MaterializerError> {
        Err(MaterializerError::OperationReplayUnsupported)
    }
}

pub trait Materializer: Send + Sync {
    fn id(&self) -> &str;

    fn path_kind(&self) -> MaterializationPathKind {
        MaterializationPathKind::Other
    }

    fn restore_capability(&self) -> RestoreCapability;

    fn encode(
        &self,
        target: &ComputationRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ContentRef, MaterializerError>;

    fn verify(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ComputationRef, MaterializerError>;

    fn compatibility(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Compatibility;

    fn contracts(
        &self,
        _descriptor: &ContentRef,
        _context: &MaterializerContext<'_>,
    ) -> Result<Vec<ContractDescriptor>, MaterializerError> {
        Ok(Vec::new())
    }

    fn operation_records(
        &self,
        _descriptor: &ContentRef,
        _context: &MaterializerContext<'_>,
    ) -> Result<Vec<RecordEnvelopeV2>, MaterializerError> {
        Ok(Vec::new())
    }

    fn restore(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<Box<dyn Realization>, MaterializerError> {
        let _ = (descriptor, context);
        Err(MaterializerError::RestoreUnsupported(self.id().to_owned()))
    }
}

#[derive(Default)]
pub struct MaterializerRegistry {
    materializers: BTreeMap<String, Arc<dyn Materializer>>,
}

impl MaterializerRegistry {
    pub fn register(
        &mut self,
        materializer: Arc<dyn Materializer>,
    ) -> Result<(), MaterializerError> {
        let id = validate_id(materializer.id())?;
        if self
            .materializers
            .insert(id.clone(), materializer)
            .is_some()
        {
            return Err(MaterializerError::Duplicate(id));
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&dyn Materializer, MaterializerError> {
        self.materializers
            .get(id)
            .map(Arc::as_ref)
            .ok_or_else(|| MaterializerError::Unknown(id.to_owned()))
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.materializers.keys().map(String::as_str)
    }
}

fn validate_id(value: &str) -> Result<String, MaterializerError> {
    let (name, version) = value
        .rsplit_once('@')
        .ok_or_else(|| MaterializerError::InvalidId(value.to_owned()))?;
    if !name.contains('.')
        || name.is_empty()
        || version.is_empty()
        || version.starts_with('0')
        || !version.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(MaterializerError::InvalidId(value.to_owned()));
    }
    Ok(value.to_owned())
}

#[derive(Debug, Error)]
pub enum MaterializerError {
    #[error("invalid materializer id `{0}`")]
    InvalidId(String),
    #[error("materializer `{0}` is already registered")]
    Duplicate(String),
    #[error("materializer `{0}` is not registered")]
    Unknown(String),
    #[error("Contract verifier `{0}` is already registered")]
    DuplicateContractVerifier(String),
    #[error("Contract verifier `{0}` is not registered")]
    UnknownContractVerifier(String),
    #[error("materializer `{0}` cannot restore")]
    RestoreUnsupported(String),
    #[error("materializer `{0}` requires a realization driver")]
    RealizationUnavailable(String),
    #[error("materializer `{materializer}` requires Adapter.apply from `{adapter}`")]
    MissingApply {
        materializer: String,
        adapter: String,
    },
    #[error("materializer operation failed: {0}")]
    Operation(String),
    #[error("the realization driver does not support operation-based replay")]
    OperationReplayUnsupported,
    #[error("candidate rejected during {phase}: {reason}; cleanup: {cleanup:?}")]
    CandidateRejected {
        phase: &'static str,
        reason: String,
        cleanup: Option<String>,
    },
    #[error(transparent)]
    Objects(#[from] ato_objects::ObjectError),
    #[error("materializer JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ato_objects::MemoryObjectStore;

    use super::*;

    struct ThirdParty;

    impl Materializer for ThirdParty {
        fn id(&self) -> &str {
            "example.materializer@1"
        }
        fn restore_capability(&self) -> RestoreCapability {
            RestoreCapability::VerifyOnly
        }
        fn encode(
            &self,
            _: &ComputationRef,
            _: &MaterializerContext<'_>,
        ) -> Result<ContentRef, MaterializerError> {
            unreachable!()
        }
        fn verify(
            &self,
            _: &ContentRef,
            _: &MaterializerContext<'_>,
        ) -> Result<ComputationRef, MaterializerError> {
            unreachable!()
        }
        fn compatibility(&self, _: &ContentRef, _: &MaterializerContext<'_>) -> Compatibility {
            Compatibility::Unknown
        }
    }

    #[test]
    fn third_party_uses_the_same_registry_path() {
        let mut registry = MaterializerRegistry::default();
        registry.register(Arc::new(ThirdParty)).unwrap();
        assert_eq!(
            registry
                .get("example.materializer@1")
                .unwrap()
                .restore_capability(),
            RestoreCapability::VerifyOnly
        );
    }

    struct FakeRealization {
        target: ComputationRef,
        lifecycle: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Realization for FakeRealization {
        fn target(&self) -> &ComputationRef {
            &self.target
        }

        fn activate(&mut self) -> Result<(), MaterializerError> {
            self.lifecycle.lock().unwrap().push("activate");
            Ok(())
        }

        fn publish(&mut self) -> Result<(), MaterializerError> {
            self.lifecycle.lock().unwrap().push("publish");
            Ok(())
        }

        fn wait(&mut self) -> Result<(), MaterializerError> {
            self.lifecycle.lock().unwrap().push("wait");
            Ok(())
        }

        fn quiesce(&mut self) -> Result<(), MaterializerError> {
            self.lifecycle.lock().unwrap().push("quiesce");
            Ok(())
        }
    }

    struct ExtensionVerifier {
        lifecycle: Arc<Mutex<Vec<&'static str>>>,
        reject: bool,
    }

    impl ContractVerifier for ExtensionVerifier {
        fn id(&self) -> &str {
            "example.contract@1"
        }

        fn verify(
            &self,
            descriptor: &ContractDescriptor,
            candidate: &mut dyn Realization,
            _context: &ContractContext<'_>,
        ) -> Result<ContractResult, MaterializerError> {
            let mut lifecycle = self.lifecycle.lock().unwrap();
            assert_eq!(lifecycle.as_slice(), ["activate"]);
            lifecycle.push("verify");
            drop(lifecycle);
            if self.reject {
                return Err(MaterializerError::Operation(
                    "extension assertion rejected the candidate".to_owned(),
                ));
            }
            Ok(ContractResult {
                verifier_id: descriptor.verifier_id.clone(),
                target: candidate.target().clone(),
                summary: "PASS".to_owned(),
            })
        }
    }

    fn computation() -> ComputationRef {
        ComputationRef::parse(format!("blake3:{}", "a".repeat(64))).unwrap()
    }

    #[test]
    fn contract_extension_runs_before_surface_publication() {
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let candidate = Box::new(FakeRealization {
            target: computation(),
            lifecycle: Arc::clone(&lifecycle),
        });
        let descriptor = ContractDescriptor::new(
            "example.contract@1",
            serde_json::json!({ "marker": "ready" }),
        )
        .unwrap();
        let mut registry = ContractVerifierRegistry::default();
        registry
            .register(Arc::new(ExtensionVerifier {
                lifecycle: Arc::clone(&lifecycle),
                reject: false,
            }))
            .unwrap();
        let objects = MemoryObjectStore::default();
        let context = ContractContext {
            objects: &objects,
            workspace: Path::new("."),
        };

        let accepted = accept_candidate(candidate, &[descriptor], &registry, &context).unwrap();

        assert_eq!(accepted.target(), &computation());
        assert_eq!(accepted.contract_results()[0].summary, "PASS");
        assert_eq!(
            lifecycle.lock().unwrap().as_slice(),
            ["activate", "verify", "publish"]
        );
        drop(accepted);
        assert_eq!(
            lifecycle.lock().unwrap().as_slice(),
            ["activate", "verify", "publish", "quiesce"]
        );
    }

    #[test]
    fn rejected_contract_cleans_hidden_candidate_without_publication() {
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let candidate = Box::new(FakeRealization {
            target: computation(),
            lifecycle: Arc::clone(&lifecycle),
        });
        let descriptor = ContractDescriptor::new(
            "example.contract@1",
            serde_json::json!({ "marker": "ready" }),
        )
        .unwrap();
        let mut registry = ContractVerifierRegistry::default();
        registry
            .register(Arc::new(ExtensionVerifier {
                lifecycle: Arc::clone(&lifecycle),
                reject: true,
            }))
            .unwrap();
        let objects = MemoryObjectStore::default();
        let context = ContractContext {
            objects: &objects,
            workspace: Path::new("."),
        };

        let error = match accept_candidate(candidate, &[descriptor], &registry, &context) {
            Ok(_) => panic!("rejected Contract must not accept a candidate"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("candidate rejected during contract")
        );
        assert_eq!(
            lifecycle.lock().unwrap().as_slice(),
            ["activate", "verify", "quiesce"]
        );
    }

    #[test]
    fn missing_verifier_fails_closed_and_cleans_candidate() {
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let candidate = Box::new(FakeRealization {
            target: computation(),
            lifecycle: Arc::clone(&lifecycle),
        });
        let descriptor =
            ContractDescriptor::new("missing.contract@1", serde_json::json!({})).unwrap();
        let objects = MemoryObjectStore::default();
        let context = ContractContext {
            objects: &objects,
            workspace: Path::new("."),
        };

        let error = match accept_candidate(
            candidate,
            &[descriptor],
            &ContractVerifierRegistry::default(),
            &context,
        ) {
            Ok(_) => panic!("missing verifier must not accept a candidate"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("not registered"));
        assert_eq!(
            lifecycle.lock().unwrap().as_slice(),
            ["activate", "quiesce"]
        );
    }
}
