//! Local content-addressed store: immutable chunk blobs on the filesystem.
//!
//! Layout: `<root>/blobs/blake3/<hex>`. A chunk is written once (its content
//! address is its name); re-putting identical bytes is a no-op. Every read
//! re-hashes and fails closed on mismatch — a corrupt or tampered chunk is
//! never returned.

use std::fs;
use std::path::{Path, PathBuf};

use crate::hash::{ContentHash, hash_bytes};
use crate::{CapsuleFsError, Result};

/// A filesystem-backed content-addressed chunk store.
#[derive(Debug, Clone)]
pub struct CasStore {
    root: PathBuf,
}

impl CasStore {
    /// Open (creating if needed) a store rooted at `root`. The `blobs/blake3`
    /// directory is created eagerly so callers never race on first write.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("blobs").join("blake3"))?;
        Ok(Self { root })
    }

    /// The store root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// On-disk path for a content hash (whether or not it exists).
    ///
    /// Safe against path traversal by construction: [`ContentHash`] is a
    /// validated type whose `hex()` is always exactly 64 lowercase-hex
    /// characters — a single path component with no separators or `..`. A
    /// hostile manifest can never produce a `ContentHash` that escapes the CAS
    /// root, so this join cannot leave `<root>/blobs/blake3/`.
    fn chunk_path(&self, hash: &ContentHash) -> PathBuf {
        self.root.join("blobs").join("blake3").join(hash.hex())
    }

    /// Whether a chunk is present.
    pub fn has_chunk(&self, hash: &ContentHash) -> bool {
        self.chunk_path(hash).exists()
    }

    /// Store chunk bytes, returning their content address. Idempotent: if the
    /// chunk already exists it is not rewritten. The write is atomic (write to a
    /// temp sibling then rename) so a concurrent reader never sees a partial
    /// chunk.
    pub fn put_chunk(&self, bytes: &[u8]) -> Result<ContentHash> {
        let hash = hash_bytes(bytes);
        let path = self.chunk_path(&hash);
        if path.exists() {
            return Ok(hash);
        }
        // Temp name is content-derived + pid to avoid collisions without needing
        // a random source (kept dependency-free).
        let tmp = path.with_extension(crate::unique_tmp_suffix());
        fs::write(&tmp, bytes)?;
        // rename is atomic on the same filesystem; if another writer won the
        // race the destination already exists and rename still yields the
        // correct content (identical bytes), so ignore AlreadyExists.
        match fs::rename(&tmp, &path) {
            Ok(()) => {}
            Err(_) if path.exists() => {
                let _ = fs::remove_file(&tmp);
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e.into());
            }
        }
        Ok(hash)
    }

    /// Read chunk bytes, verifying integrity. Errors with [`CapsuleFsError::MissingChunk`]
    /// if absent, or [`CapsuleFsError::IntegrityMismatch`] if the stored bytes no
    /// longer hash to `hash`.
    pub fn get_chunk(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let path = self.chunk_path(hash);
        if !path.exists() {
            return Err(CapsuleFsError::MissingChunk(hash.clone()));
        }
        let bytes = fs::read(&path)?;
        let actual = hash_bytes(&bytes);
        if &actual != hash {
            return Err(CapsuleFsError::IntegrityMismatch {
                expected: hash.clone(),
                actual,
            });
        }
        Ok(bytes)
    }

    /// All chunk hashes currently resident in the store. Used by GC to compute
    /// the unreferenced set.
    pub fn list_chunks(&self) -> Result<Vec<ContentHash>> {
        let dir = self.root.join("blobs").join("blake3");
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Skip in-flight temp files (`<hex>.tmp.<pid>`).
            if name.contains(".tmp.") {
                continue;
            }
            // Validate before admitting: a stray non-chunk file in the CAS dir
            // must never become a ContentHash the rest of the system trusts.
            match ContentHash::parse(&format!("blake3:{name}")) {
                Ok(h) => out.push(h),
                Err(_) => continue,
            }
        }
        Ok(out)
    }

    /// Remove a chunk. Returns the number of bytes reclaimed (0 if absent).
    /// Used only by GC.
    pub(crate) fn remove_chunk(&self, hash: &ContentHash) -> Result<u64> {
        let path = self.chunk_path(hash);
        match fs::metadata(&path) {
            Ok(meta) => {
                let len = meta.len();
                fs::remove_file(&path)?;
                Ok(len)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let h = store.put_chunk(b"hello capsulefs").unwrap();
        assert!(store.has_chunk(&h));
        assert_eq!(store.get_chunk(&h).unwrap(), b"hello capsulefs");
    }

    #[test]
    fn put_is_idempotent_and_content_addressed() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let h1 = store.put_chunk(b"same").unwrap();
        let h2 = store.put_chunk(b"same").unwrap();
        assert_eq!(h1, h2);
        assert_eq!(store.list_chunks().unwrap().len(), 1);
    }

    #[test]
    fn missing_chunk_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let absent = hash_bytes(b"never stored");
        assert!(matches!(
            store.get_chunk(&absent),
            Err(CapsuleFsError::MissingChunk(_))
        ));
    }

    #[test]
    fn corrupted_chunk_fails_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let h = store.put_chunk(b"original").unwrap();
        // Corrupt the on-disk bytes behind the content address.
        let path = store.chunk_path(&h);
        std::fs::write(&path, b"tampered").unwrap();
        assert!(matches!(
            store.get_chunk(&h),
            Err(CapsuleFsError::IntegrityMismatch { .. })
        ));
    }
}
