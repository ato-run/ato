use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use tempfile::Builder;

use crate::capsule_v3::manifest::{parse_blake3_digest, CapsuleManifestV3};
use crate::error::{CapsuleError, Result};

const DEFAULT_CAS_DIR: &str = ".capsule/cas";

#[derive(Debug, Clone)]
pub struct CasStore {
    root: PathBuf,
    objects_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PutChunkResult {
    pub inserted: bool,
    pub path: PathBuf,
    pub zstd_size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct FsckReport {
    pub checked_chunks: usize,
    pub ok_chunks: usize,
    pub missing_chunks: Vec<String>,
    pub size_mismatch_chunks: Vec<String>,
    pub decode_error_chunks: Vec<String>,
    pub hash_mismatch_chunks: Vec<String>,
    pub hard_errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl FsckReport {
    pub fn is_ok(&self) -> bool {
        self.missing_chunks.is_empty()
            && self.size_mismatch_chunks.is_empty()
            && self.decode_error_chunks.is_empty()
            && self.hash_mismatch_chunks.is_empty()
            && self.hard_errors.is_empty()
    }
}

#[derive(Debug)]
enum ChunkIntegrityError {
    Decode(String),
    SizeMismatch { expected: u32, actual: u64 },
    HashMismatch { expected: String, actual: String },
    Io(String),
}

impl CasStore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let objects_dir = root.join("objects").join("blake3");
        fs::create_dir_all(&objects_dir).map_err(|e| {
            CapsuleError::Pack(format!(
                "failed to create CAS objects directory {}: {}",
                objects_dir.display(),
                e
            ))
        })?;
        Ok(Self { root, objects_dir })
    }

    pub fn from_env() -> Result<Self> {
        if let Ok(raw) = env::var("ATO_CAS_ROOT") {
            let value = raw.trim();
            if !value.is_empty() {
                return Self::new(expand_tilde(value));
            }
        }

        if let Some(home) = dirs::home_dir() {
            return Self::new(home.join(DEFAULT_CAS_DIR));
        }

        Self::new(PathBuf::from("/tmp").join(DEFAULT_CAS_DIR))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn chunk_path(&self, raw_hash: &str) -> Result<PathBuf> {
        let hex = parse_blake3_digest(raw_hash)?;
        let shard1 = &hex[0..2];
        let shard2 = &hex[2..4];
        Ok(self
            .objects_dir
            .join(shard1)
            .join(shard2)
            .join(format!("{hex}.zst")))
    }

    pub fn has_chunk(&self, raw_hash: &str) -> Result<bool> {
        Ok(self.chunk_path(raw_hash)?.exists())
    }

    pub fn put_chunk_zstd(&self, raw_hash: &str, zstd_bytes: &[u8]) -> Result<PutChunkResult> {
        self.put_chunk_zstd_inner(raw_hash, zstd_bytes, false)
    }

    fn put_chunk_zstd_inner(
        &self,
        raw_hash: &str,
        zstd_bytes: &[u8],
        fail_before_persist: bool,
    ) -> Result<PutChunkResult> {
        let path = self.chunk_path(raw_hash)?;
        let parent = path.parent().ok_or_else(|| {
            CapsuleError::Pack(format!(
                "failed to resolve parent directory for {}",
                path.display()
            ))
        })?;
        fs::create_dir_all(parent).map_err(|e| {
            CapsuleError::Pack(format!(
                "failed to create CAS shard directory {}: {}",
                parent.display(),
                e
            ))
        })?;

        let mut tmp = Builder::new()
            .prefix(".tmp-")
            .suffix(".zst")
            .tempfile_in(parent)
            .map_err(|e| {
                CapsuleError::Pack(format!(
                    "failed to create temporary CAS file in {}: {}",
                    parent.display(),
                    e
                ))
            })?;

        tmp.write_all(zstd_bytes).map_err(|e| {
            CapsuleError::Pack(format!(
                "failed to write temporary CAS file in {}: {}",
                parent.display(),
                e
            ))
        })?;
        tmp.as_file_mut().sync_all().map_err(|e| {
            CapsuleError::Pack(format!(
                "failed to sync temporary CAS file in {}: {}",
                parent.display(),
                e
            ))
        })?;
        if fail_before_persist {
            return Err(CapsuleError::Pack(
                "injected failure before persist_noclobber".to_string(),
            ));
        }

        match tmp.persist_noclobber(&path) {
            Ok(file) => {
                file.sync_all().map_err(|e| {
                    CapsuleError::Pack(format!("failed to sync CAS file {}: {}", path.display(), e))
                })?;
                drop(file);
                sync_parent_directory(parent)?;
                let zstd_size = fs::metadata(&path).map(|m| m.len()).map_err(|e| {
                    CapsuleError::Pack(format!("failed to stat CAS file {}: {}", path.display(), e))
                })?;
                Ok(PutChunkResult {
                    inserted: true,
                    path,
                    zstd_size,
                })
            }
            Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // No-clobber: another writer already persisted this digest.
                drop(err.file);
                let meta = fs::metadata(&path).map_err(|e| {
                    CapsuleError::Pack(format!(
                        "failed to stat existing CAS file {}: {}",
                        path.display(),
                        e
                    ))
                })?;
                if !meta.is_file() {
                    return Err(CapsuleError::Pack(format!(
                        "existing CAS path is not a file: {}",
                        path.display()
                    )));
                }
                Ok(PutChunkResult {
                    inserted: false,
                    path,
                    zstd_size: meta.len(),
                })
            }
            Err(err) => {
                let temp_path = err.file.path().to_path_buf();
                let io_error = err.error;
                let _ = err.file.close();
                Err(CapsuleError::Pack(format!(
                    "failed to persist CAS chunk to {} (tmp {}): {}",
                    path.display(),
                    temp_path.display(),
                    io_error
                )))
            }
        }
    }

    #[cfg(test)]
    fn put_chunk_zstd_with_injected_failure(
        &self,
        raw_hash: &str,
        zstd_bytes: &[u8],
    ) -> Result<PutChunkResult> {
        self.put_chunk_zstd_inner(raw_hash, zstd_bytes, true)
    }

    pub fn fsck_manifest(&self, manifest: &CapsuleManifestV3) -> Result<FsckReport> {
        manifest.validate_core()?;

        let mut report = FsckReport::default();
        for (index, chunk) in manifest.chunks.iter().enumerate() {
            report.checked_chunks += 1;
            let path = self.chunk_path(&chunk.raw_hash)?;
            if !path.exists() {
                report.missing_chunks.push(chunk.raw_hash.clone());
                report
                    .hard_errors
                    .push(format!("chunk[{index}] missing: {}", chunk.raw_hash));
                continue;
            }

            let zstd_size = fs::metadata(&path).map(|m| m.len()).map_err(|e| {
                CapsuleError::Pack(format!(
                    "failed to stat CAS chunk {} ({}): {}",
                    chunk.raw_hash,
                    path.display(),
                    e
                ))
            })?;
            if let Some(hint) = chunk.zstd_size_hint {
                if zstd_size != hint as u64 {
                    report.warnings.push(format!(
                        "chunk[{index}] zstd_size_hint mismatch: hint={} actual={} ({})",
                        hint, zstd_size, chunk.raw_hash
                    ));
                }
            }

            let verify = verify_chunk_content(&path, chunk.raw_size, &chunk.raw_hash);
            match verify {
                Ok(()) => {
                    report.ok_chunks += 1;
                }
                Err(ChunkIntegrityError::Decode(message)) => {
                    report.decode_error_chunks.push(chunk.raw_hash.clone());
                    report.hard_errors.push(format!(
                        "chunk[{index}] decode error ({}): {}",
                        chunk.raw_hash, message
                    ));
                }
                Err(ChunkIntegrityError::SizeMismatch { expected, actual }) => {
                    report.size_mismatch_chunks.push(chunk.raw_hash.clone());
                    report.hard_errors.push(format!(
                        "chunk[{index}] raw_size mismatch ({}): expected {} got {}",
                        chunk.raw_hash, expected, actual
                    ));
                }
                Err(ChunkIntegrityError::HashMismatch { expected, actual }) => {
                    report.hash_mismatch_chunks.push(chunk.raw_hash.clone());
                    report.hard_errors.push(format!(
                        "chunk[{index}] raw_hash mismatch ({}): expected {} got {}",
                        chunk.raw_hash, expected, actual
                    ));
                }
                Err(ChunkIntegrityError::Io(message)) => {
                    report.hard_errors.push(format!(
                        "chunk[{index}] io error ({}): {}",
                        chunk.raw_hash, message
                    ));
                }
            }
        }

        Ok(report)
    }
}

fn verify_chunk_content(
    path: &Path,
    expected_raw_size: u32,
    expected_raw_hash: &str,
) -> std::result::Result<(), ChunkIntegrityError> {
    let file = fs::File::open(path).map_err(|e| {
        ChunkIntegrityError::Io(format!(
            "failed to open CAS chunk {}: {}",
            path.display(),
            e
        ))
    })?;

    let mut decoder = zstd::Decoder::new(file).map_err(|e| {
        ChunkIntegrityError::Decode(format!(
            "failed to initialize zstd decoder for {}: {}",
            path.display(),
            e
        ))
    })?;

    let mut hasher = blake3::Hasher::new();
    let mut raw_size: u64 = 0;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = decoder.read(&mut buf).map_err(|e| {
            ChunkIntegrityError::Decode(format!(
                "failed to decode zstd chunk {}: {}",
                path.display(),
                e
            ))
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        raw_size += n as u64;
    }

    if raw_size != expected_raw_size as u64 {
        return Err(ChunkIntegrityError::SizeMismatch {
            expected: expected_raw_size,
            actual: raw_size,
        });
    }

    let computed = format!("blake3:{}", hasher.finalize().to_hex());
    if computed != expected_raw_hash {
        return Err(ChunkIntegrityError::HashMismatch {
            expected: expected_raw_hash.to_string(),
            actual: computed,
        });
    }

    Ok(())
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn sync_parent_directory(parent: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let dir = fs::File::open(parent).map_err(|e| {
            CapsuleError::Pack(format!(
                "failed to open CAS parent directory {} for sync: {}",
                parent.display(),
                e
            ))
        })?;
        dir.sync_all().map_err(|e| {
            CapsuleError::Pack(format!(
                "failed to sync CAS parent directory {}: {}",
                parent.display(),
                e
            ))
        })?;
    }

    #[cfg(windows)]
    {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;
    use crate::capsule_v3::manifest::{blake3_digest, CdcParams, ChunkMeta};

    fn compress(data: &[u8]) -> Vec<u8> {
        let mut encoder = zstd::Encoder::new(Vec::new(), 3).unwrap();
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn sample_manifest(chunks: Vec<ChunkMeta>) -> CapsuleManifestV3 {
        let total_raw_size = chunks.iter().map(|chunk| chunk.raw_size as u64).sum();
        CapsuleManifestV3 {
            schema_version: 3,
            artifact_hash:
                "blake3:0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
            cdc_params: CdcParams::default_fastcdc(),
            total_raw_size,
            chunks,
        }
    }

    #[test]
    fn test_cas_path_sharding() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::new(dir.path()).unwrap();
        let digest = "blake3:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let expected = dir
            .path()
            .join("objects")
            .join("blake3")
            .join("e3")
            .join("b0")
            .join("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.zst");
        assert_eq!(store.chunk_path(digest).unwrap(), expected);
    }

    #[test]
    fn test_put_chunk_prevents_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::new(dir.path()).unwrap();
        let raw_a = b"canonical-payload-a";
        let raw_b = b"tampered-payload-b";
        let raw_hash = blake3_digest(raw_a);

        let inserted = store.put_chunk_zstd(&raw_hash, &compress(raw_a)).unwrap();
        assert!(inserted.inserted);

        let overwrite = store.put_chunk_zstd(&raw_hash, &compress(raw_b)).unwrap();
        assert!(!overwrite.inserted);

        let path = store.chunk_path(&raw_hash).unwrap();
        let mut decoder = zstd::Decoder::new(fs::File::open(path).unwrap()).unwrap();
        let mut restored = Vec::new();
        decoder.read_to_end(&mut restored).unwrap();
        assert_eq!(restored, raw_a);
    }

    #[test]
    fn test_concurrent_put_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(CasStore::new(dir.path()).unwrap());
        let raw = b"same-raw-chunk";
        let raw_hash = blake3_digest(raw);
        let zstd = Arc::new(compress(raw));
        let workers = 100usize;
        let start = Arc::new(Barrier::new(workers));

        let mut handles = Vec::new();
        for _ in 0..workers {
            let store = Arc::clone(&store);
            let zstd = Arc::clone(&zstd);
            let start = Arc::clone(&start);
            let raw_hash = raw_hash.clone();
            handles.push(thread::spawn(move || {
                start.wait();
                store.put_chunk_zstd(&raw_hash, &zstd).unwrap()
            }));
        }

        let mut inserted_count = 0usize;
        for handle in handles {
            if handle.join().unwrap().inserted {
                inserted_count += 1;
            }
        }
        assert_eq!(inserted_count, 1);
        assert!(store.has_chunk(&raw_hash).unwrap());

        let shard = store
            .chunk_path(&raw_hash)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        for entry in fs::read_dir(shard).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(!name.starts_with(".tmp-"), "tmp file leaked: {name}");
        }
    }

    #[test]
    fn test_parallel_put_chunk_same_shard_different_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(CasStore::new(dir.path()).unwrap());
        let hash_a = "blake3:aaaabbbb00000000000000000000000000000000000000000000000000000000";
        let hash_b = "blake3:aaaacccc00000000000000000000000000000000000000000000000000000000";
        let bytes_a = Arc::new(compress(&vec![b'a'; 1024 * 1024]));
        let bytes_b = Arc::new(compress(&vec![b'b'; 1024 * 1024]));
        let start = Arc::new(Barrier::new(2));

        let t1 = {
            let store = Arc::clone(&store);
            let bytes = Arc::clone(&bytes_a);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                store.put_chunk_zstd(hash_a, &bytes).unwrap()
            })
        };
        let t2 = {
            let store = Arc::clone(&store);
            let bytes = Arc::clone(&bytes_b);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                store.put_chunk_zstd(hash_b, &bytes).unwrap()
            })
        };

        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();
        assert!(r1.inserted);
        assert!(r2.inserted);
        assert!(store.has_chunk(hash_a).unwrap());
        assert!(store.has_chunk(hash_b).unwrap());
    }

    #[test]
    fn test_put_chunk_cleans_up_tmp_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::new(dir.path()).unwrap();
        let raw = b"tmp-cleanup";
        let raw_hash = blake3_digest(raw);
        let bytes = compress(raw);
        let result = store.put_chunk_zstd_with_injected_failure(&raw_hash, &bytes);
        assert!(result.is_err());

        let shard = store
            .chunk_path(&raw_hash)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let names = fs::read_dir(shard)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(
            names.iter().all(|name| !name.starts_with(".tmp-")),
            "tmp files leaked: {names:?}"
        );
    }

    #[test]
    fn test_fsck_perfect_match() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::new(dir.path()).unwrap();
        let raw_a = b"hello";
        let raw_b = b"world";
        let chunk_a = ChunkMeta {
            raw_hash: blake3_digest(raw_a),
            raw_size: raw_a.len() as u32,
            zstd_size_hint: None,
        };
        let chunk_b = ChunkMeta {
            raw_hash: blake3_digest(raw_b),
            raw_size: raw_b.len() as u32,
            zstd_size_hint: None,
        };
        store
            .put_chunk_zstd(&chunk_a.raw_hash, &compress(raw_a))
            .unwrap();
        store
            .put_chunk_zstd(&chunk_b.raw_hash, &compress(raw_b))
            .unwrap();

        let manifest = sample_manifest(vec![chunk_a, chunk_b]);
        let report = store.fsck_manifest(&manifest).unwrap();
        assert!(report.is_ok());
        assert_eq!(report.ok_chunks, manifest.chunks.len());
        assert!(report.missing_chunks.is_empty());
        assert!(report.size_mismatch_chunks.is_empty());
        assert!(report.decode_error_chunks.is_empty());
        assert!(report.hash_mismatch_chunks.is_empty());
    }

    #[test]
    fn test_fsck_detects_multiple_corruptions() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::new(dir.path()).unwrap();
        let ok_raw = b"ok";
        let missing_raw = b"missing";
        let size_raw = b"size";
        let hash_expected_raw = b"hash-a";
        let hash_actual_raw = b"hash-b";
        let decode_raw = b"decode";

        let ok_chunk = ChunkMeta {
            raw_hash: blake3_digest(ok_raw),
            raw_size: ok_raw.len() as u32,
            zstd_size_hint: None,
        };
        let missing_chunk = ChunkMeta {
            raw_hash: blake3_digest(missing_raw),
            raw_size: missing_raw.len() as u32,
            zstd_size_hint: None,
        };
        let size_chunk = ChunkMeta {
            raw_hash: blake3_digest(size_raw),
            raw_size: (size_raw.len() + 1) as u32,
            zstd_size_hint: None,
        };
        let hash_chunk = ChunkMeta {
            raw_hash: blake3_digest(hash_expected_raw),
            raw_size: hash_actual_raw.len() as u32,
            zstd_size_hint: None,
        };
        let decode_chunk = ChunkMeta {
            raw_hash: blake3_digest(decode_raw),
            raw_size: decode_raw.len() as u32,
            zstd_size_hint: Some(100),
        };
        store
            .put_chunk_zstd(&ok_chunk.raw_hash, &compress(ok_raw))
            .unwrap();
        store
            .put_chunk_zstd(&size_chunk.raw_hash, &compress(size_raw))
            .unwrap();
        store
            .put_chunk_zstd(&hash_chunk.raw_hash, &compress(hash_actual_raw))
            .unwrap();
        let decode_path = store.chunk_path(&decode_chunk.raw_hash).unwrap();
        fs::create_dir_all(decode_path.parent().unwrap()).unwrap();
        fs::write(&decode_path, b"not-zstd").unwrap();

        let manifest = sample_manifest(vec![
            ok_chunk.clone(),
            missing_chunk.clone(),
            size_chunk.clone(),
            hash_chunk.clone(),
            decode_chunk.clone(),
        ]);

        let report = store.fsck_manifest(&manifest).unwrap();
        assert!(!report.is_ok());
        assert_eq!(report.ok_chunks, 1);
        assert!(report.missing_chunks.contains(&missing_chunk.raw_hash));
        assert_eq!(
            report.size_mismatch_chunks,
            vec![size_chunk.raw_hash.clone()]
        );
        assert_eq!(
            report.hash_mismatch_chunks,
            vec![hash_chunk.raw_hash.clone()]
        );
        assert_eq!(
            report.decode_error_chunks,
            vec![decode_chunk.raw_hash.clone()]
        );
    }

    #[test]
    fn test_fsck_treats_zstd_size_hint_mismatch_as_warning() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::new(dir.path()).unwrap();
        let raw = b"abc";
        let zstd = compress(raw);
        let chunk = ChunkMeta {
            raw_hash: blake3_digest(raw),
            raw_size: raw.len() as u32,
            zstd_size_hint: Some((zstd.len() as u32) + 100),
        };
        store.put_chunk_zstd(&chunk.raw_hash, &zstd).unwrap();
        let manifest = sample_manifest(vec![chunk]);

        let report = store.fsck_manifest(&manifest).unwrap();
        assert!(report.is_ok());
        assert_eq!(report.ok_chunks, 1);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.hard_errors.is_empty());
    }

    #[test]
    fn from_env_uses_ato_cas_root() {
        let dir = tempfile::tempdir().unwrap();
        let old = std::env::var_os("ATO_CAS_ROOT");
        std::env::set_var("ATO_CAS_ROOT", dir.path());
        let store = CasStore::from_env().unwrap();
        assert_eq!(store.root(), dir.path());
        match old {
            Some(v) => std::env::set_var("ATO_CAS_ROOT", v),
            None => std::env::remove_var("ATO_CAS_ROOT"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_parent_sync_succeeds_for_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(sync_parent_directory(dir.path()).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn windows_parent_sync_is_best_effort() {
        let non_existing = PathBuf::from(r"C:\not-existing-parent-dir");
        assert!(sync_parent_directory(&non_existing).is_ok());
    }
}
