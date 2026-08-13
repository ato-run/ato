//! The minimal Ato kernel advances addressable computations.
//!
//! Concrete logical behavior is registered through [`Semantics`]. Physical
//! realization, history, authoring syntax, and provider state do not live here.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use ato_computation::{
    ComputationObject, ComputationRef, ContentRef, PortId, ResolvedComputation, SemanticsId,
    computation_ref, encode_computation_object,
};
use ato_objects::{ObjectError, ObjectStore, read_exact_object, resolve_computation};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action<V> {
    Tau,
    Input { port: PortId, value: V },
    Output { port: PortId, value: V },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub head: ComputationRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition<V> {
    pub from: ComputationRef,
    pub action: Action<V>,
    pub to: ComputationRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticStep<V> {
    pub action: Action<V>,
    pub successor: ComputationObject,
}

pub trait SemanticHost<V>: Send + Sync {
    fn resolve(&self, reference: &ComputationRef) -> Result<ResolvedComputation, KernelError>;

    fn enabled(&self, reference: &ComputationRef) -> Result<Vec<Action<V>>, KernelError>;

    fn transition(
        &self,
        reference: &ComputationRef,
        action: &Action<V>,
    ) -> Result<Transition<V>, KernelError>;

    fn put_object(&self, bytes: &[u8]) -> Result<ContentRef, KernelError>;

    fn get_object(&self, reference: &ContentRef, maximum: u64) -> Result<Vec<u8>, KernelError>;
}

pub trait Semantics<V>: Send + Sync {
    fn id(&self) -> &SemanticsId;

    fn validate(
        &self,
        _current: &ResolvedComputation,
        _host: &dyn SemanticHost<V>,
    ) -> Result<(), SemanticError> {
        Ok(())
    }

    fn enabled(
        &self,
        _current: &ResolvedComputation,
        _host: &dyn SemanticHost<V>,
    ) -> Result<Vec<Action<V>>, SemanticError> {
        Ok(Vec::new())
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        action: &Action<V>,
        host: &dyn SemanticHost<V>,
    ) -> Result<SemanticStep<V>, SemanticError>;
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

pub trait TransitionSink<V>: Send + Sync {
    fn observe(&self, transition: &Transition<V>);
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
    #[error("registered semantics {registered} returned successor owned by {actual}")]
    SuccessorSemanticsMismatch {
        registered: SemanticsId,
        actual: SemanticsId,
    },
    #[error(transparent)]
    Semantic(#[from] SemanticError),
    #[error("canonical computation encoding failed: {0}")]
    Computation(#[from] ato_computation::CodecError),
}

pub struct Kernel<V> {
    objects: Arc<dyn ObjectStore>,
    semantics: BTreeMap<SemanticsId, Arc<dyn Semantics<V>>>,
    sink: Option<Arc<dyn TransitionSink<V>>>,
}

impl<V> Kernel<V>
where
    V: Clone + Send + Sync + 'static,
{
    pub fn new(objects: Arc<dyn ObjectStore>) -> Self {
        Self {
            objects,
            semantics: BTreeMap::new(),
            sink: None,
        }
    }

    pub fn with_transition_sink(mut self, sink: Arc<dyn TransitionSink<V>>) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn register(&mut self, semantics: Arc<dyn Semantics<V>>) -> Result<(), KernelError> {
        let id = semantics.id().clone();
        if self.semantics.insert(id.clone(), semantics).is_some() {
            return Err(KernelError::DuplicateSemantics(id));
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

    pub fn enabled(&self, reference: &ComputationRef) -> Result<Vec<Action<V>>, KernelError> {
        let current = self.resolve(reference)?;
        let semantics = self.semantics_for(&current)?;
        semantics.validate(&current, self)?;
        Ok(semantics.enabled(&current, self)?)
    }

    pub fn transition(
        &self,
        reference: &ComputationRef,
        action: &Action<V>,
    ) -> Result<Transition<V>, KernelError> {
        let current = self.resolve(reference)?;
        let semantics = self.semantics_for(&current)?;
        semantics.validate(&current, self)?;
        let step = semantics.step(&current, action, self)?;
        if step.successor.semantics != *semantics.id() {
            return Err(KernelError::SuccessorSemanticsMismatch {
                registered: semantics.id().clone(),
                actual: step.successor.semantics,
            });
        }
        let to = self.seal(&step.successor)?;
        let successor = self.resolve(&to)?;
        semantics.validate(&successor, self)?;
        let transition = Transition {
            from: reference.clone(),
            action: step.action,
            to,
        };
        if let Some(sink) = &self.sink {
            sink.observe(&transition);
        }
        Ok(transition)
    }

    pub fn step(&self, run: &mut Run, action: &Action<V>) -> Result<Transition<V>, KernelError> {
        let transition = self.transition(&run.head, action)?;
        run.head = transition.to.clone();
        Ok(transition)
    }

    pub fn observe<O>(&self, run: &Run, observer: &dyn Observer<O>) -> Result<O, KernelError> {
        Ok(observer.observe(&self.resolve(&run.head)?))
    }

    fn semantics_for(
        &self,
        current: &ResolvedComputation,
    ) -> Result<&Arc<dyn Semantics<V>>, KernelError> {
        self.semantics
            .get(&current.object().semantics)
            .ok_or_else(|| KernelError::UnknownSemantics(current.object().semantics.clone()))
    }
}

impl<V> SemanticHost<V> for Kernel<V>
where
    V: Clone + Send + Sync + 'static,
{
    fn resolve(&self, reference: &ComputationRef) -> Result<ResolvedComputation, KernelError> {
        Kernel::resolve(self, reference)
    }

    fn enabled(&self, reference: &ComputationRef) -> Result<Vec<Action<V>>, KernelError> {
        Kernel::enabled(self, reference)
    }

    fn transition(
        &self,
        reference: &ComputationRef,
        action: &Action<V>,
    ) -> Result<Transition<V>, KernelError> {
        Kernel::transition(self, reference, action)
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
