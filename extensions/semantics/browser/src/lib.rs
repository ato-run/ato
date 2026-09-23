//! Logical Browser interaction frontier. Chrome state remains physical.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ato_adapter_api::LiveOperation;
use ato_adapter_browser::{BROWSER_PROTOCOL_ID, decode_event, operation_for_event};
use ato_computation::{
    ComputationObject, ContentRef, OperationId, PortId, ProtocolId, ResolvedComputation, RoleId,
    SemanticsId,
};
use ato_kernel::{
    AcceptedOperation, Action, EvolutionError, KernelError, ProtocolError, ProtocolPayload,
    ProtocolSemantics, RunEvolutionAuthority, SemanticError, SemanticHost, SemanticStep, Semantics,
    TransitionOffer,
};
use ato_objects::{BundleError, ComputationReferences, ObjectLink, ObjectResolver};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;
use thiserror::Error;

pub const BROWSER_COMPUTATION_SEMANTICS_ID: &str = "ato.browser.computation@1";

/// Browser residuals contain no further object links. Registering this
/// extractor still makes the residual an explicit, validated part of a graph
/// closure rather than relying on an unknown-semantics fallback.
pub struct BrowserComputationReferences {
    id: SemanticsId,
}

impl Default for BrowserComputationReferences {
    fn default() -> Self {
        Self {
            id: SemanticsId::parse(BROWSER_COMPUTATION_SEMANTICS_ID)
                .expect("static Browser Semantics ID"),
        }
    }
}

impl ComputationReferences for BrowserComputationReferences {
    fn semantics(&self) -> &SemanticsId {
        &self.id
    }

    fn outgoing(
        &self,
        computation: &ResolvedComputation,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let metadata = objects.metadata(&computation.object().residual)?;
        let bytes = ato_objects::read_exact_object(
            objects,
            &computation.object().residual,
            metadata.size,
            64 * 1024,
        )?;
        decode_residual(&bytes).map_err(|error| {
            BundleError::Object(ato_objects::ObjectError::Storage(error.to_string()))
        })?;
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserResidualV2 {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_frontier: Option<String>,
    /// A physical Browser state object explicitly attached by a system
    /// checkpoint. This is a ContentRef, not a hash-derived ComputationRef.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_state_ref: Option<String>,
}

/// Compatibility name for v1 residual construction. New Browser-aware
/// Capsules must use `BrowserResidualV2 { version: 2, .. }`.
pub type BrowserResidualV1 = BrowserResidualV2;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrowserSemanticsError {
    #[error("invalid Browser residual: {0}")]
    Residual(String),
}

pub fn encode_residual(residual: &BrowserResidualV2) -> Result<Vec<u8>, BrowserSemanticsError> {
    validate_residual(residual)?;
    serde_jcs::to_vec(residual).map_err(|error| BrowserSemanticsError::Residual(error.to_string()))
}

pub fn decode_residual(bytes: &[u8]) -> Result<BrowserResidualV2, BrowserSemanticsError> {
    let residual: BrowserResidualV2 = serde_json::from_slice(bytes)
        .map_err(|error| BrowserSemanticsError::Residual(error.to_string()))?;
    if serde_jcs::to_vec(&residual)
        .map_err(|error| BrowserSemanticsError::Residual(error.to_string()))?
        != bytes
    {
        return Err(BrowserSemanticsError::Residual(
            "residual is not canonical JCS".to_owned(),
        ));
    }
    validate_residual(&residual)?;
    Ok(residual)
}

pub struct BrowserComputationSemantics {
    id: SemanticsId,
}

impl Default for BrowserComputationSemantics {
    fn default() -> Self {
        Self {
            id: SemanticsId::parse(BROWSER_COMPUTATION_SEMANTICS_ID)
                .expect("static Browser Semantics ID"),
        }
    }
}

impl Semantics for BrowserComputationSemantics {
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn validate(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost,
    ) -> Result<(), SemanticError> {
        residual_for(current, host)
            .map(|_| ())
            .map_err(|error| SemanticError::new(error.to_string()))
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        offer: &TransitionOffer,
        host: &dyn SemanticHost,
    ) -> Result<SemanticStep, SemanticError> {
        if matches!(offer.action, Action::Tau) {
            return checkpoint_step(current, offer, host);
        }
        let Action::Input { port, payload } = &offer.action else {
            return Err(SemanticError::new(
                "Browser Computation accepts Browser input or a checkpoint",
            ));
        };
        let definition = current
            .object()
            .boundary
            .get(port)
            .ok_or_else(|| SemanticError::new("Browser input names no boundary port"))?;
        if definition.protocol.as_str() != BROWSER_PROTOCOL_ID {
            return Err(SemanticError::new("Browser input must use ato.browser@1"));
        }
        let event = decode_event(payload.as_bytes())
            .map_err(|error| SemanticError::new(error.to_string()))?;
        let residual =
            residual_for(current, host).map_err(|error| SemanticError::new(error.to_string()))?;
        let transition = BrowserInteractionV1 {
            version: 1,
            prior_frontier: residual.interaction_frontier.as_deref(),
            protocol_id: BROWSER_PROTOCOL_ID,
            port_id: port.as_str(),
            operation: operation_for_event(&event),
            payload: event,
        };
        let frontier = format!(
            "blake3:{}",
            blake3::hash(
                &serde_jcs::to_vec(&transition)
                    .map_err(|error| SemanticError::new(error.to_string()))?
            )
            .to_hex()
        );
        let next = BrowserResidualV2 {
            interaction_frontier: Some(frontier),
            ..residual
        };
        let residual = host
            .put_object(
                &encode_residual(&next).map_err(|error| SemanticError::new(error.to_string()))?,
            )
            .map_err(|error| SemanticError::new(error.to_string()))?;
        Ok(SemanticStep {
            offer: offer.clone(),
            successor: ComputationObject {
                semantics: current.object().semantics.clone(),
                boundary: current.object().boundary.clone(),
                residual,
            },
        })
    }
}

#[derive(Serialize)]
struct BrowserInteractionV1<'a> {
    version: u32,
    prior_frontier: Option<&'a str>,
    protocol_id: &'a str,
    port_id: &'a str,
    operation: &'a str,
    payload: ato_adapter_browser::BrowserEvent,
}

pub struct BrowserProtocolSemantics {
    id: ProtocolId,
}

impl Default for BrowserProtocolSemantics {
    fn default() -> Self {
        Self {
            id: ProtocolId::parse(BROWSER_PROTOCOL_ID).expect("static Browser protocol ID"),
        }
    }
}

impl ProtocolSemantics for BrowserProtocolSemantics {
    fn id(&self) -> &ProtocolId {
        &self.id
    }
    fn roles_compatible(&self, left: &RoleId, right: &RoleId) -> Result<bool, ProtocolError> {
        Ok(BTreeSet::from([left.as_str(), right.as_str()])
            == BTreeSet::from(["server", "controller"]))
    }
    fn validate_input(
        &self,
        _role: &RoleId,
        payload: &ProtocolPayload,
    ) -> Result<(), ProtocolError> {
        decode_event(payload.as_bytes())
            .map(|_| ())
            .map_err(|error| ProtocolError::new(error.to_string()))
    }
    fn validate_output(
        &self,
        _role: &RoleId,
        _payload: &ProtocolPayload,
    ) -> Result<(), ProtocolError> {
        Err(ProtocolError::new("ato.browser@1 has no output operation"))
    }
}

/// Internal checkpoint which makes the Browser Materialization ContentRef an
/// explicit residual fact. It is neither an `ato.browser@1` operation nor a
/// Record. The successor remains a normal Kernel-sealed Computation.
pub fn checkpoint_offer(state_ref: ContentRef) -> TransitionOffer {
    TransitionOffer::selected(
        ato_kernel::ChoiceId::new(format!("ato.browser.checkpoint@1:{state_ref}")),
        Action::Tau,
    )
}

fn checkpoint_step(
    current: &ResolvedComputation,
    offer: &TransitionOffer,
    host: &dyn SemanticHost,
) -> Result<SemanticStep, SemanticError> {
    let choice = offer
        .choice
        .as_ref()
        .ok_or_else(|| SemanticError::new("Browser checkpoint requires an explicit choice"))?;
    let state_ref = choice
        .as_str()
        .strip_prefix("ato.browser.checkpoint@1:")
        .ok_or_else(|| SemanticError::new("Browser Tau transition is not a checkpoint"))?;
    ContentRef::parse(state_ref).map_err(|error| SemanticError::new(error.to_string()))?;
    let mut next =
        residual_for(current, host).map_err(|error| SemanticError::new(error.to_string()))?;
    if next.version != 2 {
        return Err(SemanticError::new(
            "Browser checkpoint requires residual version 2",
        ));
    }
    next.checkpoint_state_ref = Some(state_ref.to_owned());
    let residual = host
        .put_object(&encode_residual(&next).map_err(|error| SemanticError::new(error.to_string()))?)
        .map_err(|error| SemanticError::new(error.to_string()))?;
    Ok(SemanticStep {
        offer: offer.clone(),
        successor: ComputationObject {
            semantics: current.object().semantics.clone(),
            boundary: current.object().boundary.clone(),
            residual,
        },
    })
}

fn residual_for(
    current: &ResolvedComputation,
    host: &dyn SemanticHost,
) -> Result<BrowserResidualV2, KernelError> {
    let bytes = host.get_object(&current.object().residual, 64 * 1024)?;
    decode_residual(&bytes)
        .map_err(|error| KernelError::Semantic(SemanticError::new(error.to_string())))
}

fn validate_residual(residual: &BrowserResidualV2) -> Result<(), BrowserSemanticsError> {
    if !matches!(residual.version, 1 | 2) {
        return Err(BrowserSemanticsError::Residual(
            "unsupported Browser residual version".to_owned(),
        ));
    }
    if residual.version == 1 && residual.checkpoint_state_ref.is_some() {
        return Err(BrowserSemanticsError::Residual(
            "Browser residual v1 cannot contain a checkpoint".to_owned(),
        ));
    }
    if let Some(reference) = &residual.checkpoint_state_ref {
        ContentRef::parse(reference)
            .map_err(|error| BrowserSemanticsError::Residual(error.to_string()))?;
    }
    Ok(())
}

/// Physical Browser boundary used by hosted operation ingress. Implementations
/// send the canonical live operation to Chrome and return only after its ACK.
pub trait BrowserOperationActuator: Send + Sync {
    fn apply(
        &self,
        correlation_id: &str,
        realization_generation: Option<&str>,
        operation: &LiveOperation,
    ) -> Result<u64, String>;
}

/// Runner control-plane projection port. Its failure is intentionally kept
/// separate from physical Browser success.
pub trait BrowserHeadPersistence: Send + Sync {
    fn persist(&self, operation: &AcceptedBrowserOperation) -> Result<(), String>;
}

/// Non-persisting Record submission port. The caller decides whether a Record
/// Writer queue or a test probe receives the accepted candidate.
pub trait BrowserRecordSubmission: Send + Sync {
    fn submit(&self, operation: &AcceptedBrowserOperation) -> Result<(), String>;

    fn record_ref(&self, _operation_id: &str) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcceptedBrowserOperation {
    pub operation_id: String,
    pub event: ato_adapter_browser::BrowserEvent,
    pub transition: ato_kernel::Transition,
    pub run_seq: u64,
}

#[derive(Debug, Clone)]
struct PendingBrowserOperation {
    operation_id: String,
    event: ato_adapter_browser::BrowserEvent,
    realization_generation: Option<String>,
    settlement_order: u64,
}

/// One canonical hosted Browser operation path. It is not a Player: it takes a
/// live event, derives the logical transition, obtains a physical ACK, commits
/// the head, persists the runner projection, then submits exactly one Record.
pub struct BrowserOperationIngress<A, P, R> {
    authority: Arc<RunEvolutionAuthority>,
    browser_port: PortId,
    actuator: A,
    persistence: P,
    records: R,
    lifecycle_gate: RwLock<()>,
    commit_gate: Mutex<()>,
    next_settlement_order: Mutex<u64>,
    settlement_changed: Condvar,
    in_flight_operations: Mutex<BTreeMap<String, Vec<u8>>>,
    physically_applied_operations: Mutex<BTreeMap<String, PendingBrowserOperation>>,
    physical_commit_failure: Mutex<Option<String>>,
    pending_operation: Mutex<Option<PendingBrowserOperation>>,
    accepted_operations: Mutex<AcceptedOperationCache>,
    next_operation_id: AtomicU64,
}

const ACCEPTED_OPERATION_CACHE_LIMIT: usize = 1024;

/// Bounded live-run idempotency cache. It is deliberately operational state:
/// operation IDs and client retries never enter the Browser residual.
#[derive(Default)]
struct AcceptedOperationCache {
    by_id: BTreeMap<String, (Vec<u8>, AcceptedOperation)>,
    insertion_order: VecDeque<String>,
}

impl AcceptedOperationCache {
    fn get(
        &self,
        operation_id: &str,
        payload: &[u8],
    ) -> Result<Option<AcceptedOperation>, EvolutionError> {
        let Some((known_payload, accepted)) = self.by_id.get(operation_id) else {
            return Ok(None);
        };
        if known_payload != payload {
            return Err(EvolutionError::Apply(
                "Browser operation id was reused with a different payload".to_owned(),
            ));
        }
        Ok(Some(accepted.clone()))
    }

    fn insert(&mut self, operation_id: String, payload: Vec<u8>, accepted: AcceptedOperation) {
        if self.by_id.contains_key(&operation_id) {
            return;
        }
        self.insertion_order.push_back(operation_id.clone());
        self.by_id.insert(operation_id, (payload, accepted));
        while self.insertion_order.len() > ACCEPTED_OPERATION_CACHE_LIMIT {
            if let Some(expired) = self.insertion_order.pop_front() {
                self.by_id.remove(&expired);
            }
        }
    }
}

impl<A, P, R> BrowserOperationIngress<A, P, R>
where
    A: BrowserOperationActuator,
    P: BrowserHeadPersistence,
    R: BrowserRecordSubmission,
{
    pub fn new(
        authority: Arc<RunEvolutionAuthority>,
        browser_port: PortId,
        actuator: A,
        persistence: P,
        records: R,
    ) -> Self {
        Self {
            authority,
            browser_port,
            actuator,
            persistence,
            records,
            lifecycle_gate: RwLock::new(()),
            commit_gate: Mutex::new(()),
            next_settlement_order: Mutex::new(1),
            settlement_changed: Condvar::new(),
            in_flight_operations: Mutex::new(BTreeMap::new()),
            physically_applied_operations: Mutex::new(BTreeMap::new()),
            physical_commit_failure: Mutex::new(None),
            pending_operation: Mutex::new(None),
            accepted_operations: Mutex::new(AcceptedOperationCache::default()),
            next_operation_id: AtomicU64::new(0),
        }
    }

    pub fn accept(
        &self,
        event: ato_adapter_browser::BrowserEvent,
    ) -> Result<AcceptedOperation, EvolutionError> {
        let operation_id = format!(
            "browser-{}",
            self.next_operation_id.fetch_add(1, Ordering::Relaxed) + 1
        );
        self.accept_with_operation_id(operation_id, event)
    }

    pub fn accept_with_operation_id(
        &self,
        operation_id: String,
        event: ato_adapter_browser::BrowserEvent,
    ) -> Result<AcceptedOperation, EvolutionError> {
        self.accept_with_operation_context(operation_id, event, None)
    }

    /// Accepts an operation bound to one opaque physical Browser document
    /// incarnation. The generation is deliberately excluded from the
    /// semantic event/Record and is used only by the Adapter as a stale-input
    /// fence across navigation.
    pub fn accept_with_operation_context(
        &self,
        operation_id: String,
        event: ato_adapter_browser::BrowserEvent,
        realization_generation: Option<String>,
    ) -> Result<AcceptedOperation, EvolutionError> {
        if !valid_operation_id(&operation_id) {
            return Err(EvolutionError::Apply(
                "invalid Browser operation id".to_owned(),
            ));
        }
        let _lifecycle = self
            .lifecycle_gate
            .read()
            .expect("Browser lifecycle gate poisoned");
        if self
            .physical_commit_failure
            .lock()
            .expect("Browser physical commit failure mutex poisoned")
            .as_deref()
            .is_some_and(|failed| failed != operation_id)
        {
            return Err(EvolutionError::Apply("operation_in_flight".to_owned()));
        }
        let payload = ato_adapter_browser::encode_event(&event)
            .map_err(|error| EvolutionError::Apply(error.to_string()))?;
        let cache_payload = operation_cache_key(&payload, realization_generation.as_deref());
        if let Some(accepted) = self
            .accepted_operations
            .lock()
            .expect("Browser accepted operation cache mutex poisoned")
            .get(&operation_id, &cache_payload)?
        {
            return Ok(accepted);
        }
        let operation = LiveOperation {
            protocol_id: ProtocolId::parse(BROWSER_PROTOCOL_ID)
                .expect("static Browser Protocol ID"),
            operation_id: OperationId::parse(operation_for_event(&event))
                .expect("static Browser operation ID"),
            port_id: self.browser_port.clone(),
            payload: payload.clone(),
        };
        let offer = TransitionOffer::external_input(
            self.browser_port.clone(),
            ProtocolPayload::from(payload.clone()),
        );
        {
            let mut in_flight = self
                .in_flight_operations
                .lock()
                .expect("Browser in-flight operation mutex poisoned");
            if let Some(known) = in_flight.get(&operation_id) {
                return if known == &cache_payload {
                    Err(EvolutionError::Apply("operation_in_flight".to_owned()))
                } else {
                    Err(EvolutionError::Apply(
                        "Browser operation id was reused with a different payload".to_owned(),
                    ))
                };
            }
            in_flight.insert(operation_id.clone(), cache_payload.clone());
        }
        let physically_applied = self
            .physically_applied_operations
            .lock()
            .expect("Browser applied operation mutex poisoned")
            .get(&operation_id)
            .cloned();
        let physical_result = match physically_applied {
            Some(known)
                if known.event == event
                    && known.realization_generation == realization_generation =>
            {
                Ok(known.settlement_order)
            }
            Some(_) => Err(EvolutionError::Apply(
                "Browser operation id was reused with a different payload".to_owned(),
            )),
            None => {
                // Browser input semantics accept every already-validated
                // ato.browser@1 event at every Browser frontier. Validate at
                // the current head before allowing the physical handler to run
                // without the commit sequencer.
                self.authority.validate_offer(&offer).and_then(|()| {
                    self.actuator
                        .apply(&operation_id, realization_generation.as_deref(), &operation)
                        .map_err(EvolutionError::Apply)
                })
            }
        };
        let settlement_order = match physical_result {
            Ok(order) => order,
            Err(error) => {
                self.in_flight_operations
                    .lock()
                    .expect("Browser in-flight operation mutex poisoned")
                    .remove(&operation_id);
                return Err(error);
            }
        };
        self.physically_applied_operations
            .lock()
            .expect("Browser applied operation mutex poisoned")
            .entry(operation_id.clone())
            .or_insert_with(|| PendingBrowserOperation {
                operation_id: operation_id.clone(),
                event: event.clone(),
                realization_generation: realization_generation.clone(),
                settlement_order,
            });

        if !self.wait_for_settlement_turn(settlement_order) {
            self.finish_operation(&operation_id, false);
            return Err(EvolutionError::Apply("operation_in_flight".to_owned()));
        }

        // Physical handlers may overlap, but head derivation/commit remains a
        // short sequencer. Runner sequence follows the authoritative
        // post-settlement commit race, and each transition is derived from the
        // latest committed head.
        let _commit = self
            .commit_gate
            .lock()
            .expect("Browser commit gate poisoned");
        if self
            .physical_commit_failure
            .lock()
            .expect("Browser physical commit failure mutex poisoned")
            .as_deref()
            .is_some_and(|failed| failed != operation_id)
        {
            // This operation has physically settled, but cannot cross a prior
            // uncommitted physical effect. Keep its physical tombstone, drop
            // only the live owner, and let the controller retry after the
            // earlier operation repairs/fences the Run.
            self.finish_operation(&operation_id, false);
            return Err(EvolutionError::Apply("operation_in_flight".to_owned()));
        }
        if self.authority.pending_persistence().is_some()
            && let Err(error) = self.retry_pending_persistence_inner()
        {
            self.finish_operation(&operation_id, false);
            return Err(error);
        }
        if let Some(accepted) = self
            .accepted_operations
            .lock()
            .expect("Browser accepted operation cache mutex poisoned")
            .get(&operation_id, &cache_payload)?
        {
            self.finish_operation(&operation_id, true);
            return Ok(accepted);
        }
        *self
            .pending_operation
            .lock()
            .expect("Browser pending operation mutex poisoned") = Some(PendingBrowserOperation {
            operation_id: operation_id.clone(),
            event: event.clone(),
            realization_generation: realization_generation.clone(),
            settlement_order,
        });
        let result = self.authority.accept(
            &offer,
            || Ok(()),
            |pending| {
                self.persistence.persist(&AcceptedBrowserOperation {
                    operation_id: operation_id.clone(),
                    event: event.clone(),
                    transition: pending.transition.clone(),
                    run_seq: pending.run_seq,
                })
            },
            |transition, run_seq| {
                self.records.submit(&AcceptedBrowserOperation {
                    operation_id: operation_id.clone(),
                    event: event.clone(),
                    transition: transition.clone(),
                    run_seq,
                })
            },
        );
        if let Ok(accepted) = &result {
            self.accepted_operations
                .lock()
                .expect("Browser accepted operation cache mutex poisoned")
                .insert(operation_id.clone(), cache_payload, accepted.clone());
        }
        if !matches!(result, Err(EvolutionError::Persist(_))) {
            *self
                .pending_operation
                .lock()
                .expect("Browser pending operation mutex poisoned") = None;
        }
        if result.is_err() && !matches!(result, Err(EvolutionError::Persist(_))) {
            *self
                .physical_commit_failure
                .lock()
                .expect("Browser physical commit failure mutex poisoned") =
                Some(operation_id.clone());
        }
        match result {
            Ok(accepted) => {
                self.advance_settlement_order(settlement_order)?;
                self.finish_operation(&operation_id, true);
                Ok(accepted)
            }
            Err(EvolutionError::Persist(error)) => {
                self.finish_operation(&operation_id, false);
                Err(EvolutionError::Persist(error))
            }
            Err(_) => {
                // The physical effect already settled. Preserve its event and
                // ticket for same-ID commit repair and keep later tickets
                // fenced; a terminal failed receipt would make repair
                // impossible and contradict the physical Browser.
                self.finish_operation(&operation_id, false);
                Err(EvolutionError::Apply("operation_in_flight".to_owned()))
            }
        }
    }

    pub fn retry_pending_persistence(&self) -> Result<BrowserPersistenceRetry, EvolutionError> {
        let _lifecycle = self
            .lifecycle_gate
            .read()
            .expect("Browser lifecycle gate poisoned");
        let _commit = self
            .commit_gate
            .lock()
            .expect("Browser commit gate poisoned");
        self.retry_pending_persistence_inner()
    }

    pub fn operation_retry_stage(&self, operation_id: &str) -> BrowserOperationRetryStage {
        if self
            .physically_applied_operations
            .lock()
            .expect("Browser applied operation mutex poisoned")
            .contains_key(operation_id)
        {
            BrowserOperationRetryStage::PhysicallyAppliedPendingCommit
        } else {
            BrowserOperationRetryStage::BeforeApply
        }
    }

    pub fn record_ref(&self, operation_id: &str) -> Option<String> {
        self.records.record_ref(operation_id)
    }

    fn retry_pending_persistence_inner(&self) -> Result<BrowserPersistenceRetry, EvolutionError> {
        let operation = self
            .pending_operation
            .lock()
            .expect("Browser pending operation mutex poisoned")
            .clone()
            .ok_or_else(|| EvolutionError::Apply("no Browser operation is pending".to_owned()))?;
        let pending = self.authority.pending_persistence().ok_or_else(|| {
            EvolutionError::Apply(
                "Browser operation context is pending without a head transition".to_owned(),
            )
        })?;
        let result = self.authority.retry_pending_persistence(|pending| {
            self.persistence.persist(&AcceptedBrowserOperation {
                operation_id: operation.operation_id.clone(),
                event: operation.event.clone(),
                transition: pending.transition.clone(),
                run_seq: pending.run_seq,
            })
        });
        let retried = result?;
        if !retried {
            return Ok(BrowserPersistenceRetry {
                persisted: false,
                record_error: None,
                accepted: None,
                operation_id: None,
            });
        }
        let accepted_operation = AcceptedBrowserOperation {
            operation_id: operation.operation_id.clone(),
            event: operation.event.clone(),
            transition: pending.transition.clone(),
            run_seq: pending.run_seq,
        };
        let record_error = self.records.submit(&accepted_operation).err();
        let accepted = AcceptedOperation {
            transition: pending.transition.clone(),
            run_seq: pending.run_seq,
            record_error: record_error.clone(),
        };
        let payload = ato_adapter_browser::encode_event(&operation.event)
            .map_err(|error| EvolutionError::Apply(error.to_string()))?;
        let cache_payload =
            operation_cache_key(&payload, operation.realization_generation.as_deref());
        self.accepted_operations
            .lock()
            .expect("Browser accepted operation cache mutex poisoned")
            .insert(
                operation.operation_id.clone(),
                cache_payload,
                accepted.clone(),
            );
        *self
            .pending_operation
            .lock()
            .expect("Browser pending operation mutex poisoned") = None;
        self.advance_settlement_order(operation.settlement_order)?;
        self.finish_operation(&operation.operation_id, true);
        Ok(BrowserPersistenceRetry {
            persisted: true,
            record_error,
            accepted: Some(accepted),
            operation_id: Some(operation.operation_id),
        })
    }

    fn finish_operation(&self, operation_id: &str, committed: bool) {
        self.in_flight_operations
            .lock()
            .expect("Browser in-flight operation mutex poisoned")
            .remove(operation_id);
        if committed {
            self.physically_applied_operations
                .lock()
                .expect("Browser applied operation mutex poisoned")
                .remove(operation_id);
            let mut failure = self
                .physical_commit_failure
                .lock()
                .expect("Browser physical commit failure mutex poisoned");
            if failure.as_deref() == Some(operation_id) {
                *failure = None;
            }
        }
    }

    fn wait_for_settlement_turn(&self, settlement_order: u64) -> bool {
        let next = self
            .next_settlement_order
            .lock()
            .expect("Browser settlement sequencer mutex poisoned");
        let (next, timeout) = self
            .settlement_changed
            .wait_timeout_while(next, Duration::from_millis(100), |next| {
                *next < settlement_order
            })
            .expect("Browser settlement sequencer mutex poisoned");
        !timeout.timed_out() || *next >= settlement_order
    }

    fn advance_settlement_order(&self, settlement_order: u64) -> Result<(), EvolutionError> {
        let mut next = self
            .next_settlement_order
            .lock()
            .expect("Browser settlement sequencer mutex poisoned");
        if *next > settlement_order {
            return Ok(());
        }
        if *next != settlement_order {
            return Err(EvolutionError::Apply(
                "Browser settlement ordering evidence is discontinuous".to_owned(),
            ));
        }
        *next = next.checked_add(1).ok_or_else(|| {
            EvolutionError::Apply("Browser settlement order exhausted".to_owned())
        })?;
        self.settlement_changed.notify_all();
        Ok(())
    }

    pub fn freeze(&self) -> Result<ato_kernel::RunHeadSnapshot, EvolutionError> {
        let _lifecycle = self
            .lifecycle_gate
            .write()
            .expect("Browser lifecycle gate poisoned");
        let _commit = self
            .commit_gate
            .lock()
            .expect("Browser commit gate poisoned");
        self.authority.freeze()
    }

    pub fn unfreeze(&self) {
        let _lifecycle = self
            .lifecycle_gate
            .write()
            .expect("Browser lifecycle gate poisoned");
        self.authority.unfreeze();
    }
}

/// Result of retrying the single control-plane write which followed a physical
/// Browser ACK. A Record failure never reopens that accepted transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserPersistenceRetry {
    pub persisted: bool,
    pub record_error: Option<String>,
    pub accepted: Option<AcceptedOperation>,
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserOperationRetryStage {
    BeforeApply,
    PhysicallyAppliedPendingCommit,
}

fn valid_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
}

fn operation_cache_key(payload: &[u8], realization_generation: Option<&str>) -> Vec<u8> {
    let mut key = Vec::with_capacity(
        payload.len() + realization_generation.map_or(1, |value| value.len() + 1),
    );
    key.extend_from_slice(payload);
    // Canonical JSON never contains a literal NUL byte, so this boundary is
    // unambiguous without placing the realization generation in the semantic
    // protocol payload.
    key.push(0);
    if let Some(generation) = realization_generation {
        key.extend_from_slice(generation.as_bytes());
    }
    key
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};

    use ato_adapter_browser::{BrowserEvent, KeyboardKind, Modifiers};
    use ato_computation::{Boundary, PortDef};
    use ato_kernel::Kernel;
    use ato_objects::{
        MemoryObjectStore, ObjectError, ObjectMetadata, ObjectResolver, ObjectStore,
    };

    use super::*;

    #[derive(Default)]
    struct Actuator {
        calls: Mutex<Vec<LiveOperation>>,
        reject: AtomicBool,
        next_settlement: AtomicU64,
    }
    impl BrowserOperationActuator for Actuator {
        fn apply(
            &self,
            _correlation_id: &str,
            _realization_generation: Option<&str>,
            operation: &LiveOperation,
        ) -> Result<u64, String> {
            if self.reject.load(Ordering::Acquire) {
                return Err("Browser ACK timeout".to_owned());
            }
            self.calls.lock().unwrap().push(operation.clone());
            Ok(self.next_settlement.fetch_add(1, Ordering::SeqCst) + 1)
        }
    }

    #[derive(Default)]
    struct Persistence(Mutex<Vec<AcceptedBrowserOperation>>);
    impl BrowserHeadPersistence for Persistence {
        fn persist(&self, operation: &AcceptedBrowserOperation) -> Result<(), String> {
            self.0.lock().unwrap().push(operation.clone());
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct Records(Arc<Mutex<Vec<AcceptedBrowserOperation>>>);
    impl BrowserRecordSubmission for Records {
        fn submit(&self, operation: &AcceptedBrowserOperation) -> Result<(), String> {
            self.0.lock().unwrap().push(operation.clone());
            Ok(())
        }
    }

    struct FailingRecords;

    impl BrowserRecordSubmission for FailingRecords {
        fn submit(&self, _operation: &AcceptedBrowserOperation) -> Result<(), String> {
            Err("Record Writer is unavailable".to_owned())
        }
    }

    #[derive(Default)]
    struct FailOncePersistence {
        fail: AtomicBool,
        received: Mutex<Vec<AcceptedBrowserOperation>>,
    }

    struct ParallelActuator {
        slow_started: Sender<()>,
        slow_release: Mutex<Receiver<()>>,
        correlations: Mutex<Vec<String>>,
        next_settlement: AtomicU64,
    }

    struct ReverseWakeActuator {
        first_ticket_assigned: Sender<()>,
        second_ticket_assigned: Sender<()>,
        release_first_waiter: Mutex<Receiver<()>>,
        next_settlement: AtomicU64,
    }

    struct FailNextInsertStore {
        inner: MemoryObjectStore,
        fail_next: AtomicBool,
    }

    impl ObjectResolver for FailNextInsertStore {
        fn metadata(&self, reference: &ContentRef) -> Result<ObjectMetadata, ObjectError> {
            self.inner.metadata(reference)
        }

        fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send + '_>, ObjectError> {
            self.inner.open(reference)
        }
    }

    impl ObjectStore for FailNextInsertStore {
        fn insert(&self, reference: &ContentRef, bytes: &[u8]) -> Result<(), ObjectError> {
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(ObjectError::Storage("injected CAS failure".to_owned()));
            }
            self.inner.insert(reference, bytes)
        }
    }

    struct FailCommitOnceActuator {
        store: Arc<FailNextInsertStore>,
        calls: AtomicU64,
    }

    impl BrowserOperationActuator for FailCommitOnceActuator {
        fn apply(
            &self,
            _correlation_id: &str,
            _realization_generation: Option<&str>,
            _operation: &LiveOperation,
        ) -> Result<u64, String> {
            let order = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if order == 1 {
                self.store.fail_next.store(true, Ordering::SeqCst);
            }
            Ok(order)
        }
    }

    impl BrowserOperationActuator for ReverseWakeActuator {
        fn apply(
            &self,
            _correlation_id: &str,
            _realization_generation: Option<&str>,
            _operation: &LiveOperation,
        ) -> Result<u64, String> {
            let order = self.next_settlement.fetch_add(1, Ordering::SeqCst) + 1;
            if order == 1 {
                let _ = self.first_ticket_assigned.send(());
                self.release_first_waiter
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .map_err(|error| error.to_string())?;
            } else {
                let _ = self.second_ticket_assigned.send(());
            }
            Ok(order)
        }
    }

    impl BrowserOperationActuator for ParallelActuator {
        fn apply(
            &self,
            correlation_id: &str,
            _realization_generation: Option<&str>,
            _operation: &LiveOperation,
        ) -> Result<u64, String> {
            self.correlations
                .lock()
                .unwrap()
                .push(correlation_id.to_owned());
            if correlation_id == "actor-a-slow" {
                let _ = self.slow_started.send(());
                self.slow_release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .map_err(|error| error.to_string())?;
            }
            Ok(self.next_settlement.fetch_add(1, Ordering::SeqCst) + 1)
        }
    }

    impl FailOncePersistence {
        fn failing() -> Self {
            Self {
                fail: AtomicBool::new(true),
                received: Mutex::new(Vec::new()),
            }
        }
    }

    impl BrowserHeadPersistence for FailOncePersistence {
        fn persist(&self, operation: &AcceptedBrowserOperation) -> Result<(), String> {
            self.received.lock().unwrap().push(operation.clone());
            if self.fail.swap(false, Ordering::SeqCst) {
                Err("temporary API outage".to_owned())
            } else {
                Ok(())
            }
        }
    }

    fn authority() -> Arc<RunEvolutionAuthority> {
        let objects = Arc::new(MemoryObjectStore::default());
        let mut kernel = Kernel::new(objects.clone());
        kernel
            .register(Arc::new(BrowserComputationSemantics::default()))
            .unwrap();
        kernel
            .register_protocol(Arc::new(BrowserProtocolSemantics::default()))
            .unwrap();
        let residual = objects
            .put(
                &encode_residual(&BrowserResidualV1 {
                    version: 1,
                    interaction_frontier: None,
                    checkpoint_state_ref: None,
                })
                .unwrap(),
            )
            .unwrap();
        let root = kernel
            .seal(&ComputationObject {
                semantics: SemanticsId::parse(BROWSER_COMPUTATION_SEMANTICS_ID).unwrap(),
                boundary: Boundary::from([(
                    PortId::parse("browser").unwrap(),
                    PortDef {
                        protocol: ProtocolId::parse(BROWSER_PROTOCOL_ID).unwrap(),
                        role: RoleId::parse("controller").unwrap(),
                    },
                )]),
                residual,
            })
            .unwrap();
        Arc::new(RunEvolutionAuthority::new(kernel, root))
    }

    fn checkpoint_authority() -> (Arc<RunEvolutionAuthority>, Arc<MemoryObjectStore>) {
        let objects = Arc::new(MemoryObjectStore::default());
        let mut kernel = Kernel::new(objects.clone());
        kernel
            .register(Arc::new(BrowserComputationSemantics::default()))
            .unwrap();
        kernel
            .register_protocol(Arc::new(BrowserProtocolSemantics::default()))
            .unwrap();
        let residual = objects
            .put(
                &encode_residual(&BrowserResidualV2 {
                    version: 2,
                    interaction_frontier: None,
                    checkpoint_state_ref: None,
                })
                .unwrap(),
            )
            .unwrap();
        let root = kernel
            .seal(&ComputationObject {
                semantics: SemanticsId::parse(BROWSER_COMPUTATION_SEMANTICS_ID).unwrap(),
                boundary: Boundary::from([(
                    PortId::parse("browser").unwrap(),
                    PortDef {
                        protocol: ProtocolId::parse(BROWSER_PROTOCOL_ID).unwrap(),
                        role: RoleId::parse("controller").unwrap(),
                    },
                )]),
                residual,
            })
            .unwrap();
        (Arc::new(RunEvolutionAuthority::new(kernel, root)), objects)
    }

    fn authority_with_failing_store() -> (Arc<RunEvolutionAuthority>, Arc<FailNextInsertStore>) {
        let objects = Arc::new(FailNextInsertStore {
            inner: MemoryObjectStore::default(),
            fail_next: AtomicBool::new(false),
        });
        let mut kernel = Kernel::new(objects.clone());
        kernel
            .register(Arc::new(BrowserComputationSemantics::default()))
            .unwrap();
        kernel
            .register_protocol(Arc::new(BrowserProtocolSemantics::default()))
            .unwrap();
        let residual = objects
            .put(
                &encode_residual(&BrowserResidualV1 {
                    version: 1,
                    interaction_frontier: None,
                    checkpoint_state_ref: None,
                })
                .unwrap(),
            )
            .unwrap();
        let root = kernel
            .seal(&ComputationObject {
                semantics: SemanticsId::parse(BROWSER_COMPUTATION_SEMANTICS_ID).unwrap(),
                boundary: Boundary::from([(
                    PortId::parse("browser").unwrap(),
                    PortDef {
                        protocol: ProtocolId::parse(BROWSER_PROTOCOL_ID).unwrap(),
                        role: RoleId::parse("controller").unwrap(),
                    },
                )]),
                residual,
            })
            .unwrap();
        (Arc::new(RunEvolutionAuthority::new(kernel, root)), objects)
    }

    fn key() -> BrowserEvent {
        BrowserEvent::Keyboard {
            kind: KeyboardKind::KeyDown,
            code: "ArrowRight".to_owned(),
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn accepted_browser_operations_share_one_head_chain_and_one_record_each() {
        let authority = authority();
        let persistence = Persistence::default();
        let records = Records::default();
        let submitted = Arc::clone(&records.0);
        let ingress = BrowserOperationIngress::new(
            authority.clone(),
            PortId::parse("browser").unwrap(),
            Actuator::default(),
            persistence,
            records,
        );
        let first = ingress.accept(key()).unwrap();
        let second = ingress
            .accept(BrowserEvent::Click {
                x_normalized: 0.5,
                y_normalized: 0.5,
                button: 0,
            })
            .unwrap();
        assert_eq!(first.run_seq, 1);
        assert_eq!(second.run_seq, 2);
        assert_eq!(authority.current_head().head, second.transition.to);
        assert_eq!(authority.current_head().run_seq, 2);
        assert_ne!(first.transition.to, second.transition.to);
        assert_eq!(submitted.lock().unwrap().len(), 2);
    }

    #[test]
    fn rejected_browser_ack_does_not_advance_or_record() {
        let authority = authority();
        let before = authority.current_head();
        let ingress = BrowserOperationIngress::new(
            authority.clone(),
            PortId::parse("browser").unwrap(),
            Actuator {
                calls: Mutex::new(Vec::new()),
                reject: AtomicBool::new(true),
                next_settlement: AtomicU64::new(0),
            },
            Persistence::default(),
            Records::default(),
        );
        assert!(matches!(
            ingress.accept(key()),
            Err(EvolutionError::Apply(_))
        ));
        assert_eq!(authority.current_head(), before);
    }

    #[test]
    fn persistence_retry_keeps_the_same_operation_context_and_submits_one_record() {
        let authority = authority();
        let persistence = FailOncePersistence::failing();
        let records = Records::default();
        let ingress = BrowserOperationIngress::new(
            authority.clone(),
            PortId::parse("browser").unwrap(),
            Actuator::default(),
            persistence,
            records,
        );
        assert!(matches!(
            ingress.accept_with_operation_id("op-browser-1".to_owned(), key()),
            Err(EvolutionError::Persist(_))
        ));
        assert!(matches!(
            ingress.accept(key()),
            Err(EvolutionError::PersistencePending(1))
        ));

        let retry = ingress.retry_pending_persistence().unwrap();
        assert!(retry.persisted);
        assert_eq!(retry.record_error, None);
        assert_eq!(retry.operation_id.as_deref(), Some("op-browser-1"));
        assert_eq!(retry.accepted.as_ref().map(|value| value.run_seq), Some(1));
        assert_eq!(ingress.actuator.calls.lock().unwrap().len(), 1);
        let accepted = ingress.accept(key()).unwrap();
        assert_eq!(accepted.run_seq, 2);
    }

    #[test]
    fn different_actor_browser_handlers_overlap_and_commit_in_settlement_order() {
        let authority = authority();
        let (slow_started, started) = mpsc::channel();
        let (release, slow_release) = mpsc::channel();
        let ingress = Arc::new(BrowserOperationIngress::new(
            authority.clone(),
            PortId::parse("browser").unwrap(),
            ParallelActuator {
                slow_started,
                slow_release: Mutex::new(slow_release),
                correlations: Mutex::new(Vec::new()),
                next_settlement: AtomicU64::new(0),
            },
            Persistence::default(),
            Records::default(),
        ));
        let slow_ingress = Arc::clone(&ingress);
        let slow = std::thread::spawn(move || {
            slow_ingress.accept_with_operation_id("actor-a-slow".to_owned(), key())
        });
        started
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("slow physical handler should start");

        let fast = ingress
            .accept_with_operation_id(
                "actor-b-human".to_owned(),
                BrowserEvent::Click {
                    x_normalized: 0.5,
                    y_normalized: 0.5,
                    button: 0,
                },
            )
            .expect("different Actor must not wait for slow WebMCP settlement");
        assert_eq!(fast.run_seq, 1);
        release.send(()).expect("release slow handler");
        let slow = slow.join().expect("slow thread").expect("slow acceptance");
        assert_eq!(slow.run_seq, 2);
        assert_eq!(authority.current_head().run_seq, 2);
        assert_eq!(
            ingress.actuator.correlations.lock().unwrap().as_slice(),
            ["actor-a-slow", "actor-b-human"]
        );
    }

    #[test]
    fn ack_ticket_order_survives_reverse_waiter_scheduling() {
        let authority = authority();
        let (first_ticket_assigned, first_assigned) = mpsc::channel();
        let (second_ticket_assigned, second_assigned) = mpsc::channel();
        let (release_first, release_first_waiter) = mpsc::channel();
        let ingress = Arc::new(BrowserOperationIngress::new(
            authority,
            PortId::parse("browser").unwrap(),
            ReverseWakeActuator {
                first_ticket_assigned,
                second_ticket_assigned,
                release_first_waiter: Mutex::new(release_first_waiter),
                next_settlement: AtomicU64::new(0),
            },
            Persistence::default(),
            Records::default(),
        ));
        let first_ingress = Arc::clone(&ingress);
        let first = std::thread::spawn(move || {
            first_ingress.accept_with_operation_id("ticket-first".to_owned(), key())
        });
        first_assigned
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("first ACK demux ticket");
        let second_ingress = Arc::clone(&ingress);
        let second = std::thread::spawn(move || {
            second_ingress.accept_with_operation_id(
                "ticket-second".to_owned(),
                BrowserEvent::Click {
                    x_normalized: 0.5,
                    y_normalized: 0.5,
                    button: 0,
                },
            )
        });
        second_assigned
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("second ACK demux ticket");
        release_first.send(()).expect("wake first ticket waiter");

        let first = first.join().expect("first thread").expect("first receipt");
        let second = second
            .join()
            .expect("second thread")
            .expect("second receipt");
        assert_eq!(first.run_seq, 1);
        assert_eq!(second.run_seq, 2);
    }

    #[test]
    fn post_physical_commit_failure_repairs_same_id_without_reapply_or_failed_terminal() {
        let (authority, store) = authority_with_failing_store();
        let records = Records::default();
        let submitted = Arc::clone(&records.0);
        let ingress = BrowserOperationIngress::new(
            authority.clone(),
            PortId::parse("browser").unwrap(),
            FailCommitOnceActuator {
                store,
                calls: AtomicU64::new(0),
            },
            Persistence::default(),
            records,
        );
        assert!(matches!(
            ingress.accept_with_operation_id("physical-repair".to_owned(), key()),
            Err(EvolutionError::Apply(ref message)) if message == "operation_in_flight"
        ));
        assert_eq!(
            ingress.operation_retry_stage("physical-repair"),
            BrowserOperationRetryStage::PhysicallyAppliedPendingCommit
        );
        assert!(matches!(
            ingress.accept_with_operation_id(
                "later-operation".to_owned(),
                BrowserEvent::Click {
                    x_normalized: 0.5,
                    y_normalized: 0.5,
                    button: 0,
                },
            ),
            Err(EvolutionError::Apply(ref message)) if message == "operation_in_flight"
        ));
        assert_eq!(ingress.actuator.calls.load(Ordering::SeqCst), 1);

        let repaired = ingress
            .accept_with_operation_id("physical-repair".to_owned(), key())
            .expect("same operation should repair the logical commit");
        assert_eq!(repaired.run_seq, 1);
        assert_eq!(ingress.actuator.calls.load(Ordering::SeqCst), 1);
        let later = ingress
            .accept_with_operation_id(
                "later-operation".to_owned(),
                BrowserEvent::Click {
                    x_normalized: 0.5,
                    y_normalized: 0.5,
                    button: 0,
                },
            )
            .expect("later operation after repair");
        assert_eq!(later.run_seq, 2);
        assert_eq!(ingress.actuator.calls.load(Ordering::SeqCst), 2);
        assert_eq!(submitted.lock().unwrap().len(), 2);
        assert_eq!(authority.current_head().run_seq, 2);
    }

    #[test]
    fn duplicate_operation_id_returns_the_first_acceptance_without_reapplying() {
        let authority = authority();
        let records = Records::default();
        let submitted = Arc::clone(&records.0);
        let ingress = BrowserOperationIngress::new(
            authority.clone(),
            PortId::parse("browser").unwrap(),
            Actuator::default(),
            Persistence::default(),
            records,
        );
        let first = ingress
            .accept_with_operation_id("op-reconnect-1".to_owned(), key())
            .unwrap();
        let retry = ingress
            .accept_with_operation_id("op-reconnect-1".to_owned(), key())
            .unwrap();
        assert_eq!(retry, first);
        assert_eq!(authority.current_head().run_seq, 1);
        assert_eq!(submitted.lock().unwrap().len(), 1);
        assert!(matches!(
            ingress.accept_with_operation_id(
                "op-reconnect-1".to_owned(),
                BrowserEvent::Click {
                    x_normalized: 0.5,
                    y_normalized: 0.5,
                    button: 0,
                },
            ),
            Err(EvolutionError::Apply(_))
        ));
    }

    #[test]
    fn record_failure_is_reported_without_rolling_back_the_accepted_head() {
        let authority = authority();
        let ingress = BrowserOperationIngress::new(
            authority.clone(),
            PortId::parse("browser").unwrap(),
            Actuator::default(),
            Persistence::default(),
            FailingRecords,
        );
        let accepted = ingress.accept(key()).unwrap();
        assert_eq!(accepted.run_seq, 1);
        assert_eq!(
            accepted.record_error.as_deref(),
            Some("Record Writer is unavailable")
        );
        assert_eq!(authority.current_head().head, accepted.transition.to);
    }

    #[test]
    fn checkpoint_explicitly_links_physical_state_without_a_browser_record() {
        let (authority, objects) = checkpoint_authority();
        let state_ref = objects.put(b"browser-state-object").unwrap();
        let before = authority.current_head();
        let accepted = authority
            .accept(
                &checkpoint_offer(state_ref.clone()),
                || Ok(()),
                |_| Ok(()),
                |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(accepted.run_seq, 1);
        assert_ne!(accepted.transition.to, before.head);
        let next = authority.current_head();
        let computation = ato_objects::resolve_computation(&*objects, &next.head).unwrap();
        let residual_ref = &computation.object().residual;
        let residual = decode_residual(
            &ato_objects::read_exact_object(
                &*objects,
                residual_ref,
                objects.metadata(residual_ref).unwrap().size,
                64 * 1024,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(residual.checkpoint_state_ref, Some(state_ref.to_string()));
    }
}
