//! The minimal Ato kernel advances addressable computations.
//!
//! Concrete logical behavior is registered through [`Semantics`]. Physical
//! realization, history, authoring syntax, and provider state do not live here.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use ato_computation::{
    ComputationObject, ComputationRef, ContentRef, PortId, ProtocolId, ResolvedComputation, RoleId,
    SemanticsId, computation_ref, encode_computation_object,
};
use ato_objects::{ObjectError, ObjectStore, read_exact_object, resolve_computation};
use thiserror::Error;

/// An opaque protocol message.
///
/// The kernel carries these bytes but never interprets them. The registered
/// [`ProtocolSemantics`] for a Port gives the bytes their type and validates
/// them before a semantic transition is attempted.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolPayload(Arc<[u8]>);

impl ProtocolPayload {
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ProtocolPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ProtocolPayload")
            .field(&format_args!("{} opaque bytes", self.0.len()))
            .finish()
    }
}

impl From<Vec<u8>> for ProtocolPayload {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }
}

impl From<&[u8]> for ProtocolPayload {
    fn from(bytes: &[u8]) -> Self {
        Self(Arc::from(bytes))
    }
}

impl From<&str> for ProtocolPayload {
    fn from(value: &str) -> Self {
        Self(Arc::from(value.as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Tau,
    Input {
        port: PortId,
        payload: ProtocolPayload,
    },
    Output {
        port: PortId,
        payload: ProtocolPayload,
    },
}

/// A semantics-owned discriminator for one enabled transition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChoiceId(Arc<str>);

impl ChoiceId {
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ChoiceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One explicitly selectable semantic transition.
///
/// External inputs may omit a choice because their payload can select the
/// transition. Enabled Tau and output transitions must always carry one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionOffer {
    pub choice: Option<ChoiceId>,
    pub action: Action,
}

impl TransitionOffer {
    pub fn external_input(port: PortId, payload: ProtocolPayload) -> Self {
        Self {
            choice: None,
            action: Action::Input { port, payload },
        }
    }

    pub fn selected(choice: ChoiceId, action: Action) -> Self {
        Self {
            choice: Some(choice),
            action,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub head: ComputationRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: ComputationRef,
    pub offer: TransitionOffer,
    pub to: ComputationRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticStep {
    pub offer: TransitionOffer,
    pub successor: ComputationObject,
}

pub trait SemanticHost: Send + Sync {
    fn resolve(&self, reference: &ComputationRef) -> Result<ResolvedComputation, KernelError>;

    fn enabled(&self, reference: &ComputationRef) -> Result<Vec<TransitionOffer>, KernelError>;

    fn derive_transition(
        &self,
        reference: &ComputationRef,
        offer: &TransitionOffer,
    ) -> Result<Transition, KernelError>;

    fn roles_compatible(
        &self,
        protocol: &ProtocolId,
        left: &RoleId,
        right: &RoleId,
    ) -> Result<bool, KernelError>;

    fn put_object(&self, bytes: &[u8]) -> Result<ContentRef, KernelError>;

    fn get_object(&self, reference: &ContentRef, maximum: u64) -> Result<Vec<u8>, KernelError>;
}

pub trait Semantics: Send + Sync {
    fn id(&self) -> &SemanticsId;

    fn validate(
        &self,
        _current: &ResolvedComputation,
        _host: &dyn SemanticHost,
    ) -> Result<(), SemanticError> {
        Ok(())
    }

    fn enabled(
        &self,
        _current: &ResolvedComputation,
        _host: &dyn SemanticHost,
    ) -> Result<Vec<TransitionOffer>, SemanticError> {
        Ok(Vec::new())
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        offer: &TransitionOffer,
        host: &dyn SemanticHost,
    ) -> Result<SemanticStep, SemanticError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("semantic transition failed: {message}")]
pub struct SemanticError {
    message: String,
}

impl SemanticError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("protocol interaction failed: {message}")]
pub struct ProtocolError {
    message: String,
}

impl ProtocolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Protocol-owned typing and behavioral rules for opaque payloads.
pub trait ProtocolSemantics: Send + Sync {
    fn id(&self) -> &ProtocolId;

    fn roles_compatible(&self, left: &RoleId, right: &RoleId) -> Result<bool, ProtocolError>;

    fn validate_input(&self, role: &RoleId, payload: &ProtocolPayload)
    -> Result<(), ProtocolError>;

    fn validate_output(
        &self,
        role: &RoleId,
        payload: &ProtocolPayload,
    ) -> Result<(), ProtocolError>;
}

pub trait TransitionSink: Send + Sync {
    fn observe(&self, transition: &Transition);
}

pub trait Observer<O>: Send + Sync {
    fn observe(&self, computation: &ResolvedComputation) -> O;
}

#[derive(Debug, Error)]
pub enum KernelError {
    #[error(transparent)]
    Objects(#[from] ObjectError),
    #[error("no semantics registered for {0}")]
    UnknownSemantics(SemanticsId),
    #[error("semantics {0} is already registered")]
    DuplicateSemantics(SemanticsId),
    #[error("no protocol semantics registered for {0}")]
    UnknownProtocol(ProtocolId),
    #[error("protocol semantics {0} is already registered")]
    DuplicateProtocol(ProtocolId),
    #[error("action names missing boundary port {0}")]
    UnknownPort(PortId),
    #[error("enabled {kind} transition is missing a semantics-owned choice id")]
    MissingChoice { kind: &'static str },
    #[error("semantics returned duplicate enabled choice {0}")]
    DuplicateChoice(ChoiceId),
    #[error("registered semantics {registered} returned successor owned by {actual}")]
    SuccessorSemanticsMismatch {
        registered: SemanticsId,
        actual: SemanticsId,
    },
    #[error("semantics changed the selected transition offer while deriving its successor")]
    TransitionOfferMismatch,
    #[error(transparent)]
    Semantic(#[from] SemanticError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("canonical computation encoding failed: {0}")]
    Computation(#[from] ato_computation::CodecError),
}

pub struct Kernel {
    objects: Arc<dyn ObjectStore>,
    semantics: BTreeMap<SemanticsId, Arc<dyn Semantics>>,
    protocols: BTreeMap<ProtocolId, Arc<dyn ProtocolSemantics>>,
    sink: Option<Arc<dyn TransitionSink>>,
}

impl Kernel {
    pub fn new(objects: Arc<dyn ObjectStore>) -> Self {
        Self {
            objects,
            semantics: BTreeMap::new(),
            protocols: BTreeMap::new(),
            sink: None,
        }
    }

    pub fn with_transition_sink(mut self, sink: Arc<dyn TransitionSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn register(&mut self, semantics: Arc<dyn Semantics>) -> Result<(), KernelError> {
        let id = semantics.id().clone();
        if self.semantics.insert(id.clone(), semantics).is_some() {
            return Err(KernelError::DuplicateSemantics(id));
        }
        Ok(())
    }

    pub fn register_protocol(
        &mut self,
        protocol: Arc<dyn ProtocolSemantics>,
    ) -> Result<(), KernelError> {
        let id = protocol.id().clone();
        if self.protocols.insert(id.clone(), protocol).is_some() {
            return Err(KernelError::DuplicateProtocol(id));
        }
        Ok(())
    }

    pub fn seal(&self, object: &ComputationObject) -> Result<ComputationRef, KernelError> {
        let bytes = encode_computation_object(object)?;
        let reference = computation_ref(object)?;
        self.objects.insert(reference.content_ref(), &bytes)?;
        Ok(reference)
    }

    pub fn resolve(&self, reference: &ComputationRef) -> Result<ResolvedComputation, KernelError> {
        Ok(resolve_computation(self.objects.as_ref(), reference)?)
    }

    pub fn enabled(&self, reference: &ComputationRef) -> Result<Vec<TransitionOffer>, KernelError> {
        let current = self.resolve(reference)?;
        let semantics = self.semantics_for(&current)?;
        semantics.validate(&current, self)?;
        let offers = semantics.enabled(&current, self)?;
        self.validate_offers(&current, &offers)?;
        Ok(offers)
    }

    /// Derive and seal a successor without publishing transition evidence.
    ///
    /// Concrete composition semantics use this operation for boundary-hidden
    /// child steps. The resulting object may remain unreachable when a larger
    /// reduction fails and is then ordinary GC material.
    pub fn derive_transition(
        &self,
        reference: &ComputationRef,
        offer: &TransitionOffer,
    ) -> Result<Transition, KernelError> {
        let current = self.resolve(reference)?;
        let semantics = self.semantics_for(&current)?;
        semantics.validate(&current, self)?;
        self.validate_action(&current, &offer.action)?;
        let step = semantics.step(&current, offer, self)?;
        if step.offer != *offer {
            return Err(KernelError::TransitionOfferMismatch);
        }
        if step.successor.semantics != *semantics.id() {
            return Err(KernelError::SuccessorSemanticsMismatch {
                registered: semantics.id().clone(),
                actual: step.successor.semantics,
            });
        }
        let to = self.seal(&step.successor)?;
        let successor = self.resolve(&to)?;
        semantics.validate(&successor, self)?;
        Ok(Transition {
            from: reference.clone(),
            offer: step.offer,
            to,
        })
    }

    /// Publish one already-derived transition as visible evidence.
    pub fn commit_transition(&self, transition: &Transition) {
        if let Some(sink) = &self.sink {
            sink.observe(transition);
        }
    }

    pub fn step(&self, run: &mut Run, offer: &TransitionOffer) -> Result<Transition, KernelError> {
        let transition = self.derive_transition(&run.head, offer)?;
        self.commit_transition(&transition);
        run.head = transition.to.clone();
        Ok(transition)
    }

    pub fn observe<O>(&self, run: &Run, observer: &dyn Observer<O>) -> Result<O, KernelError> {
        Ok(observer.observe(&self.resolve(&run.head)?))
    }

    fn semantics_for(
        &self,
        current: &ResolvedComputation,
    ) -> Result<&Arc<dyn Semantics>, KernelError> {
        self.semantics
            .get(&current.object().semantics)
            .ok_or_else(|| KernelError::UnknownSemantics(current.object().semantics.clone()))
    }

    fn protocol_for(
        &self,
        protocol: &ProtocolId,
    ) -> Result<&Arc<dyn ProtocolSemantics>, KernelError> {
        self.protocols
            .get(protocol)
            .ok_or_else(|| KernelError::UnknownProtocol(protocol.clone()))
    }

    fn validate_offers(
        &self,
        current: &ResolvedComputation,
        offers: &[TransitionOffer],
    ) -> Result<(), KernelError> {
        let mut choices = BTreeSet::new();
        for offer in offers {
            self.validate_action(current, &offer.action)?;
            if !matches!(offer.action, Action::Input { .. }) && offer.choice.is_none() {
                let kind = if matches!(offer.action, Action::Tau) {
                    "tau"
                } else {
                    "output"
                };
                return Err(KernelError::MissingChoice { kind });
            }
            if let Some(choice) = &offer.choice
                && !choices.insert(choice.clone())
            {
                return Err(KernelError::DuplicateChoice(choice.clone()));
            }
        }
        Ok(())
    }

    fn validate_action(
        &self,
        current: &ResolvedComputation,
        action: &Action,
    ) -> Result<(), KernelError> {
        let (port, payload, input) = match action {
            Action::Tau => return Ok(()),
            Action::Input { port, payload } => (port, payload, true),
            Action::Output { port, payload } => (port, payload, false),
        };
        let definition = current
            .object()
            .boundary
            .get(port)
            .ok_or_else(|| KernelError::UnknownPort(port.clone()))?;
        let protocol = self.protocol_for(&definition.protocol)?;
        if input {
            protocol.validate_input(&definition.role, payload)?;
        } else {
            protocol.validate_output(&definition.role, payload)?;
        }
        Ok(())
    }
}

impl SemanticHost for Kernel {
    fn resolve(&self, reference: &ComputationRef) -> Result<ResolvedComputation, KernelError> {
        Kernel::resolve(self, reference)
    }

    fn enabled(&self, reference: &ComputationRef) -> Result<Vec<TransitionOffer>, KernelError> {
        Kernel::enabled(self, reference)
    }

    fn derive_transition(
        &self,
        reference: &ComputationRef,
        offer: &TransitionOffer,
    ) -> Result<Transition, KernelError> {
        Kernel::derive_transition(self, reference, offer)
    }

    fn roles_compatible(
        &self,
        protocol: &ProtocolId,
        left: &RoleId,
        right: &RoleId,
    ) -> Result<bool, KernelError> {
        Ok(self.protocol_for(protocol)?.roles_compatible(left, right)?)
    }

    fn put_object(&self, bytes: &[u8]) -> Result<ContentRef, KernelError> {
        Ok(self.objects.put(bytes)?)
    }

    fn get_object(&self, reference: &ContentRef, maximum: u64) -> Result<Vec<u8>, KernelError> {
        let metadata = self.objects.metadata(reference)?;
        Ok(read_exact_object(
            self.objects.as_ref(),
            reference,
            metadata.size,
            maximum,
        )?)
    }
}
