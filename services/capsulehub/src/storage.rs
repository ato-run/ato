//! Content-addressed ciphertext storage.
//!
//! Objects are stored as opaque bytes under `<storage>/objects/<hex>` where
//! `<hex>` is the SHA-256 of the bytes. Uploads stream into
//! `<storage>/tmp/<random>.partial` while being hashed; a partial object is
//! only renamed into place after the file is synced and the digest matches the
//! requested id. A failed, aborted, or mismatched upload never becomes visible
//! as an object and never leaves a partial file behind.
//!
//! The store never inspects the bytes. There is no decoding, no validation of
//! bundle shape or Capsule identity, and no key material anywhere in this
//! layer.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

/// Directory (under the storage root) that holds complete objects.
pub const OBJECTS_DIRECTORY: &str = "objects";
/// Directory (under the storage root) that holds in-flight uploads.
pub const TEMP_DIRECTORY: &str = "tmp";

static PARTIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("object id must be `sha256:<64 lowercase hex>`")]
    InvalidKey,
    #[error("object exceeds the configured maximum size")]
    TooLarge,
    #[error("storage quota exceeded")]
    QuotaExceeded,
    #[error("uploaded bytes do not match the object id: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("storage I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Hex digest part of a validated `sha256:<hex>` object id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// Strictly parses `sha256:<64 lowercase hex>`.
    pub fn parse(text: &str) -> Result<Self, StorageError> {
        let Some(hex) = text.strip_prefix("sha256:") else {
            return Err(StorageError::InvalidKey);
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(StorageError::InvalidKey);
        }
        Ok(Self(hex.to_owned()))
    }

    /// The 64 lowercase hex characters.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The full `sha256:<hex>` object id.
    pub fn id(&self) -> String {
        format!("sha256:{}", self.0)
    }
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.id())
    }
}

/// Result of a committed upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredObject {
    pub size: u64,
    /// `false` when the object already existed (idempotent re-PUT).
    pub created: bool,
}

/// Result of one retention sweep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub removed_objects: usize,
    pub removed_uploads: usize,
    pub removed_bytes: u64,
}

#[derive(Debug, Default)]
struct Usage {
    used: u64,
    reserved: u64,
}

/// Filesystem blob store with an accounting quota.
#[derive(Debug)]
pub struct BlobStore {
    root: PathBuf,
    max_object_bytes: u64,
    max_storage_bytes: Option<u64>,
    usage: Mutex<Usage>,
}

impl BlobStore {
    /// Creates the storage layout if needed and loads the current usage.
    pub fn open(
        root: impl Into<PathBuf>,
        max_object_bytes: u64,
        max_storage_bytes: Option<u64>,
    ) -> Result<Self, StorageError> {
        let root = root.into();
        fs::create_dir_all(root.join(OBJECTS_DIRECTORY))?;
        fs::create_dir_all(root.join(TEMP_DIRECTORY))?;

        let mut used = 0u64;
        for entry in fs::read_dir(root.join(OBJECTS_DIRECTORY))? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if ObjectKey::parse(&format!("sha256:{name}")).is_ok() {
                used = used.saturating_add(entry.metadata()?.len());
            }
        }

        Ok(Self {
            root,
            max_object_bytes,
            max_storage_bytes,
            usage: Mutex::new(Usage { used, reserved: 0 }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.root.join(OBJECTS_DIRECTORY)
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join(TEMP_DIRECTORY)
    }

    pub fn max_object_bytes(&self) -> u64 {
        self.max_object_bytes
    }

    pub fn max_storage_bytes(&self) -> Option<u64> {
        self.max_storage_bytes
    }

    /// `(committed bytes, bytes reserved by in-flight uploads)`.
    pub fn usage(&self) -> (u64, u64) {
        let usage = self.usage.lock().expect("storage usage lock poisoned");
        (usage.used, usage.reserved)
    }

    pub fn object_path(&self, key: &ObjectKey) -> PathBuf {
        // The path is built only from validated lowercase hex, never from a
        // caller-controlled string.
        self.root.join(OBJECTS_DIRECTORY).join(key.as_str())
    }

    /// Size of a stored object, if present.
    pub async fn metadata(&self, key: &ObjectKey) -> Result<Option<u64>, StorageError> {
        match tokio::fs::metadata(self.object_path(key)).await {
            Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Opens a stored object for streaming, if present.
    pub async fn open_object(
        &self,
        key: &ObjectKey,
    ) -> Result<Option<tokio::fs::File>, StorageError> {
        match tokio::fs::File::open(self.object_path(key)).await {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Starts a streaming upload into a unique partial file.
    pub async fn begin_upload(&self, key: &ObjectKey) -> Result<Upload<'_>, StorageError> {
        let unique = format!(
            "{}-{}-{}.partial",
            std::process::id(),
            now_nanos(),
            PARTIAL_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let temp_path = self.tmp_dir().join(unique);
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await?;
        Ok(Upload {
            store: self,
            key: key.clone(),
            temp_path,
            file: Some(file),
            hasher: Sha256::new(),
            received: 0,
            reserved: 0,
        })
    }

    /// Removes a stored object. Returns `false` when it did not exist.
    pub async fn remove(&self, key: &ObjectKey) -> Result<bool, StorageError> {
        let path = self.object_path(key);
        let size = match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => metadata.len(),
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                self.release_used(size);
                let _ = sync_directory(&self.objects_dir()).await;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Removes objects and abandoned uploads older than `retention` relative
    /// to `now`.
    pub async fn sweep(
        &self,
        retention: Duration,
        now: SystemTime,
    ) -> Result<SweepReport, StorageError> {
        let cutoff = now.checked_sub(retention).unwrap_or(UNIX_EPOCH);
        let mut report = SweepReport::default();

        let mut objects = tokio::fs::read_dir(self.objects_dir()).await?;
        while let Some(entry) = objects.next_entry().await? {
            let metadata = match entry.metadata().await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if !metadata.is_file() {
                continue;
            }
            let modified = match metadata.modified() {
                Ok(modified) => modified,
                Err(_) => continue,
            };
            if modified >= cutoff {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Ok(key) = ObjectKey::parse(&format!("sha256:{name}"))
                && self.remove(&key).await?
            {
                report.removed_objects += 1;
                report.removed_bytes = report.removed_bytes.saturating_add(metadata.len());
            }
        }

        let mut uploads = tokio::fs::read_dir(self.tmp_dir()).await?;
        while let Some(entry) = uploads.next_entry().await? {
            let metadata = match entry.metadata().await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if !metadata.is_file() {
                continue;
            }
            let modified = match metadata.modified() {
                Ok(modified) => modified,
                Err(_) => continue,
            };
            if modified >= cutoff {
                continue;
            }
            if tokio::fs::remove_file(entry.path()).await.is_ok() {
                report.removed_uploads += 1;
            }
        }

        Ok(report)
    }

    fn reserve(&self, bytes: u64) -> Result<(), StorageError> {
        let mut usage = self.usage.lock().expect("storage usage lock poisoned");
        if let Some(maximum) = self.max_storage_bytes
            && usage
                .used
                .saturating_add(usage.reserved)
                .saturating_add(bytes)
                > maximum
        {
            return Err(StorageError::QuotaExceeded);
        }
        usage.reserved = usage.reserved.saturating_add(bytes);
        Ok(())
    }

    fn release_reserved(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let mut usage = self.usage.lock().expect("storage usage lock poisoned");
        usage.reserved = usage.reserved.saturating_sub(bytes);
    }

    fn commit_reserved(&self, bytes: u64) {
        let mut usage = self.usage.lock().expect("storage usage lock poisoned");
        usage.reserved = usage.reserved.saturating_sub(bytes);
        usage.used = usage.used.saturating_add(bytes);
    }

    fn release_used(&self, bytes: u64) {
        let mut usage = self.usage.lock().expect("storage usage lock poisoned");
        usage.used = usage.used.saturating_sub(bytes);
    }
}

/// A single in-flight upload.
///
/// Dropping the upload without calling [`Upload::finish`] removes the partial
/// file and releases its quota reservation.
pub struct Upload<'a> {
    store: &'a BlobStore,
    key: ObjectKey,
    temp_path: PathBuf,
    file: Option<tokio::fs::File>,
    hasher: Sha256,
    received: u64,
    reserved: u64,
}

impl Upload<'_> {
    /// Appends a chunk to the partial object.
    ///
    /// Enforces the per-object size bound and the storage quota before writing
    /// anything, so an oversized or over-quota upload stores nothing.
    pub async fn write(&mut self, data: &[u8]) -> Result<(), StorageError> {
        let additional = data.len() as u64;
        let received = self.received.saturating_add(additional);
        if received > self.store.max_object_bytes {
            return Err(StorageError::TooLarge);
        }
        self.store.reserve(additional)?;
        self.reserved = self.reserved.saturating_add(additional);
        let file = self
            .file
            .as_mut()
            .expect("upload file is open until finish");
        file.write_all(data).await?;
        self.hasher.update(data);
        self.received = received;
        Ok(())
    }

    /// Syncs, verifies the digest, and atomically publishes the object.
    pub async fn finish(mut self) -> Result<StoredObject, StorageError> {
        let file = self.file.take().expect("upload file is open until finish");
        file.sync_all().await?;
        drop(file);

        let digest = hex::encode(self.hasher.finalize_reset());
        if digest != self.key.as_str() {
            return Err(StorageError::DigestMismatch {
                expected: self.key.id(),
                actual: format!("sha256:{digest}"),
            });
        }

        let destination = self.store.object_path(&self.key);
        if tokio::fs::try_exists(&destination).await? {
            return Ok(StoredObject {
                size: self.received,
                created: false,
            });
        }

        match tokio::fs::rename(&self.temp_path, &destination).await {
            Ok(()) => {}
            Err(error) => {
                if tokio::fs::try_exists(&destination).await? {
                    // Lost the race to a concurrent identical PUT.
                    return Ok(StoredObject {
                        size: self.received,
                        created: false,
                    });
                }
                return Err(error.into());
            }
        }

        self.store.commit_reserved(self.reserved);
        self.reserved = 0;
        sync_directory(&self.store.objects_dir()).await?;
        Ok(StoredObject {
            size: self.received,
            created: true,
        })
    }
}

impl Drop for Upload<'_> {
    fn drop(&mut self) {
        // Best-effort cleanup on abort, mismatch, or oversize rejection.
        let _ = std::fs::remove_file(&self.temp_path);
        self.store.release_reserved(self.reserved);
        self.reserved = 0;
    }
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

/// Makes a rename durable. Some platforms/filesystems do not support fsync on
/// a directory handle; that case is not an error for the caller.
async fn sync_directory(path: &Path) -> Result<(), StorageError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> Result<(), StorageError> {
        let directory = fs::File::open(&path)?;
        match directory.sync_all() {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    })
    .await
    .map_err(|error| StorageError::Io(io::Error::other(error.to_string())))??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_for(bytes: &[u8]) -> ObjectKey {
        ObjectKey::parse(&format!("sha256:{}", hex::encode(Sha256::digest(bytes)))).unwrap()
    }

    #[tokio::test]
    async fn uploads_stream_verify_and_publish() {
        let directory = tempfile::tempdir().unwrap();
        let store = BlobStore::open(directory.path(), 1024, None).unwrap();
        let payload = b"opaque ciphertext";
        let key = key_for(payload);

        let mut upload = store.begin_upload(&key).await.unwrap();
        upload.write(&payload[..5]).await.unwrap();
        upload.write(&payload[5..]).await.unwrap();
        let stored = upload.finish().await.unwrap();

        assert!(stored.created);
        assert_eq!(stored.size, payload.len() as u64);
        assert_eq!(
            store.metadata(&key).await.unwrap(),
            Some(payload.len() as u64)
        );
        assert_eq!(store.usage(), (payload.len() as u64, 0));
        assert!(
            tokio::fs::read_dir(store.tmp_dir())
                .await
                .unwrap()
                .next_entry()
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn repeated_uploads_are_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let store = BlobStore::open(directory.path(), 1024, None).unwrap();
        let payload = b"same bytes";
        let key = key_for(payload);

        let mut first = store.begin_upload(&key).await.unwrap();
        first.write(payload).await.unwrap();
        assert!(first.finish().await.unwrap().created);

        let mut second = store.begin_upload(&key).await.unwrap();
        second.write(payload).await.unwrap();
        assert!(!second.finish().await.unwrap().created);

        assert_eq!(store.usage(), (payload.len() as u64, 0));
    }

    #[tokio::test]
    async fn digest_mismatch_stores_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let store = BlobStore::open(directory.path(), 1024, None).unwrap();
        let key = key_for(b"expected bytes");

        let mut upload = store.begin_upload(&key).await.unwrap();
        upload.write(b"different bytes").await.unwrap();
        assert!(matches!(
            upload.finish().await,
            Err(StorageError::DigestMismatch { .. })
        ));
        assert_eq!(store.metadata(&key).await.unwrap(), None);
        assert_eq!(store.usage(), (0, 0));
        assert!(
            tokio::fs::read_dir(store.tmp_dir())
                .await
                .unwrap()
                .next_entry()
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn oversize_upload_is_rejected_before_writing() {
        let directory = tempfile::tempdir().unwrap();
        let store = BlobStore::open(directory.path(), 4, None).unwrap();
        let key = key_for(b"0123456789");

        let mut upload = store.begin_upload(&key).await.unwrap();
        assert!(matches!(
            upload.write(b"01234").await,
            Err(StorageError::TooLarge)
        ));
        drop(upload);
        assert_eq!(store.usage(), (0, 0));
        assert!(
            tokio::fs::read_dir(store.tmp_dir())
                .await
                .unwrap()
                .next_entry()
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn quota_is_enforced_and_released() {
        let directory = tempfile::tempdir().unwrap();
        let store = BlobStore::open(directory.path(), 1024, Some(8)).unwrap();
        let first_key = key_for(b"12345");

        let mut first = store.begin_upload(&first_key).await.unwrap();
        first.write(b"12345").await.unwrap();
        assert_eq!(store.usage(), (0, 5));
        first.finish().await.unwrap();
        assert_eq!(store.usage(), (5, 0));

        let second_key = key_for(b"6789abcd");
        let mut second = store.begin_upload(&second_key).await.unwrap();
        assert!(matches!(
            second.write(b"6789abcd").await,
            Err(StorageError::QuotaExceeded)
        ));
        drop(second);
        assert_eq!(store.usage(), (5, 0));
    }

    #[tokio::test]
    async fn aborted_upload_leaves_no_partial() {
        let directory = tempfile::tempdir().unwrap();
        let store = BlobStore::open(directory.path(), 1024, None).unwrap();
        let key = key_for(b"complete bytes");
        {
            let mut upload = store.begin_upload(&key).await.unwrap();
            upload.write(b"partial").await.unwrap();
        }
        assert_eq!(store.usage(), (0, 0));
        assert!(
            tokio::fs::read_dir(store.tmp_dir())
                .await
                .unwrap()
                .next_entry()
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(store.metadata(&key).await.unwrap(), None);
    }

    #[tokio::test]
    async fn remove_updates_usage() {
        let directory = tempfile::tempdir().unwrap();
        let store = BlobStore::open(directory.path(), 1024, None).unwrap();
        let payload = b"takedown me";
        let key = key_for(payload);
        let mut upload = store.begin_upload(&key).await.unwrap();
        upload.write(payload).await.unwrap();
        upload.finish().await.unwrap();

        assert!(store.remove(&key).await.unwrap());
        assert!(!store.remove(&key).await.unwrap());
        assert_eq!(store.usage(), (0, 0));
    }

    #[tokio::test]
    async fn restart_keeps_objects_and_usage() {
        let directory = tempfile::tempdir().unwrap();
        let payload = b"persisted";
        let key = key_for(payload);
        {
            let store = BlobStore::open(directory.path(), 1024, None).unwrap();
            let mut upload = store.begin_upload(&key).await.unwrap();
            upload.write(payload).await.unwrap();
            upload.finish().await.unwrap();
        }
        let reopened = BlobStore::open(directory.path(), 1024, None).unwrap();
        assert_eq!(
            reopened.metadata(&key).await.unwrap(),
            Some(payload.len() as u64)
        );
        assert_eq!(reopened.usage(), (payload.len() as u64, 0));
    }

    #[tokio::test]
    async fn sweep_removes_objects_and_stale_partials_past_retention() {
        let directory = tempfile::tempdir().unwrap();
        let store = BlobStore::open(directory.path(), 1024, None).unwrap();
        let payload = b"expire me";
        let key = key_for(payload);
        let mut upload = store.begin_upload(&key).await.unwrap();
        upload.write(payload).await.unwrap();
        upload.finish().await.unwrap();

        let stale = store.tmp_dir().join("stale.partial");
        tokio::fs::write(&stale, b"partial").await.unwrap();

        let now = SystemTime::now();
        let report = store.sweep(Duration::from_secs(60), now).await.unwrap();
        assert_eq!(report, SweepReport::default());

        let later = now + Duration::from_secs(120);
        let report = store.sweep(Duration::from_secs(60), later).await.unwrap();
        assert_eq!(report.removed_objects, 1);
        assert_eq!(report.removed_uploads, 1);
        assert_eq!(report.removed_bytes, payload.len() as u64);
        assert_eq!(store.usage(), (0, 0));
        assert_eq!(store.metadata(&key).await.unwrap(), None);
    }

    #[test]
    fn object_keys_reject_non_canonical_ids() {
        assert!(ObjectKey::parse(&format!("sha256:{}", "a".repeat(64))).is_ok());
        for value in [
            "a".repeat(64),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("blake3:{}", "a".repeat(64)),
            format!("sha256:{}", "g".repeat(64)),
            "sha256:../escape".to_owned(),
        ] {
            assert!(
                ObjectKey::parse(&value).is_err(),
                "{value} must be rejected"
            );
        }
    }
}
