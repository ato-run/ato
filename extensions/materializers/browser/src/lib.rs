//! Portable Browser Materialization v1.
//!
//! This extension encodes only bounded `localStorage` state. Origin and Chrome
//! profile are runtime bindings: neither appears here or in a Computation
//! residual. Cookies, sessionStorage, IndexedDB, console, DOM and screenshots
//! are intentionally unsupported in v1.

#![forbid(unsafe_code)]

use ato_computation::{ComputationRef, ContentRef};
use ato_objects::{ObjectResolver, ObjectStore, read_exact_object};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const BROWSER_MATERIALIZER_ID: &str = "ato.materialize.browser@1";
pub const BROWSER_MATERIALIZATION_VERSION: u32 = 1;
pub const LOCAL_STORAGE_CAPABILITY: &str = "local_storage";

pub const MAX_LOCAL_STORAGE_ITEMS: usize = 4096;
pub const MAX_LOCAL_STORAGE_KEY_BYTES: usize = 16 * 1024;
pub const MAX_LOCAL_STORAGE_VALUE_BYTES: usize = 1024 * 1024;
pub const MAX_LOCAL_STORAGE_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = MAX_LOCAL_STORAGE_TOTAL_BYTES as u64 + 2 * 1024 * 1024;
const MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserLocalStorageEntryV1 {
    pub key: String,
    pub value: String,
}

/// State needed to resume a document after its runtime origin has been bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserStateV1 {
    pub version: u32,
    pub local_storage: Vec<BrowserLocalStorageEntryV1>,
}

/// Materialization descriptor. `state_ref` is a physical ContentRef; it is not
/// itself a ComputationRef. The target is explicit so planner/validator can
/// reject an accidental attachment to a different continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserMaterializationDescriptorV1 {
    pub version: u32,
    pub target_computation_ref: String,
    pub state_ref: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Error)]
pub enum BrowserMaterializationError {
    #[error("invalid Browser materialization: {0}")]
    Invalid(String),
    #[error("Browser materialization object error: {0}")]
    Object(String),
}

pub fn encode_state(state: &BrowserStateV1) -> Result<Vec<u8>, BrowserMaterializationError> {
    validate_state(state)?;
    serde_jcs::to_vec(state)
        .map_err(|error| BrowserMaterializationError::Invalid(error.to_string()))
}

pub fn store_state(
    state: &BrowserStateV1,
    objects: &dyn ObjectStore,
) -> Result<ContentRef, BrowserMaterializationError> {
    objects
        .put(&encode_state(state)?)
        .map_err(|error| BrowserMaterializationError::Object(error.to_string()))
}

pub fn load_state(
    reference: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<BrowserStateV1, BrowserMaterializationError> {
    let metadata = objects
        .metadata(reference)
        .map_err(|error| BrowserMaterializationError::Object(error.to_string()))?;
    let bytes = read_exact_object(objects, reference, metadata.size, MAX_STATE_BYTES)
        .map_err(|error| BrowserMaterializationError::Object(error.to_string()))?;
    let state: BrowserStateV1 = serde_json::from_slice(&bytes)
        .map_err(|error| BrowserMaterializationError::Invalid(error.to_string()))?;
    if encode_state(&state)? != bytes {
        return Err(BrowserMaterializationError::Invalid(
            "state is not canonical JCS".to_owned(),
        ));
    }
    Ok(state)
}

pub fn store_descriptor(
    target: &ComputationRef,
    state_ref: &ContentRef,
    objects: &dyn ObjectStore,
) -> Result<ContentRef, BrowserMaterializationError> {
    let descriptor = BrowserMaterializationDescriptorV1 {
        version: BROWSER_MATERIALIZATION_VERSION,
        target_computation_ref: target.to_string(),
        state_ref: state_ref.to_string(),
        capabilities: vec![LOCAL_STORAGE_CAPABILITY.to_owned()],
    };
    validate_descriptor(&descriptor)?;
    let bytes = serde_jcs::to_vec(&descriptor)
        .map_err(|error| BrowserMaterializationError::Invalid(error.to_string()))?;
    objects
        .put(&bytes)
        .map_err(|error| BrowserMaterializationError::Object(error.to_string()))
}

pub fn load_descriptor(
    reference: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<BrowserMaterializationDescriptorV1, BrowserMaterializationError> {
    let metadata = objects
        .metadata(reference)
        .map_err(|error| BrowserMaterializationError::Object(error.to_string()))?;
    let bytes = read_exact_object(objects, reference, metadata.size, MAX_DESCRIPTOR_BYTES)
        .map_err(|error| BrowserMaterializationError::Object(error.to_string()))?;
    let descriptor: BrowserMaterializationDescriptorV1 = serde_json::from_slice(&bytes)
        .map_err(|error| BrowserMaterializationError::Invalid(error.to_string()))?;
    validate_descriptor(&descriptor)?;
    let canonical = serde_jcs::to_vec(&descriptor)
        .map_err(|error| BrowserMaterializationError::Invalid(error.to_string()))?;
    if canonical != bytes {
        return Err(BrowserMaterializationError::Invalid(
            "descriptor is not canonical JCS".to_owned(),
        ));
    }
    Ok(descriptor)
}

pub fn validate_state(state: &BrowserStateV1) -> Result<(), BrowserMaterializationError> {
    if state.version != BROWSER_MATERIALIZATION_VERSION {
        return Err(BrowserMaterializationError::Invalid(
            "unsupported Browser state version".to_owned(),
        ));
    }
    if state.local_storage.len() > MAX_LOCAL_STORAGE_ITEMS {
        return Err(BrowserMaterializationError::Invalid(
            "localStorage item count exceeds bound".to_owned(),
        ));
    }
    let mut total = 0usize;
    let mut previous: Option<&str> = None;
    for entry in &state.local_storage {
        if entry.key.len() > MAX_LOCAL_STORAGE_KEY_BYTES
            || entry.value.len() > MAX_LOCAL_STORAGE_VALUE_BYTES
        {
            return Err(BrowserMaterializationError::Invalid(
                "localStorage key or value exceeds bound".to_owned(),
            ));
        }
        if let Some(prior) = previous
            && prior >= entry.key.as_str()
        {
            return Err(BrowserMaterializationError::Invalid(
                "localStorage entries must be sorted and unique".to_owned(),
            ));
        }
        total = total
            .checked_add(entry.key.len())
            .and_then(|value| value.checked_add(entry.value.len()))
            .ok_or_else(|| {
                BrowserMaterializationError::Invalid("localStorage size overflow".to_owned())
            })?;
        if total > MAX_LOCAL_STORAGE_TOTAL_BYTES {
            return Err(BrowserMaterializationError::Invalid(
                "localStorage total exceeds bound".to_owned(),
            ));
        }
        previous = Some(&entry.key);
    }
    Ok(())
}

pub fn validate_descriptor(
    descriptor: &BrowserMaterializationDescriptorV1,
) -> Result<(), BrowserMaterializationError> {
    if descriptor.version != BROWSER_MATERIALIZATION_VERSION {
        return Err(BrowserMaterializationError::Invalid(
            "unsupported Browser descriptor version".to_owned(),
        ));
    }
    ComputationRef::parse(&descriptor.target_computation_ref)
        .map_err(|error| BrowserMaterializationError::Invalid(error.to_string()))?;
    ContentRef::parse(&descriptor.state_ref)
        .map_err(|error| BrowserMaterializationError::Invalid(error.to_string()))?;
    if descriptor.capabilities != [LOCAL_STORAGE_CAPABILITY] {
        return Err(BrowserMaterializationError::Invalid(
            "Browser v1 supports exactly local_storage".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ato_objects::MemoryObjectStore;

    use super::*;

    #[test]
    fn state_and_descriptor_are_canonical_and_origin_free() {
        let objects = MemoryObjectStore::default();
        let state = BrowserStateV1 {
            version: 1,
            local_storage: vec![BrowserLocalStorageEntryV1 {
                key: "gameState".to_owned(),
                value: "{}".to_owned(),
            }],
        };
        let state_ref = store_state(&state, &objects).unwrap();
        let target = ComputationRef::parse(format!("blake3:{}", "11".repeat(32))).unwrap();
        let descriptor_ref = store_descriptor(&target, &state_ref, &objects).unwrap();
        assert_eq!(load_state(&state_ref, &objects).unwrap(), state);
        let descriptor = load_descriptor(&descriptor_ref, &objects).unwrap();
        assert_eq!(descriptor.state_ref, state_ref.to_string());
        assert!(!format!("{descriptor:?}").contains("127.0.0.1"));
    }

    #[test]
    fn rejects_unsorted_or_oversized_state() {
        let unsorted = BrowserStateV1 {
            version: 1,
            local_storage: vec![
                BrowserLocalStorageEntryV1 {
                    key: "b".to_owned(),
                    value: String::new(),
                },
                BrowserLocalStorageEntryV1 {
                    key: "a".to_owned(),
                    value: String::new(),
                },
            ],
        };
        assert!(validate_state(&unsorted).is_err());
    }
}
