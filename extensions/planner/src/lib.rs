//! Explicit selection of physical Realization paths.
//!
//! Materializers report compatibility; they never choose or execute fallback.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use ato_adapter_api::{ActuatorProviderRegistry, AdapterContext, PortBinding};
use ato_computation::{ComputationRef, ContentRef};
use ato_materializer_api::{
    Compatibility, ContractVerifierRegistry, MaterializationPathKind, MaterializerContext,
    MaterializerRegistry, RestoreCapability,
};
use ato_player::Player;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Placement {
    Local,
    Hosted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrustBoundary {
    Local,
    TenantIsolated,
    PublicHosted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetEnvironment {
    pub id: String,
    pub placement: Placement,
    pub trust_boundary: TrustBoundary,
}

pub struct MaterializationCandidate<'a> {
    pub materializer_id: String,
    pub descriptor_ref: ContentRef,
    pub environment: TargetEnvironment,
    pub context: MaterializerContext<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPath {
    pub ordinal: usize,
    pub materializer_id: String,
    pub descriptor_ref: ContentRef,
    pub environment: TargetEnvironment,
    pub kind: MaterializationPathKind,
    pub contract_count: usize,
    pub operation_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedPath {
    pub ordinal: usize,
    pub materializer_id: String,
    pub environment_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RealizationPlan {
    pub candidates: Vec<PlannedPath>,
    pub rejected: Vec<RejectedPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerPolicy {
    pub allowed_trust_boundaries: BTreeSet<TrustBoundary>,
}

impl Default for PlannerPolicy {
    fn default() -> Self {
        Self {
            allowed_trust_boundaries: BTreeSet::from([
                TrustBoundary::Local,
                TrustBoundary::TenantIsolated,
            ]),
        }
    }
}

pub struct RealizationPlanner<'a> {
    pub target: &'a ComputationRef,
    pub materializers: &'a MaterializerRegistry,
    pub actuator_providers: &'a ActuatorProviderRegistry,
    pub contract_verifiers: &'a ContractVerifierRegistry,
    pub port_bindings: &'a [PortBinding],
    pub policy: &'a PlannerPolicy,
}

impl RealizationPlanner<'_> {
    pub fn plan(
        &self,
        candidates: Vec<MaterializationCandidate<'_>>,
    ) -> Result<RealizationPlan, PlannerError> {
        let mut plan = RealizationPlan::default();
        for (ordinal, candidate) in candidates.into_iter().enumerate() {
            match self.evaluate(ordinal, &candidate) {
                Ok(path) => plan.candidates.push(path),
                Err(reason) => plan.rejected.push(RejectedPath {
                    ordinal,
                    materializer_id: candidate.materializer_id,
                    environment_id: candidate.environment.id,
                    reason,
                }),
            }
        }
        plan.candidates
            .sort_by_key(|path| (rank(path), path.ordinal));
        if plan.candidates.is_empty() {
            return Err(PlannerError::NoCandidate(plan.rejected));
        }
        Ok(plan)
    }

    fn evaluate(
        &self,
        ordinal: usize,
        candidate: &MaterializationCandidate<'_>,
    ) -> Result<PlannedPath, String> {
        if !self
            .policy
            .allowed_trust_boundaries
            .contains(&candidate.environment.trust_boundary)
        {
            return Err("policy rejects the target trust boundary".to_owned());
        }
        let materializer = self
            .materializers
            .get(&candidate.materializer_id)
            .map_err(|error| error.to_string())?;
        if materializer.restore_capability() != RestoreCapability::Supported {
            return Err("Materializer is verify-only".to_owned());
        }
        match materializer.compatibility(&candidate.descriptor_ref, &candidate.context) {
            Compatibility::Compatible => {}
            Compatibility::Incompatible => return Err("Materializer is incompatible".to_owned()),
            Compatibility::Unknown => {
                return Err("Materializer compatibility is unknown".to_owned());
            }
        }
        let verified = materializer
            .verify(&candidate.descriptor_ref, &candidate.context)
            .map_err(|error| error.to_string())?;
        if &verified != self.target {
            return Err(format!(
                "Materializer targets {verified}, expected {}",
                self.target
            ));
        }
        let contracts = materializer
            .contracts(&candidate.descriptor_ref, &candidate.context)
            .map_err(|error| error.to_string())?;
        if let Some(contract) = contracts
            .iter()
            .find(|contract| !self.contract_verifiers.can_verify(contract))
        {
            return Err(format!(
                "Contract verifier `{}` is unavailable",
                contract.verifier_id
            ));
        }
        let records = materializer
            .operation_records(&candidate.descriptor_ref, &candidate.context)
            .map_err(|error| error.to_string())?;
        if !records.is_empty() {
            Player::new(
                self.actuator_providers,
                self.port_bindings,
                AdapterContext {
                    workspace: candidate.context.workspace,
                    objects: candidate.context.objects,
                },
            )
            .preflight(&records)
            .map_err(|error| error.to_string())?;
        }
        Ok(PlannedPath {
            ordinal,
            materializer_id: candidate.materializer_id.clone(),
            descriptor_ref: candidate.descriptor_ref.clone(),
            environment: candidate.environment.clone(),
            kind: materializer.path_kind(),
            contract_count: contracts.len(),
            operation_count: records.len(),
        })
    }
}

fn rank(path: &PlannedPath) -> u8 {
    match (path.environment.placement, path.kind) {
        (Placement::Local, MaterializationPathKind::VmSnapshot) => 0,
        (Placement::Local, MaterializationPathKind::ReconstructionReplay) => 1,
        (Placement::Hosted, MaterializationPathKind::VmSnapshot) => 2,
        (Placement::Hosted, MaterializationPathKind::ReconstructionReplay) => 3,
        (Placement::Local, _) => 4,
        (Placement::Hosted, _) => 5,
    }
}

#[derive(Debug, Error)]
pub enum PlannerError {
    #[error("no acceptable Realization path; rejected candidates: {0:?}")]
    NoCandidate(Vec<RejectedPath>),
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use ato_adapter_api::{
        Actuator, ActuatorProvider, ActuatorRoute, AdapterError, AdapterRegistry,
        SupportedOperation, WorkspaceCapturePolicy,
    };
    use ato_computation::{OperationId, PortId, ProtocolId};
    use ato_materializer_api::{Materializer, MaterializerError, Realization};
    use ato_objects::{MemoryObjectStore, ObjectStore, RecordBodyV2, RecordEnvelopeV2};

    use super::*;

    struct FakeMaterializer {
        id: &'static str,
        kind: MaterializationPathKind,
        compatibility: Compatibility,
        target: ComputationRef,
        records: Vec<RecordEnvelopeV2>,
    }

    impl Materializer for FakeMaterializer {
        fn id(&self) -> &str {
            self.id
        }
        fn path_kind(&self) -> MaterializationPathKind {
            self.kind
        }
        fn restore_capability(&self) -> RestoreCapability {
            RestoreCapability::Supported
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
            Ok(self.target.clone())
        }
        fn compatibility(&self, _: &ContentRef, _: &MaterializerContext<'_>) -> Compatibility {
            self.compatibility
        }
        fn operation_records(
            &self,
            _: &ContentRef,
            _: &MaterializerContext<'_>,
        ) -> Result<Vec<RecordEnvelopeV2>, MaterializerError> {
            Ok(self.records.clone())
        }
        fn restore(
            &self,
            _: &ContentRef,
            _: &MaterializerContext<'_>,
        ) -> Result<Box<dyn Realization>, MaterializerError> {
            unreachable!()
        }
    }

    struct Provider {
        operations: Vec<SupportedOperation>,
    }

    impl ActuatorProvider for Provider {
        fn id(&self) -> &str {
            "browser.firefox@1"
        }
        fn supported_operations(&self) -> &[SupportedOperation] {
            &self.operations
        }
        fn validate_payload(
            &self,
            record: &RecordEnvelopeV2,
            context: &AdapterContext<'_>,
        ) -> Result<(), AdapterError> {
            context.objects.metadata(&record.payload_ref)?;
            Ok(())
        }
        fn provision(
            &self,
            _: &ActuatorRoute,
            _: &AdapterContext<'_>,
        ) -> Result<Box<dyn Actuator>, AdapterError> {
            unreachable!()
        }
    }

    fn target() -> ComputationRef {
        ComputationRef::parse(format!("blake3:{}", "c".repeat(64))).unwrap()
    }

    fn record(objects: &MemoryObjectStore) -> RecordEnvelopeV2 {
        RecordEnvelopeV2::seal(RecordBodyV2 {
            protocol_id: ProtocolId::parse("ato.browser@1").unwrap(),
            operation_id: OperationId::parse("click").unwrap(),
            port_id: PortId::parse("ui.main").unwrap(),
            payload_ref: objects.put(b"{}").unwrap(),
            payload_version: 1,
            required_features: BTreeSet::new(),
            recorded_by: None,
            stream: "browser.main".to_owned(),
            local_seq: 1,
            writer_order: 1,
            caused_by: Vec::new(),
            observed_at: "2030-01-01T00:00:00Z".to_owned(),
        })
        .unwrap()
    }

    fn context<'a>(
        objects: &'a MemoryObjectStore,
        adapters: &'a AdapterRegistry,
        policy: &'a WorkspaceCapturePolicy,
    ) -> MaterializerContext<'a> {
        MaterializerContext {
            objects,
            adapters,
            records: &[],
            records_v2: &[],
            replay_anchor: None,
            record_frontier_ref: None,
            workspace: Path::new("."),
            workspace_policy: policy,
            realization: None,
            contracts: &[],
            runner_capabilities: None,
        }
    }

    #[test]
    fn incompatible_vm_falls_back_to_provisionable_replay_by_explicit_rank() {
        let objects = MemoryObjectStore::default();
        let descriptor = objects.put(b"descriptor").unwrap();
        let target = target();
        let policy = WorkspaceCapturePolicy::secure_default();
        let adapters = AdapterRegistry::default();
        let mut materializers = MaterializerRegistry::default();
        materializers
            .register(Arc::new(FakeMaterializer {
                id: "example.vm@1",
                kind: MaterializationPathKind::VmSnapshot,
                compatibility: Compatibility::Incompatible,
                target: target.clone(),
                records: Vec::new(),
            }))
            .unwrap();
        materializers
            .register(Arc::new(FakeMaterializer {
                id: "example.replay@1",
                kind: MaterializationPathKind::ReconstructionReplay,
                compatibility: Compatibility::Compatible,
                target: target.clone(),
                records: vec![record(&objects)],
            }))
            .unwrap();
        let mut providers = ActuatorProviderRegistry::default();
        providers
            .register(Arc::new(Provider {
                operations: vec![
                    SupportedOperation::new("ato.browser@1", "click", 1, BTreeSet::new()).unwrap(),
                ],
            }))
            .unwrap();
        let verifiers = ContractVerifierRegistry::default();
        let planner_policy = PlannerPolicy::default();
        let planner = RealizationPlanner {
            target: &target,
            materializers: &materializers,
            actuator_providers: &providers,
            contract_verifiers: &verifiers,
            port_bindings: &[],
            policy: &planner_policy,
        };

        let plan = planner
            .plan(vec![
                MaterializationCandidate {
                    materializer_id: "example.replay@1".to_owned(),
                    descriptor_ref: descriptor.clone(),
                    environment: TargetEnvironment {
                        id: "local".to_owned(),
                        placement: Placement::Local,
                        trust_boundary: TrustBoundary::Local,
                    },
                    context: context(&objects, &adapters, &policy),
                },
                MaterializationCandidate {
                    materializer_id: "example.vm@1".to_owned(),
                    descriptor_ref: descriptor,
                    environment: TargetEnvironment {
                        id: "local".to_owned(),
                        placement: Placement::Local,
                        trust_boundary: TrustBoundary::Local,
                    },
                    context: context(&objects, &adapters, &policy),
                },
            ])
            .unwrap();

        assert_eq!(plan.candidates[0].materializer_id, "example.replay@1");
        assert_eq!(plan.rejected[0].materializer_id, "example.vm@1");
    }
}
