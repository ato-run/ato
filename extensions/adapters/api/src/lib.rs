//! Shared public contract used identically by built-in and third-party adapters.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use ato_computation::{PortId, ProtocolId};
use ato_objects::{Direction, ObjectStore, RecordEnvelope, RecordId};
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
    pub protocol_id: ProtocolId,
    pub port_id: PortId,
    pub direction: Direction,
    pub payload: Vec<u8>,
    pub caused_by: Vec<RecordId>,
}

pub struct AdapterContext<'a> {
    pub workspace: &'a Path,
    pub objects: &'a dyn ObjectStore,
}

pub trait Adapter: Send + Sync {
    fn id(&self) -> &str;

    fn capabilities(&self) -> AdapterCapabilities;

    fn preflight(&self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn attach(&self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn observe(
        &self,
        _context: &AdapterContext<'_>,
    ) -> Result<Vec<AdapterObservation>, AdapterError> {
        if self.capabilities().observe {
            Ok(Vec::new())
        } else {
            Err(AdapterError::Unsupported {
                adapter: self.id().to_owned(),
                operation: "observe",
            })
        }
    }

    fn apply(
        &self,
        _record: &RecordEnvelope,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.id().to_owned(),
            operation: "apply",
        })
    }

    fn verify(
        &self,
        _record: &RecordEnvelope,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.id().to_owned(),
            operation: "verify",
        })
    }

    fn quiesce(&self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn detach(&self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }
}

#[derive(Default)]
pub struct AdapterRegistry {
    adapters: BTreeMap<String, Arc<dyn Adapter>>,
}

impl AdapterRegistry {
    pub fn register(&mut self, adapter: Arc<dyn Adapter>) -> Result<(), AdapterError> {
        let id = validate_adapter_id(adapter.id())?;
        if self.adapters.insert(id.clone(), adapter).is_some() {
            return Err(AdapterError::Duplicate(id));
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&dyn Adapter, AdapterError> {
        self.adapters
            .get(id)
            .map(Arc::as_ref)
            .ok_or_else(|| AdapterError::Unknown(id.to_owned()))
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.adapters.keys().map(String::as_str)
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
    #[error("adapter operation failed: {0}")]
    Operation(String),
    #[error(transparent)]
    Objects(#[from] ato_objects::ObjectError),
    #[error("adapter I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ThirdParty;

    impl Adapter for ThirdParty {
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
    }

    #[test]
    fn third_party_uses_the_same_registry_path() {
        let mut registry = AdapterRegistry::default();
        registry.register(Arc::new(ThirdParty)).unwrap();
        let adapter = registry.get("example.custom@1").unwrap();
        assert!(adapter.capabilities().observe);
        assert!(!adapter.capabilities().apply);
    }
}
