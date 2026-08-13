//! Persistence and verified loading for addressable Ato objects.
//!
//! This crate stores bytes and traverses closures. Semantic identity remains
//! defined exclusively by `ato-computation`.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::sync::RwLock;

use ato_computation::{
    CodecError, ComputationRef, ContentRef, MAX_COMPUTATION_OBJECT_BYTES, ResolvedComputation,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub size: u64,
}

pub trait ObjectResolver: Send + Sync {
    fn metadata(&self, reference: &ContentRef) -> Result<ObjectMetadata, ObjectError>;

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send + '_>, ObjectError>;
}

pub trait ObjectStore: ObjectResolver {
    fn insert(&self, reference: &ContentRef, bytes: &[u8]) -> Result<(), ObjectError>;

    fn put(&self, bytes: &[u8]) -> Result<ContentRef, ObjectError> {
        let reference = blake3_reference(bytes);
        self.insert(&reference, bytes)?;
        Ok(reference)
    }
}

#[derive(Debug, Error)]
pub enum ObjectError {
    #[error("object resolution I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("object not found: {0}")]
    NotFound(ContentRef),
    #[error("computation object is {actual} bytes; maximum is {maximum}")]
    ObjectTooLarge { actual: u64, maximum: u64 },
    #[error("object metadata reported {expected} bytes but resolver returned {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("object identity mismatch: expected {expected}, got {actual}")]
    IdentityMismatch {
        expected: ContentRef,
        actual: ContentRef,
    },
    #[error(transparent)]
    Computation(#[from] CodecError),
    #[error("object store failed: {0}")]
    Storage(String),
}

pub fn resolve_computation(
    objects: &dyn ObjectResolver,
    reference: &ComputationRef,
) -> Result<ResolvedComputation, ObjectError> {
    let metadata = objects.metadata(reference.content_ref())?;
    if metadata.size > MAX_COMPUTATION_OBJECT_BYTES {
        return Err(ObjectError::ObjectTooLarge {
            actual: metadata.size,
            maximum: MAX_COMPUTATION_OBJECT_BYTES,
        });
    }
    let bytes = read_exact_object(
        objects,
        reference.content_ref(),
        metadata.size,
        MAX_COMPUTATION_OBJECT_BYTES,
    )?;
    Ok(ResolvedComputation::verify(reference.clone(), &bytes)?)
}

pub fn read_exact_object(
    objects: &dyn ObjectResolver,
    reference: &ContentRef,
    expected_size: u64,
    maximum: u64,
) -> Result<Vec<u8>, ObjectError> {
    if expected_size > maximum {
        return Err(ObjectError::ObjectTooLarge {
            actual: expected_size,
            maximum,
        });
    }
    let mut bytes = Vec::new();
    objects
        .open(reference)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual = bytes.len() as u64;
    if actual != expected_size {
        return Err(ObjectError::SizeMismatch {
            expected: expected_size,
            actual,
        });
    }
    verify_content(reference, &bytes)?;
    Ok(bytes)
}

pub fn verify_content(reference: &ContentRef, bytes: &[u8]) -> Result<(), ObjectError> {
    let actual = match reference.algorithm() {
        "blake3" => blake3_reference(bytes),
        "sha256" => ContentRef::parse(format!("sha256:{:x}", Sha256::digest(bytes)))
            .expect("SHA-256 creates a valid ContentRef"),
        _ => unreachable!("ContentRef parser restricts algorithms"),
    };
    if &actual != reference {
        return Err(ObjectError::IdentityMismatch {
            expected: reference.clone(),
            actual,
        });
    }
    Ok(())
}

pub fn blake3_reference(bytes: &[u8]) -> ContentRef {
    ContentRef::parse(format!("blake3:{}", blake3::hash(bytes).to_hex()))
        .expect("BLAKE3 creates a valid ContentRef")
}

#[derive(Debug, Default)]
pub struct MemoryObjectStore {
    bytes: RwLock<BTreeMap<ContentRef, Vec<u8>>>,
}

impl MemoryObjectStore {
    pub fn contains(&self, reference: &ContentRef) -> bool {
        self.bytes
            .read()
            .expect("memory object store lock poisoned")
            .contains_key(reference)
    }
}

impl ObjectResolver for MemoryObjectStore {
    fn metadata(&self, reference: &ContentRef) -> Result<ObjectMetadata, ObjectError> {
        let bytes = self
            .bytes
            .read()
            .map_err(|error| ObjectError::Storage(error.to_string()))?;
        let bytes = bytes
            .get(reference)
            .ok_or_else(|| ObjectError::NotFound(reference.clone()))?;
        Ok(ObjectMetadata {
            size: bytes.len() as u64,
        })
    }

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send + '_>, ObjectError> {
        let bytes = self
            .bytes
            .read()
            .map_err(|error| ObjectError::Storage(error.to_string()))?
            .get(reference)
            .cloned()
            .ok_or_else(|| ObjectError::NotFound(reference.clone()))?;
        Ok(Box::new(Cursor::new(bytes)))
    }
}

impl ObjectStore for MemoryObjectStore {
    fn insert(&self, reference: &ContentRef, bytes: &[u8]) -> Result<(), ObjectError> {
        verify_content(reference, bytes)?;
        let mut objects = self
            .bytes
            .write()
            .map_err(|error| ObjectError::Storage(error.to_string()))?;
        if let Some(existing) = objects.get(reference) {
            if existing != bytes {
                return Err(ObjectError::IdentityMismatch {
                    expected: reference.clone(),
                    actual: blake3_reference(existing),
                });
            }
            return Ok(());
        }
        objects.insert(reference.clone(), bytes.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ato_computation::{
        ComputationObject, SemanticsId, computation_ref, encode_computation_object,
    };

    use super::*;

    #[test]
    fn store_verifies_content_before_insertion() {
        let store = MemoryObjectStore::default();
        let wrong = ContentRef::parse(format!("blake3:{}", "ab".repeat(32))).unwrap();

        assert!(matches!(
            store.insert(&wrong, b"different"),
            Err(ObjectError::IdentityMismatch { .. })
        ));
        assert!(!store.contains(&wrong));
    }

    #[test]
    fn resolver_constructs_verified_computation_from_stored_bytes() {
        let store = MemoryObjectStore::default();
        let residual = store.put(b"ready").unwrap();
        let object = ComputationObject {
            semantics: SemanticsId::parse("example.test@1").unwrap(),
            boundary: BTreeMap::new(),
            residual,
        };
        let bytes = encode_computation_object(&object).unwrap();
        let reference = computation_ref(&object).unwrap();
        store.insert(reference.content_ref(), &bytes).unwrap();

        let resolved = resolve_computation(&store, &reference).unwrap();

        assert_eq!(resolved.object(), &object);
    }
}
