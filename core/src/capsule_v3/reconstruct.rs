use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::capsule_v3::{
    verify_artifact_hash, CapsuleManifestV3, CasDisableReason, CasProvider, CasStore,
    V3_PAYLOAD_MANIFEST_PATH,
};
use crate::error::{CapsuleError, Result};

const V3_STAGING_PREFIX: &str = ".payload-v3-staging-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadUnpackOutcome {
    RestoredFromV3,
    RestoredFromV2,
    RestoredFromV2DueToCasDisabled(CasDisableReason),
    RestoredFromV2DueToV3Error(String),
}

pub fn unpack_payload_from_capsule_root(capsule_root: &Path, out_dir: &Path) -> Result<()> {
    let cas_provider = CasProvider::from_env();
    let _ = unpack_payload_from_capsule_root_with_provider(capsule_root, out_dir, &cas_provider)?;
    Ok(())
}

pub fn unpack_payload_from_v3_manifest(capsule_root: &Path, out_dir: &Path) -> Result<bool> {
    let manifest_path = capsule_root.join(V3_PAYLOAD_MANIFEST_PATH);
    if !manifest_path.exists() {
        return Ok(false);
    }
    let cas = CasStore::from_env()?;
    unpack_payload_from_manifest_file_with_cas(&manifest_path, out_dir, cas)
}

pub fn unpack_payload_from_capsule_root_with_provider(
    capsule_root: &Path,
    out_dir: &Path,
    cas_provider: &CasProvider,
) -> Result<PayloadUnpackOutcome> {
    let manifest_path = capsule_root.join(V3_PAYLOAD_MANIFEST_PATH);
    let payload_zst_path = capsule_root.join("payload.tar.zst");

    if manifest_path.exists() {
        match cas_provider {
            CasProvider::Enabled(cas) => {
                match unpack_payload_from_manifest_file_with_cas(
                    &manifest_path,
                    out_dir,
                    cas.clone(),
                ) {
                    Ok(true) => return Ok(PayloadUnpackOutcome::RestoredFromV3),
                    Ok(false) => {}
                    Err(v3_err) => {
                        if payload_zst_path.exists() {
                            unpack_payload_from_v2_tar_zst(&payload_zst_path, out_dir)?;
                            return Ok(PayloadUnpackOutcome::RestoredFromV2DueToV3Error(
                                v3_err.to_string(),
                            ));
                        }
                        return Err(v3_err);
                    }
                }
            }
            CasProvider::Disabled(reason) => {
                CasProvider::log_disabled_once("payload_unpack", reason);
                if payload_zst_path.exists() {
                    unpack_payload_from_v2_tar_zst(&payload_zst_path, out_dir)?;
                    return Ok(PayloadUnpackOutcome::RestoredFromV2DueToCasDisabled(
                        reason.clone(),
                    ));
                }
                return Err(CapsuleError::Pack(format!(
                    "payload restore failed: {} exists but payload.tar.zst is missing and CAS is disabled ({})",
                    V3_PAYLOAD_MANIFEST_PATH, reason
                )));
            }
        }
    }

    unpack_payload_from_v2_tar_zst(&payload_zst_path, out_dir)?;
    Ok(PayloadUnpackOutcome::RestoredFromV2)
}

#[cfg(test)]
fn unpack_payload_from_capsule_root_with_cas(
    capsule_root: &Path,
    out_dir: &Path,
    cas_override: Option<CasStore>,
) -> Result<()> {
    let manifest_path = capsule_root.join(V3_PAYLOAD_MANIFEST_PATH);
    let unpacked_v3 = if manifest_path.exists() {
        match cas_override {
            Some(cas) => unpack_payload_from_manifest_file_with_cas(&manifest_path, out_dir, cas)?,
            None => unpack_payload_from_v3_manifest(capsule_root, out_dir)?,
        }
    } else {
        false
    };
    if unpacked_v3 {
        return Ok(());
    }

    let payload_zst_path = capsule_root.join("payload.tar.zst");
    unpack_payload_from_v2_tar_zst(&payload_zst_path, out_dir)
}

fn unpack_payload_from_v2_tar_zst(payload_zst_path: &Path, out_dir: &Path) -> Result<()> {
    if !payload_zst_path.exists() {
        return Err(CapsuleError::Pack(format!(
            "payload restore failed: both {} and payload.tar.zst are missing",
            V3_PAYLOAD_MANIFEST_PATH
        )));
    }
    let file = fs::File::open(payload_zst_path).map_err(|e| {
        CapsuleError::Pack(format!(
            "failed to open payload.tar.zst at {}: {}",
            payload_zst_path.display(),
            e
        ))
    })?;
    let decoder = zstd::stream::Decoder::new(file).map_err(|e| {
        CapsuleError::Pack(format!(
            "failed to create zstd decoder for payload.tar.zst at {}: {}",
            payload_zst_path.display(),
            e
        ))
    })?;
    let mut payload = tar::Archive::new(decoder);
    payload.unpack(out_dir).map_err(CapsuleError::Io)?;
    Ok(())
}

fn unpack_payload_from_manifest_file_with_cas(
    manifest_path: &Path,
    out_dir: &Path,
    cas: CasStore,
) -> Result<bool> {
    if !manifest_path.exists() {
        return Ok(false);
    }

    let manifest = read_and_verify_manifest(manifest_path)?;
    let staging = tempfile::Builder::new()
        .prefix(V3_STAGING_PREFIX)
        .tempdir_in(out_dir)
        .map_err(|e| {
            CapsuleError::Pack(format!(
                "failed to create payload v3 staging directory in {}: {}",
                out_dir.display(),
                e
            ))
        })?;

    let unpack_result = (|| {
        let reader = ChainedChunkReader::new(cas, &manifest)?;
        let mut payload = tar::Archive::new(reader);
        payload.unpack(staging.path()).map_err(CapsuleError::Io)?;
        move_staged_payload(staging.path(), out_dir)?;
        Ok(())
    })();

    if let Err(err) = unpack_result {
        let _ = fs::remove_dir_all(staging.path());
        return Err(err);
    }

    staging.close().map_err(|e| {
        CapsuleError::Pack(format!(
            "failed to cleanup payload v3 staging directory {}: {}",
            out_dir.display(),
            e
        ))
    })?;

    Ok(true)
}

fn read_and_verify_manifest(manifest_path: &Path) -> Result<CapsuleManifestV3> {
    let manifest_bytes = fs::read(manifest_path).map_err(|e| {
        CapsuleError::Pack(format!(
            "failed to read payload v3 manifest {}: {}",
            manifest_path.display(),
            e
        ))
    })?;
    let manifest: CapsuleManifestV3 = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        CapsuleError::Pack(format!(
            "failed to parse payload v3 manifest {}: {}",
            manifest_path.display(),
            e
        ))
    })?;
    manifest.validate_core()?;
    verify_artifact_hash(&manifest)?;
    Ok(manifest)
}

fn move_staged_payload(staging_root: &Path, out_dir: &Path) -> Result<()> {
    let mut moved_paths = Vec::new();
    let move_result = (|| {
        for entry in fs::read_dir(staging_root).map_err(|e| {
            CapsuleError::Pack(format!(
                "failed to read staging directory {}: {}",
                staging_root.display(),
                e
            ))
        })? {
            let entry = entry.map_err(CapsuleError::Io)?;
            let src = entry.path();
            let dest = out_dir.join(entry.file_name());
            if dest.exists() {
                return Err(CapsuleError::Pack(format!(
                    "payload restore collision: destination already exists {}",
                    dest.display()
                )));
            }
            fs::rename(&src, &dest).map_err(|e| {
                CapsuleError::Pack(format!(
                    "failed to move staged payload {} -> {}: {}",
                    src.display(),
                    dest.display(),
                    e
                ))
            })?;
            moved_paths.push(dest);
        }
        Ok(())
    })();

    if let Err(err) = move_result {
        for moved in moved_paths.into_iter().rev() {
            remove_path_quietly(&moved);
        }
        return Err(err);
    }
    Ok(())
}

fn remove_path_quietly(path: &Path) {
    let _ = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
}

#[derive(Debug, Clone)]
struct ChunkReadSpec {
    raw_hash: String,
    raw_size: u32,
    path: PathBuf,
}

struct ChainedChunkReader {
    chunk_specs: Vec<ChunkReadSpec>,
    next_index: usize,
    current_index: Option<usize>,
    current_decoder: Option<zstd::Decoder<'static, std::io::BufReader<fs::File>>>,
    current_hasher: Option<blake3::Hasher>,
    current_raw_read: u64,
}

impl ChainedChunkReader {
    fn new(cas: CasStore, manifest: &CapsuleManifestV3) -> Result<Self> {
        let mut chunk_specs = Vec::with_capacity(manifest.chunks.len());
        for chunk in &manifest.chunks {
            let path = cas.chunk_path(&chunk.raw_hash)?;
            if !path.exists() {
                return Err(CapsuleError::Pack(format!(
                    "v3 payload chunk missing in local CAS: {} ({})",
                    chunk.raw_hash,
                    path.display()
                )));
            }
            chunk_specs.push(ChunkReadSpec {
                raw_hash: chunk.raw_hash.clone(),
                raw_size: chunk.raw_size,
                path,
            });
        }

        Ok(Self {
            chunk_specs,
            next_index: 0,
            current_index: None,
            current_decoder: None,
            current_hasher: None,
            current_raw_read: 0,
        })
    }

    fn current_spec(&self) -> Option<&ChunkReadSpec> {
        self.current_index.and_then(|idx| self.chunk_specs.get(idx))
    }

    fn open_next_decoder(&mut self) -> std::io::Result<bool> {
        if self.next_index >= self.chunk_specs.len() {
            self.current_index = None;
            self.current_decoder = None;
            self.current_hasher = None;
            self.current_raw_read = 0;
            return Ok(false);
        }

        let spec = self
            .chunk_specs
            .get(self.next_index)
            .expect("index checked above");
        let file = fs::File::open(&spec.path).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "failed to open v3 payload chunk {} at {}: {}",
                    spec.raw_hash,
                    spec.path.display(),
                    e
                ),
            )
        })?;
        let decoder = zstd::Decoder::new(file).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "failed to create zstd decoder for v3 payload chunk {} at {}: {}",
                    spec.raw_hash,
                    spec.path.display(),
                    e
                ),
            )
        })?;
        self.current_index = Some(self.next_index);
        self.current_decoder = Some(decoder);
        self.current_hasher = Some(blake3::Hasher::new());
        self.current_raw_read = 0;
        self.next_index += 1;
        Ok(true)
    }

    fn finalize_current_chunk(&mut self) -> std::io::Result<()> {
        let Some(spec) = self.current_spec().cloned() else {
            return Ok(());
        };
        let computed_hash = format!(
            "blake3:{}",
            self.current_hasher
                .take()
                .unwrap_or_default()
                .finalize()
                .to_hex()
        );
        if self.current_raw_read != spec.raw_size as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "v3 payload chunk raw size mismatch for {}: expected {} got {}",
                    spec.raw_hash, spec.raw_size, self.current_raw_read
                ),
            ));
        }
        if computed_hash != spec.raw_hash {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "v3 payload chunk hash mismatch for {}: expected {} got {}",
                    spec.raw_hash, spec.raw_hash, computed_hash
                ),
            ));
        }

        self.current_index = None;
        self.current_decoder = None;
        self.current_hasher = None;
        self.current_raw_read = 0;
        Ok(())
    }
}

impl Read for ChainedChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.current_decoder.is_none() && !self.open_next_decoder()? {
                return Ok(0);
            }

            let decoder = match self.current_decoder.as_mut() {
                Some(decoder) => decoder,
                None => continue,
            };
            let read = decoder.read(buf).map_err(|e| {
                let prefix = self
                    .current_spec()
                    .map(|spec| format!("v3 payload chunk {} decode failed", spec.raw_hash))
                    .unwrap_or_else(|| "v3 payload chunk decode failed".to_string());
                std::io::Error::new(e.kind(), format!("{prefix}: {e}"))
            })?;
            if read > 0 {
                if let Some(hasher) = self.current_hasher.as_mut() {
                    hasher.update(&buf[..read]);
                }
                self.current_raw_read += read as u64;
                return Ok(read);
            }

            self.finalize_current_chunk()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use tar;
    use tempfile;
    use zstd;

    use super::{
        unpack_payload_from_capsule_root_with_cas, unpack_payload_from_capsule_root_with_provider,
        unpack_payload_from_manifest_file_with_cas, unpack_payload_from_v3_manifest,
        ChainedChunkReader, PayloadUnpackOutcome, V3_PAYLOAD_MANIFEST_PATH, V3_STAGING_PREFIX,
    };
    use crate::capsule_v3::manifest::{blake3_digest, CapsuleManifestV3, CdcParams, ChunkMeta};
    use crate::capsule_v3::set_artifact_hash;

    use crate::capsule_v3::{CasDisableReason, CasProvider, CasStore};

    use std::collections::BTreeMap;

    use std::fs;

    use std::path::{Path, PathBuf};

    fn compress(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = zstd::Encoder::new(Vec::new(), 3).unwrap();
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn build_manifest(chunks: Vec<ChunkMeta>) -> CapsuleManifestV3 {
        let mut manifest = CapsuleManifestV3 {
            schema_version: 3,
            artifact_hash:
                "blake3:0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
            cdc_params: CdcParams::default_fastcdc(),
            total_raw_size: chunks.iter().map(|c| c.raw_size as u64).sum(),
            chunks,
        };
        set_artifact_hash(&mut manifest).unwrap();
        manifest
    }

    fn create_small_tar(tar_path: &Path) {
        let mut file = fs::File::create(tar_path).unwrap();
        let mut builder = tar::Builder::new(&mut file);
        append_regular(&mut builder, "source/a.txt", b"aaa");
        append_regular(&mut builder, "source/b.txt", b"bbb");
        builder.finish().unwrap();
    }

    fn create_large_tar(tar_path: &Path, size_bytes: u64) {
        let mut file = fs::File::create(tar_path).unwrap();
        let mut builder = tar::Builder::new(&mut file);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(size_bytes);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        let mut repeated = std::io::repeat(0u8).take(size_bytes);
        builder
            .append_data(&mut header, "source/large.bin", &mut repeated)
            .unwrap();
        builder.finish().unwrap();
    }

    fn append_regular(builder: &mut tar::Builder<&mut fs::File>, path: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        builder.append_data(&mut header, path, data).unwrap();
    }

    fn write_v2_payload_from_tar(tar_path: &Path, v2_root: &Path) {
        let mut src = fs::File::open(tar_path).unwrap();
        let dst = fs::File::create(v2_root.join("payload.tar.zst")).unwrap();
        let mut encoder = zstd::Encoder::new(dst, 3).unwrap();
        std::io::copy(&mut src, &mut encoder).unwrap();
        encoder.finish().unwrap();
    }

    fn build_v3_from_tar(
        tar_path: &Path,
        cas: &CasStore,
        chunk_size: usize,
    ) -> (CapsuleManifestV3, Vec<String>) {
        let mut file = fs::File::open(tar_path).unwrap();
        let mut buf = vec![0u8; chunk_size];
        let mut chunks = Vec::new();
        let mut hashes = Vec::new();
        loop {
            let n = file.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            let raw = &buf[..n];
            let hash = blake3_digest(raw);
            let compressed = compress(raw);
            cas.put_chunk_zstd(&hash, &compressed).unwrap();
            hashes.push(hash.clone());
            chunks.push(ChunkMeta {
                raw_hash: hash,
                raw_size: n as u32,
                zstd_size_hint: Some(compressed.len() as u32),
            });
        }
        (build_manifest(chunks), hashes)
    }

    fn write_manifest(root: &Path, manifest: &CapsuleManifestV3) {
        let bytes = serde_jcs::to_vec(manifest).unwrap();
        fs::write(root.join(V3_PAYLOAD_MANIFEST_PATH), bytes).unwrap();
    }

    fn snapshot_files(dir: &Path) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current) = stack.pop() {
            for entry in fs::read_dir(&current).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let rel = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = fs::read(&path).unwrap();
                map.insert(rel, blake3_digest(&bytes));
            }
        }
        map
    }

    #[test]
    fn chained_reader_restores_raw_stream_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let cas = CasStore::new(tmp.path()).unwrap();
        let first = b"hello-";
        let second = b"world";
        let h1 = blake3_digest(first);
        let h2 = blake3_digest(second);
        cas.put_chunk_zstd(&h1, &compress(first)).unwrap();
        cas.put_chunk_zstd(&h2, &compress(second)).unwrap();

        let manifest = build_manifest(vec![
            ChunkMeta {
                raw_hash: h1,
                raw_size: first.len() as u32,
                zstd_size_hint: None,
            },
            ChunkMeta {
                raw_hash: h2,
                raw_size: second.len() as u32,
                zstd_size_hint: None,
            },
        ]);

        let mut reader = ChainedChunkReader::new(cas, &manifest).unwrap();
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"hello-world");
    }

    #[test]
    fn test_v3_untar_fails_closed_on_missing_chunk() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let cas = CasStore::new(root.path().join("cas")).unwrap();
        let tar_path = root.path().join("payload.tar");
        create_small_tar(&tar_path);
        let (manifest, hashes) = build_v3_from_tar(&tar_path, &cas, 1024);
        write_manifest(root.path(), &manifest);

        let mid = hashes[hashes.len() / 2].clone();
        let mid_path = cas.chunk_path(&mid).unwrap();
        fs::remove_file(mid_path).unwrap();

        let err = unpack_payload_from_manifest_file_with_cas(
            &root.path().join(V3_PAYLOAD_MANIFEST_PATH),
            out.path(),
            cas,
        )
        .unwrap_err();
        assert!(err.to_string().contains("chunk missing"));
        assert!(!out.path().join("source").exists());
    }

    #[test]
    fn test_v3_untar_fails_closed_on_corrupted_chunk() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let cas = CasStore::new(root.path().join("cas")).unwrap();
        let tar_path = root.path().join("payload.tar");
        create_small_tar(&tar_path);
        let (manifest, hashes) = build_v3_from_tar(&tar_path, &cas, 1024);
        write_manifest(root.path(), &manifest);

        let target = hashes[hashes.len() / 2].clone();
        let target_path = cas.chunk_path(&target).unwrap();
        let mut bytes = fs::read(&target_path).unwrap();
        if !bytes.is_empty() {
            bytes[0] ^= 0xFF;
        }
        fs::write(&target_path, bytes).unwrap();

        let err = unpack_payload_from_manifest_file_with_cas(
            &root.path().join(V3_PAYLOAD_MANIFEST_PATH),
            out.path(),
            cas,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("decode failed")
                || err.to_string().contains("raw size mismatch")
                || err.to_string().contains("hash mismatch")
                || err.to_string().contains("v3 payload chunk")
                || err.to_string().contains("failed to iterate over archive")
        );
        assert!(!out.path().join("source").exists());
        let staging_leftover = fs::read_dir(out.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(V3_STAGING_PREFIX)
            });
        assert!(!staging_leftover);
    }

    #[test]
    fn test_v3_untar_is_pure_streaming() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let cas = CasStore::new(root.path().join("cas")).unwrap();
        let tar_path = root.path().join("payload.tar");
        create_large_tar(&tar_path, 100 * 1024 * 1024);
        let (manifest, _) = build_v3_from_tar(&tar_path, &cas, 4 * 1024 * 1024);
        write_manifest(root.path(), &manifest);

        unpack_payload_from_manifest_file_with_cas(
            &root.path().join(V3_PAYLOAD_MANIFEST_PATH),
            out.path(),
            cas,
        )
        .unwrap();

        let extracted = out.path().join("source/large.bin");
        assert!(extracted.exists());
        assert_eq!(fs::metadata(&extracted).unwrap().len(), 100 * 1024 * 1024);
        assert!(!out.path().join("payload.tar").exists());
        let staging_leftover = fs::read_dir(out.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(V3_STAGING_PREFIX)
            });
        assert!(!staging_leftover);

        let large_files = fs::read_dir(out.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .flat_map(|path| {
                if path.is_dir() {
                    fs::read_dir(path)
                        .into_iter()
                        .flatten()
                        .filter_map(|entry| entry.ok().map(|v| v.path()))
                        .collect::<Vec<_>>()
                } else {
                    vec![path]
                }
            })
            .filter(|path| path.is_file())
            .filter(|path| {
                fs::metadata(path)
                    .map(|m| m.len() > 32 * 1024 * 1024)
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(large_files, 1);
    }

    #[test]
    fn test_capsule_unpack_transparently_handles_v2_and_v3() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_path = tmp.path().join("payload.tar");
        create_small_tar(&tar_path);

        let v2_root = tempfile::tempdir().unwrap();
        write_v2_payload_from_tar(&tar_path, v2_root.path());
        let out_v2 = tempfile::tempdir().unwrap();
        unpack_payload_from_capsule_root_with_cas(v2_root.path(), out_v2.path(), None).unwrap();

        let v3_root = tempfile::tempdir().unwrap();
        let cas = CasStore::new(v3_root.path().join("cas")).unwrap();
        let (manifest, _) = build_v3_from_tar(&tar_path, &cas, 1024);
        write_manifest(v3_root.path(), &manifest);
        let out_v3 = tempfile::tempdir().unwrap();
        unpack_payload_from_capsule_root_with_cas(v3_root.path(), out_v3.path(), Some(cas))
            .unwrap();

        assert_eq!(snapshot_files(out_v2.path()), snapshot_files(out_v3.path()));
    }

    #[test]
    fn test_capsule_unpack_falls_back_to_v2_when_cas_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_path = tmp.path().join("payload.tar");
        create_small_tar(&tar_path);

        let root = tempfile::tempdir().unwrap();
        write_v2_payload_from_tar(&tar_path, root.path());

        let cas = CasStore::new(root.path().join("cas")).unwrap();
        let (manifest, _) = build_v3_from_tar(&tar_path, &cas, 1024);
        write_manifest(root.path(), &manifest);

        let out = tempfile::tempdir().unwrap();
        let provider = CasProvider::Disabled(CasDisableReason::InitializationFailed(
            "permission denied".to_string(),
        ));
        let outcome =
            unpack_payload_from_capsule_root_with_provider(root.path(), out.path(), &provider)
                .unwrap();
        assert!(matches!(
            outcome,
            PayloadUnpackOutcome::RestoredFromV2DueToCasDisabled(_)
        ));
        assert_eq!(
            fs::read_to_string(out.path().join("source/a.txt")).unwrap(),
            "aaa"
        );
        assert_eq!(
            fs::read_to_string(out.path().join("source/b.txt")).unwrap(),
            "bbb"
        );
    }

    #[test]
    fn test_capsule_unpack_falls_back_to_v2_when_v3_reconstruct_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_path = tmp.path().join("payload.tar");
        create_small_tar(&tar_path);

        let root = tempfile::tempdir().unwrap();
        write_v2_payload_from_tar(&tar_path, root.path());

        let cas = CasStore::new(root.path().join("cas")).unwrap();
        let (manifest, hashes) = build_v3_from_tar(&tar_path, &cas, 1024);
        write_manifest(root.path(), &manifest);
        let missing = hashes[hashes.len() / 2].clone();
        fs::remove_file(cas.chunk_path(&missing).unwrap()).unwrap();

        let out = tempfile::tempdir().unwrap();
        let provider = CasProvider::Enabled(cas);
        let outcome =
            unpack_payload_from_capsule_root_with_provider(root.path(), out.path(), &provider)
                .unwrap();
        assert!(matches!(
            outcome,
            PayloadUnpackOutcome::RestoredFromV2DueToV3Error(_)
        ));
        assert_eq!(
            fs::read_to_string(out.path().join("source/a.txt")).unwrap(),
            "aaa"
        );
        assert_eq!(
            fs::read_to_string(out.path().join("source/b.txt")).unwrap(),
            "bbb"
        );
    }

    #[test]
    fn unpack_payload_manifest_false_when_missing_manifest_file() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        assert!(!unpack_payload_from_v3_manifest(root.path(), out.path()).unwrap());
    }
}
