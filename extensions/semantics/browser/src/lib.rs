//! Logical Browser interaction frontier. Chrome state remains physical.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use ato_adapter_api::LiveOperation;
use ato_adapter_browser::{BROWSER_PROTOCOL_ID, decode_event, operation_for_event};
use ato_computation::{
    ComputationObject, OperationId, PortId, ProtocolId, ResolvedComputation, RoleId, SemanticsId,
};
use ato_kernel::{
    AcceptedOperation, Action, EvolutionError, KernelError, PendingHeadPersistence, ProtocolError,
    ProtocolPayload, ProtocolSemantics, RunEvolutionAuthority, SemanticError, SemanticHost,
    SemanticStep, Semantics, TransitionOffer,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use url::Url;

pub const BROWSER_COMPUTATION_SEMANTICS_ID: &str = "ato.browser.computation@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserResidualV1 {
    pub version: u32,
    pub expected_origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_frontier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrowserSemanticsError {
    #[error("invalid Browser residual: {0}")]
    Residual(String),
}

pub fn encode_residual(residual: &BrowserResidualV1) -> Result<Vec<u8>, BrowserSemanticsError> {
    validate_residual(residual)?;
    serde_jcs::to_vec(residual).map_err(|error| BrowserSemanticsError::Residual(error.to_string()))
}

pub fn decode_residual(bytes: &[u8]) -> Result<BrowserResidualV1, BrowserSemanticsError> {
    let residual: BrowserResidualV1 = serde_json::from_slice(bytes)
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
        let Action::Input { port, payload } = &offer.action else {
            return Err(SemanticError::new(
                "Browser Computation accepts external input only",
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
        let next = BrowserResidualV1 {
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
            == BTreeSet::from(["browser", "controller"]))
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

fn residual_for(
    current: &ResolvedComputation,
    host: &dyn SemanticHost,
) -> Result<BrowserResidualV1, KernelError> {
    let bytes = host.get_object(&current.object().residual, 64 * 1024)?;
    decode_residual(&bytes)
        .map_err(|error| KernelError::Semantic(SemanticError::new(error.to_string())))
}

fn validate_residual(residual: &BrowserResidualV1) -> Result<(), BrowserSemanticsError> {
    let origin = Url::parse(&residual.expected_origin)
        .map_err(|error| BrowserSemanticsError::Residual(error.to_string()))?;
    if residual.version != 1
        || !matches!(origin.scheme(), "http" | "https")
        || origin.origin().ascii_serialization() != residual.expected_origin
    {
        return Err(BrowserSemanticsError::Residual(
            "expected_origin must be an exact HTTP(S) origin".to_owned(),
        ));
    }
    Ok(())
}

/// Physical Browser boundary used by hosted operation ingress. Implementations
/// send the canonical live operation to Chrome and return only after its ACK.
pub trait BrowserOperationActuator: Send {
    fn apply(&mut self, operation: &LiveOperation) -> Result<(), String>;
}

/// Runner control-plane projection port. Its failure is intentionally kept
/// separate from physical Browser success.
pub trait BrowserHeadPersistence: Send + Sync {
    fn persist(&self, pending: &PendingHeadPersistence) -> Result<(), String>;
}

/// Non-persisting Record submission port. The caller decides whether a Record
/// Writer queue or a test probe receives the accepted candidate.
pub trait BrowserRecordSubmission: Send + Sync {
    fn submit(&self, event: &ato_adapter_browser::BrowserEvent) -> Result<(), String>;
}

/// One canonical hosted Browser operation path. It is not a Player: it takes a
/// live event, derives the logical transition, obtains a physical ACK, commits
/// the head, persists the runner projection, then submits exactly one Record.
pub struct BrowserOperationIngress<A, P, R> {
    authority: Arc<RunEvolutionAuthority>,
    browser_port: PortId,
    actuator: Mutex<A>,
    persistence: P,
    records: R,
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
            actuator: Mutex::new(actuator),
            persistence,
            records,
        }
    }

    pub fn accept(
        &self,
        event: ato_adapter_browser::BrowserEvent,
    ) -> Result<AcceptedOperation, EvolutionError> {
        let payload = ato_adapter_browser::encode_event(&event)
            .map_err(|error| EvolutionError::Apply(error.to_string()))?;
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
            ProtocolPayload::from(payload),
        );
        self.authority.accept(
            &offer,
            || {
                self.actuator
                    .lock()
                    .map_err(|_| "Browser actuator mutex poisoned".to_owned())?
                    .apply(&operation)
            },
            |pending| self.persistence.persist(pending),
            |_, _| self.records.submit(&event),
        )
    }

    pub fn retry_pending_persistence(&self) -> Result<bool, EvolutionError> {
        self.authority
            .retry_pending_persistence(|pending| self.persistence.persist(pending))
    }

    pub fn freeze(&self) -> Result<ato_kernel::RunHeadSnapshot, EvolutionError> {
        self.authority.freeze()
    }

    pub fn unfreeze(&self) {
        self.authority.unfreeze();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ato_adapter_browser::{BrowserEvent, KeyboardKind, Modifiers};
    use ato_computation::{Boundary, PortDef};
    use ato_kernel::Kernel;
    use ato_objects::{MemoryObjectStore, ObjectStore};

    use super::*;

    #[derive(Default)]
    struct Actuator {
        calls: Vec<LiveOperation>,
        reject: bool,
    }
    impl BrowserOperationActuator for Actuator {
        fn apply(&mut self, operation: &LiveOperation) -> Result<(), String> {
            if self.reject {
                return Err("Browser ACK timeout".to_owned());
            }
            self.calls.push(operation.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct Persistence(Mutex<Vec<PendingHeadPersistence>>);
    impl BrowserHeadPersistence for Persistence {
        fn persist(&self, pending: &PendingHeadPersistence) -> Result<(), String> {
            self.0.lock().unwrap().push(pending.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct Records(Mutex<Vec<BrowserEvent>>);
    impl BrowserRecordSubmission for Records {
        fn submit(&self, event: &BrowserEvent) -> Result<(), String> {
            self.0.lock().unwrap().push(event.clone());
            Ok(())
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
                    expected_origin: "http://127.0.0.1:8080".to_owned(),
                    interaction_frontier: None,
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
    }

    #[test]
    fn rejected_browser_ack_does_not_advance_or_record() {
        let authority = authority();
        let before = authority.current_head();
        let ingress = BrowserOperationIngress::new(
            authority.clone(),
            PortId::parse("browser").unwrap(),
            Actuator {
                calls: Vec::new(),
                reject: true,
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
}
