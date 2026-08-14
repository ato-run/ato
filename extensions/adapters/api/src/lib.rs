//! Shared public factory/session contract used identically by every Adapter.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use ato_computation::{PortId, ProtocolId};
use ato_objects::{Direction, ObjectStore, RecordEnvelope, RecordId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdapterCapabilities {
    pub observe: bool,
    pub apply: bool,
    pub verify: bool,
    pub quiesce: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterObservation {
    pub adapter_id: String,
    pub protocol_id: ProtocolId,
    pub port_id: PortId,
    pub direction: Direction,
    pub payload: Vec<u8>,
    pub caused_by: Vec<RecordId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterInstance {
    pub instance_id: String,
    pub adapter_id: String,
    #[serde(default)]
    pub config: Value,
}

pub struct AdapterContext<'a> {
    pub workspace: &'a Path,
    pub objects: &'a dyn ObjectStore,
}

pub struct AdapterAttachContext<'a> {
    pub runtime: AdapterContext<'a>,
    pub observations: Arc<dyn ObservationSink>,
}

pub trait ObservationSink: Send + Sync {
    fn emit(&self, observation: AdapterObservation) -> Result<(), AdapterError>;
}

#[derive(Default)]
pub struct IgnoreObservations;

impl ObservationSink for IgnoreObservations {
    fn emit(&self, _observation: AdapterObservation) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// One live Adapter instance. Stateful operations always target this object,
/// including replay, quiesce, and detach.
pub trait AttachedAdapter: Send {
    fn instance_id(&self) -> &str;
    fn adapter_id(&self) -> &str;
    fn capabilities(&self) -> AdapterCapabilities;

    fn accepts(&self, record: &RecordEnvelope) -> bool {
        self.adapter_id() == record.adapter_id
    }

    fn apply(
        &mut self,
        _record: &RecordEnvelope,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.adapter_id().to_owned(),
            operation: "apply",
        })
    }

    fn verify(
        &mut self,
        _record: &RecordEnvelope,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.adapter_id().to_owned(),
            operation: "verify",
        })
    }

    fn quiesce(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn detach(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn wait(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// Creates configured live instances. Built-ins and third parties enter the
/// runtime through this exact contract; the supervisor never switches on IDs.
pub trait AdapterFactory: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> AdapterCapabilities;

    fn preflight(
        &self,
        _instance: &AdapterInstance,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    fn attach(
        &self,
        instance: &AdapterInstance,
        context: &AdapterAttachContext<'_>,
    ) -> Result<Box<dyn AttachedAdapter>, AdapterError>;
}

#[derive(Default)]
pub struct AdapterRegistry {
    factories: BTreeMap<String, Arc<dyn AdapterFactory>>,
}

impl AdapterRegistry {
    pub fn register(&mut self, factory: Arc<dyn AdapterFactory>) -> Result<(), AdapterError> {
        let id = validate_adapter_id(factory.id())?;
        if self.factories.insert(id.clone(), factory).is_some() {
            return Err(AdapterError::Duplicate(id));
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&dyn AdapterFactory, AdapterError> {
        self.factories
            .get(id)
            .map(Arc::as_ref)
            .ok_or_else(|| AdapterError::Unknown(id.to_owned()))
    }

    pub fn attach_all(
        &self,
        instances: &[AdapterInstance],
        context: &AdapterAttachContext<'_>,
    ) -> Result<Vec<Box<dyn AttachedAdapter>>, AdapterError> {
        for instance in instances {
            if instance.adapter_id != self.get(&instance.adapter_id)?.id() {
                return Err(AdapterError::InvalidConfig(
                    "factory id does not match configured adapter".to_owned(),
                ));
            }
            self.get(&instance.adapter_id)?
                .preflight(instance, &context.runtime)?;
        }
        let mut attached: Vec<Box<dyn AttachedAdapter>> = Vec::new();
        for instance in instances {
            match self.get(&instance.adapter_id)?.attach(instance, context) {
                Ok(session) => attached.push(session),
                Err(error) => {
                    for session in attached.iter_mut().rev() {
                        let _ = session.detach(&context.runtime);
                    }
                    return Err(error);
                }
            }
        }
        Ok(attached)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.factories.keys().map(String::as_str)
    }
}

fn validate_adapter_id(value: &str) -> Result<String, AdapterError> {
    let (name, version) = value
        .rsplit_once('@')
        .ok_or_else(|| AdapterError::InvalidId(value.to_owned()))?;
    if !name.contains('.')
        || name.is_empty()
        || version.is_empty()
        || !version.bytes().all(|byte| byte.is_ascii_digit())
        || version.starts_with('0')
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(AdapterError::InvalidId(value.to_owned()));
    }
    Ok(value.to_owned())
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("invalid adapter id `{0}`")]
    InvalidId(String),
    #[error("adapter `{0}` is already registered")]
    Duplicate(String),
    #[error("adapter `{0}` is not registered")]
    Unknown(String),
    #[error("adapter `{adapter}` does not support {operation}")]
    Unsupported {
        adapter: String,
        operation: &'static str,
    },
    #[error("invalid adapter configuration: {0}")]
    InvalidConfig(String),
    #[error("adapter operation failed: {0}")]
    Operation(String),
    #[error(transparent)]
    Objects(#[from] ato_objects::ObjectError),
    #[error("adapter JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("adapter I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ThirdParty;
    struct ThirdPartySession(String);

    impl AttachedAdapter for ThirdPartySession {
        fn instance_id(&self) -> &str {
            &self.0
        }
        fn adapter_id(&self) -> &str {
            "example.custom@1"
        }
        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities {
                observe: true,
                verify: true,
                ..AdapterCapabilities::default()
            }
        }
    }

    impl AdapterFactory for ThirdParty {
        fn id(&self) -> &str {
            "example.custom@1"
        }
        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities {
                observe: true,
                verify: true,
                ..AdapterCapabilities::default()
            }
        }
        fn attach(
            &self,
            instance: &AdapterInstance,
            _: &AdapterAttachContext<'_>,
        ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
            Ok(Box::new(ThirdPartySession(instance.instance_id.clone())))
        }
    }

    #[test]
    fn third_party_uses_the_same_factory_and_live_session_path() {
        let directory = tempfile::tempdir().unwrap();
        let objects = ato_objects::FsObjectStore::open(directory.path().join("objects")).unwrap();
        let mut registry = AdapterRegistry::default();
        registry.register(Arc::new(ThirdParty)).unwrap();
        let sessions = registry
            .attach_all(
                &[AdapterInstance {
                    instance_id: "custom.one".to_owned(),
                    adapter_id: "example.custom@1".to_owned(),
                    config: Value::Null,
                }],
                &AdapterAttachContext {
                    runtime: AdapterContext {
                        workspace: directory.path(),
                        objects: &objects,
                    },
                    observations: Arc::new(IgnoreObservations),
                },
            )
            .unwrap();
        assert_eq!(sessions[0].instance_id(), "custom.one");
        assert!(sessions[0].capabilities().observe);
    }
}
