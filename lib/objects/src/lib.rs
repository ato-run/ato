//! Persistence and verified loading for addressable Ato objects.
//!
//! This crate stores bytes and traverses closures. Semantic identity remains
//! defined exclusively by `ato-computation`.

#![forbid(unsafe_code)]

mod bundle;

pub use bundle::{
    BUNDLE_VERSION, BundleError, BundleIndex, BundleObjectDescriptor, BundleObjectKind,
    CapsuleBundle, ComputationReferences, ObjectLink, ReferenceRegistry, bundle_root,
    decode_bundle, encode_bundle, export_bundle, import_bundle, sign_bundle,
};

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcReport {
    pub retained: usize,
    pub removed: usize,
}

#[derive(Debug, Error)]
pub enum GcError {
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error(transparent)]
    Objects(#[from] ObjectError),
    #[error("object GC I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("object store contains an invalid object filename: {0}")]
    InvalidReference(String),
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

/// Filesystem-backed content-addressed object store.
///
/// Parsed references map to `<root>/<algorithm>/<digest>`; caller-controlled
/// paths are never joined directly into the store root.
#[derive(Debug, Clone)]
pub struct FsObjectStore {
    root: PathBuf,
}

impl FsObjectStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ObjectError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, reference: &ContentRef) -> PathBuf {
        let (_, digest) = reference
            .as_str()
            .split_once(':')
            .expect("ContentRef always contains an algorithm separator");
        self.root.join(reference.algorithm()).join(digest)
    }

    /// Removes objects outside the closure of the supplied computation roots
    /// and explicitly retained provider materializations.
    pub fn gc(
        &self,
        roots: &[ComputationRef],
        retained_materializations: &[ContentRef],
        references: &ReferenceRegistry,
    ) -> Result<GcReport, GcError> {
        let mut retained: std::collections::BTreeSet<ContentRef> =
            retained_materializations.iter().cloned().collect();
        for root in roots {
            retained.extend(bundle::closure(root, self, references)?.into_keys());
        }
        let mut removed = 0;
        for algorithm in ["blake3", "sha256"] {
            let directory = self.root.join(algorithm);
            if !directory.is_dir() {
                continue;
            }
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let reference = ContentRef::parse(format!(
                    "{algorithm}:{}",
                    entry.file_name().to_string_lossy()
                ))
                .map_err(|error| GcError::InvalidReference(error.to_string()))?;
                if !retained.contains(&reference) {
                    fs::remove_file(entry.path())?;
                    removed += 1;
                }
            }
        }
        Ok(GcReport {
            retained: retained.len(),
            removed,
        })
    }
}

impl ObjectResolver for FsObjectStore {
    fn metadata(&self, reference: &ContentRef) -> Result<ObjectMetadata, ObjectError> {
        let metadata = fs::metadata(self.path(reference)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ObjectError::NotFound(reference.clone())
            } else {
                ObjectError::Io(error)
            }
        })?;
        Ok(ObjectMetadata {
            size: metadata.len(),
        })
    }

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send + '_>, ObjectError> {
        let file = fs::File::open(self.path(reference)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ObjectError::NotFound(reference.clone())
            } else {
                ObjectError::Io(error)
            }
        })?;
        Ok(Box::new(file))
    }
}

impl ObjectStore for FsObjectStore {
    fn insert(&self, reference: &ContentRef, bytes: &[u8]) -> Result<(), ObjectError> {
        verify_content(reference, bytes)?;
        let path = self.path(reference);
        if path.is_file() {
            return verify_content(reference, &fs::read(path)?);
        }
        fs::create_dir_all(path.parent().expect("object path has a parent"))?;
        let temporary = path.with_extension(format!("new-{}", std::process::id()));
        fs::write(&temporary, bytes)?;
        match fs::rename(&temporary, &path) {
            Ok(()) => Ok(()),
            Err(_error) if path.is_file() => {
                let _ = fs::remove_file(&temporary);
                verify_content(reference, &fs::read(path)?)
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(ObjectError::Io(error))
            }
        }
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

    #[test]
    fn filesystem_store_round_trips_verified_objects() {
        let directory = tempfile::tempdir().unwrap();
        let store = FsObjectStore::open(directory.path()).unwrap();
        let reference = store.put(b"persistent").unwrap();

        assert_eq!(
            read_exact_object(&store, &reference, 10, 10).unwrap(),
            b"persistent"
        );
        assert!(store.path(&reference).starts_with(directory.path()));
    }

    #[test]
    fn filesystem_gc_keeps_roots_and_removes_unreachable_objects() {
        struct NoReferences(SemanticsId);
        impl ComputationReferences for NoReferences {
            fn semantics(&self) -> &SemanticsId {
                &self.0
            }

            fn outgoing(
                &self,
                _computation: &ResolvedComputation,
                _objects: &dyn ObjectResolver,
            ) -> Result<Vec<ObjectLink>, BundleError> {
                Ok(Vec::new())
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let store = FsObjectStore::open(directory.path()).unwrap();
        let residual = store.put(b"kept residual").unwrap();
        let semantics = SemanticsId::parse("example.gc@1").unwrap();
        let object = ComputationObject {
            semantics: semantics.clone(),
            boundary: BTreeMap::new(),
            residual,
        };
        let bytes = encode_computation_object(&object).unwrap();
        let root = computation_ref(&object).unwrap();
        store.insert(root.content_ref(), &bytes).unwrap();
        let unreachable = store.put(b"remove me").unwrap();
        let mut registry = ReferenceRegistry::default();
        registry
            .register(std::sync::Arc::new(NoReferences(semantics)))
            .unwrap();

        let report = store.gc(&[root], &[], &registry).unwrap();

        assert_eq!(report.removed, 1);
        assert!(matches!(
            store.metadata(&unreachable),
            Err(ObjectError::NotFound(_))
        ));
    }
}
