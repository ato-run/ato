//! Shared public contract for encoding and restoring physical realizations.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use ato_adapter_api::AdapterRegistry;
use ato_computation::{ComputationRef, ContentRef};
use ato_objects::{ObjectStore, RecordEnvelope};
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

pub struct MaterializerContext<'a> {
    pub objects: &'a dyn ObjectStore,
    pub adapters: &'a AdapterRegistry,
    pub records: &'a [RecordEnvelope],
    pub workspace: &'a Path,
}

pub trait Materializer: Send + Sync {
    fn id(&self) -> &str;

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

    fn restore(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ComputationRef, MaterializerError> {
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
    #[error("materializer `{0}` cannot restore")]
    RestoreUnsupported(String),
    #[error("materializer `{materializer}` requires Adapter.apply from `{adapter}`")]
    MissingApply {
        materializer: String,
        adapter: String,
    },
    #[error("materializer operation failed: {0}")]
    Operation(String),
    #[error(transparent)]
    Objects(#[from] ato_objects::ObjectError),
    #[error("materializer JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
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
}
