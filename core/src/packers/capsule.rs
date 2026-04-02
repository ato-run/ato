use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

use sha2::{Digest, Sha256};
use tar::{Builder, EntryType, Header};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc as tokio_mpsc;
use tracing::debug;
use zstd::stream::write::Encoder as ZstdEncoder;

use crate::capsule_v3::{CasStore, FastCdcWriter, FastCdcWriterConfig, V3_PAYLOAD_MANIFEST_PATH};
use crate::common::paths::{
    path_contains_workspace_internal_subtree, path_contains_workspace_state_dir,
};
use crate::error::{CapsuleError, Result as CapsuleResult};
use crate::lockfile::{CAPSULE_LOCK_FILE_NAME, LEGACY_CAPSULE_LOCK_FILE_NAME};
use crate::packers::pack_filter::PackFilter;
use crate::packers::payload::{
    build_distribution_manifest, manifest_hash, normalize_relative_utf8_path,
    reconstruct_from_chunks,
};
use crate::packers::sbom::{generate_embedded_sbom_from_inputs_async, SbomFileInput, SBOM_PATH};
use crate::r3_config;
use crate::router::CompatProjectInput;

const README_CANDIDATES: [&str; 4] = ["README.md", "README.mdx", "README.txt", "README"];

/// Capsule PAX TAR Archive Structure:
/// ```text
/// my-app.capsule (PAX TAR)
/// ├── capsule.toml
/// ├── capsule.lock.json
/// ├── signature.json
/// └── payload.tar.zst
///     ├── source/ (code)
///     └── config.json (prepared by controller)
/// ```

#[derive(Debug, Clone)]
pub struct CapsulePackOptions {
    pub compat_input: Option<CompatProjectInput>,
    pub workspace_root: PathBuf,
    pub output: Option<PathBuf>,
    pub config_json: Arc<r3_config::ConfigJson>,
    pub config_path: PathBuf,
    pub lockfile_path: PathBuf,
}

#[derive(Debug, Clone)]
struct PayloadFileEntry {
    archive_path: String,
    disk_path: PathBuf,
    size: u64,
    mode: u32,
}

#[derive(Debug, Clone)]
struct PayloadRoot {
    disk_root: PathBuf,
    archive_prefix: String,
    filter_prefix: Option<PathBuf>,
    warning: Option<&'static str>,
}

#[derive(Debug, Clone)]
enum PayloadEntry {
    File(PayloadFileEntry),
    Symlink {
        archive_path: String,
        link_target: PathBuf,
    },
}

impl PayloadEntry {
    fn archive_path(&self) -> &str {
        match self {
            Self::File(file) => &file.archive_path,
            Self::Symlink { archive_path, .. } => archive_path,
        }
    }

    fn kind_rank(&self) -> u8 {
        match self {
            Self::Symlink { .. } => 1,
            Self::File(_) => 2,
        }
    }
}

enum TarStreamCommand {
    File {
        archive_path: String,
        size: u64,
        mode: u32,
        chunks: tokio_mpsc::Receiver<PayloadChunk>,
    },
    Symlink {
        archive_path: String,
        link_target: PathBuf,
    },
}

enum PayloadChunk {
    Data(Vec<u8>),
    Error(String),
}

struct PayloadChunkReader {
    chunks: tokio_mpsc::Receiver<PayloadChunk>,
    current: Cursor<Vec<u8>>,
    finished: bool,
}

impl PayloadChunkReader {
    fn new(chunks: tokio_mpsc::Receiver<PayloadChunk>) -> Self {
        Self {
            chunks,
            current: Cursor::new(Vec::new()),
            finished: false,
        }
    }
}

impl Read for PayloadChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let read = std::io::Read::read(&mut self.current, buf)?;
            if read > 0 {
                return Ok(read);
            }
            if self.finished {
                return Ok(0);
            }

            match self.chunks.blocking_recv() {
                Some(PayloadChunk::Data(chunk)) => {
                    self.current = Cursor::new(chunk);
                }
                Some(PayloadChunk::Error(message)) => {
                    return Err(std::io::Error::other(message));
                }
                None => {
                    self.finished = true;
                    return Ok(0);
                }
            }
        }
    }
}

const ZSTD_COMPRESSION_LEVEL: i32 = 19;
const PAYLOAD_CHUNK_BYTES: usize = 64 * 1024;
const PAYLOAD_CHANNEL_DEPTH: usize = 8;
const DEFAULT_REPRO_MTIME: u64 = 0;

pub async fn pack(
    plan: &crate::router::ManifestData,
    opts: CapsulePackOptions,
    reporter: Arc<dyn crate::reporter::CapsuleReporter + 'static>,
) -> CapsuleResult<PathBuf> {
    debug!("Creating capsule archive (.capsule format)");

    let compat_input = opts.compat_input.as_ref().ok_or_else(|| {
        CapsuleError::Pack("capsule pack requires compat manifest input".to_string())
    })?;
    let pack_filter = PackFilter::from_manifest(compat_input.manifest())?;

    // config/lockfile are prepared by the caller once and injected into this packer.
    debug!(
        "Phase 1: using prepared config.json (version={})",
        opts.config_json.version
    );
    let config_path = opts.config_path.clone();
    let lockfile_path = opts.lockfile_path.clone();
    if !config_path.exists() {
        return Err(CapsuleError::Pack(format!(
            "config.json is missing: {}",
            config_path.display()
        )));
    }
    if !lockfile_path.exists() {
        return Err(CapsuleError::Pack(format!(
            "{} is missing: {}",
            CAPSULE_LOCK_FILE_NAME,
            lockfile_path.display()
        )));
    }

    // Step 2: Resolve source payload input
    let payload_roots = select_payload_roots(plan, &opts.workspace_root)?;
    for root in &payload_roots {
        if let Some(message) = root.warning {
            reporter
                .warn(message.to_string())
                .await
                .map_err(|err| CapsuleError::Pack(format!("Failed to emit pack warning: {err}")))?;
        }
    }

    // Step 3: Create payload.tar.zst (single-pass stream)
    debug!("Phase 2: creating payload.tar.zst (streaming)");

    let mut payload_entries = Vec::new();
    for root in &payload_roots {
        payload_entries.extend(
            collect_payload_entries(
                root.disk_root.as_path(),
                &root.archive_prefix,
                &pack_filter,
                root.filter_prefix.as_deref(),
            )
            .await?,
        );
    }
    payload_entries.push(PayloadEntry::File(
        payload_file_entry(&config_path, "config.json".to_string()).await?,
    ));
    payload_entries.sort_by(|a, b| {
        a.archive_path()
            .cmp(b.archive_path())
            .then_with(|| a.kind_rank().cmp(&b.kind_rank()))
    });

    let temp_dir = tempfile::tempdir()?;
    let payload_zst_path = temp_dir.path().join("payload.tar.zst");

    let (tar_tx, tar_rx) = mpsc::channel::<TarStreamCommand>();
    let writer_payload_path = payload_zst_path.clone();
    let writer_handle =
        std::thread::spawn(move || write_payload_tar_zstd_stream(&writer_payload_path, tar_rx));

    let stream_result = stream_payload_entries_to_writer(&payload_entries, &tar_tx).await;
    drop(tar_tx);
    let writer_result = writer_handle
        .join()
        .map_err(|_| CapsuleError::Pack("Payload writer thread panicked".to_string()))?;

    let sbom_inputs = match (stream_result, writer_result) {
        (Ok(files), Ok(())) => files,
        (Err(producer_err), Ok(())) => return Err(producer_err),
        (Ok(_), Err(writer_err)) => return Err(writer_err),
        (Err(producer_err), Err(writer_err)) => {
            return Err(CapsuleError::Pack(format!(
                "Payload stream failed: {producer_err}; writer failed: {writer_err}"
            )))
        }
    };

    let compressed_size = fs::metadata(&payload_zst_path)?.len() as usize;
    debug!("Compressed payload size: {}", format_bytes(compressed_size));
    let payload_tar_bytes = read_payload_tar_bytes_from_zst(&payload_zst_path)?;
    let (distribution_manifest, manifest_toml_bytes) =
        build_distribution_manifest(compat_input.manifest(), &payload_tar_bytes)?;
    let rebuilt_payload = reconstruct_from_chunks(
        &payload_tar_bytes,
        &distribution_manifest
            .distribution
            .as_ref()
            .expect("distribution metadata")
            .chunk_list,
    )?;
    if rebuilt_payload != payload_tar_bytes {
        return Err(CapsuleError::Pack(
            "failed to reconstruct payload.tar from chunk_list".to_string(),
        ));
    }
    debug!(
        "Generated manifest hash={}",
        manifest_hash(&distribution_manifest)?
    );
    let payload_v3_manifest_bytes = maybe_build_payload_v3_manifest(&payload_tar_bytes)?;

    // Step 4: Create final .capsule archive
    debug!("Phase 3: creating final .capsule archive");

    let output_path = opts.output.clone().unwrap_or_else(|| {
        let name_str = compat_input.package_name().replace('\"', "-");
        opts.workspace_root.join(format!("{}.capsule", name_str))
    });

    let mut capsule_file = fs::File::create(&output_path)?;
    let mut outer_ar = Builder::new(&mut capsule_file);

    // Write actual manifest content
    let manifest_temp_path = temp_dir.path().join("capsule.toml");
    fs::write(&manifest_temp_path, &manifest_toml_bytes)?;
    append_regular_file_normalized(
        &mut outer_ar,
        &manifest_temp_path,
        "capsule.toml",
        reproducible_mtime_epoch(),
    )?;
    let packaged_lockfile_bytes =
        crate::lockfile::render_lockfile_for_manifest(&lockfile_path, &distribution_manifest)?;
    let packaged_lockfile_path = temp_dir.path().join(CAPSULE_LOCK_FILE_NAME);
    fs::write(&packaged_lockfile_path, packaged_lockfile_bytes)?;
    append_regular_file_normalized(
        &mut outer_ar,
        &packaged_lockfile_path,
        CAPSULE_LOCK_FILE_NAME,
        reproducible_mtime_epoch(),
    )?;

    let sbom = generate_embedded_sbom_from_inputs_async(
        compat_input.package_name().to_string(),
        sbom_inputs,
    )
    .await?;
    let sbom_temp_path = temp_dir.path().join(SBOM_PATH);
    fs::write(&sbom_temp_path, &sbom.document)?;
    append_regular_file_normalized(
        &mut outer_ar,
        &sbom_temp_path,
        SBOM_PATH,
        reproducible_mtime_epoch(),
    )?;

    // Add signature.json metadata
    let sig_temp_path = temp_dir.path().join("signature.json");
    let signature = serde_json::json!({
        "signed": false,
        "note": "To be signed",
        "sbom": {
            "path": SBOM_PATH,
            "sha256": sbom.sha256,
            "format": "spdx-json",
        }
    });
    let signature_bytes = serde_jcs::to_vec(&signature).map_err(|e| {
        CapsuleError::Pack(format!("Failed to serialize signature metadata (JCS): {e}"))
    })?;
    fs::write(&sig_temp_path, signature_bytes)?;
    append_regular_file_normalized(
        &mut outer_ar,
        &sig_temp_path,
        "signature.json",
        reproducible_mtime_epoch(),
    )?;

    // Add payload.tar.zst
    append_regular_file_normalized(
        &mut outer_ar,
        &payload_zst_path,
        "payload.tar.zst",
        reproducible_mtime_epoch(),
    )?;

    if let Some(payload_v3_manifest_bytes) = payload_v3_manifest_bytes {
        let payload_v3_manifest_path = temp_dir.path().join(V3_PAYLOAD_MANIFEST_PATH);
        fs::write(&payload_v3_manifest_path, payload_v3_manifest_bytes)?;
        append_regular_file_normalized(
            &mut outer_ar,
            &payload_v3_manifest_path,
            V3_PAYLOAD_MANIFEST_PATH,
            reproducible_mtime_epoch(),
        )?;
    }

    if let Some((readme_path, archive_name)) = find_nearest_readme_candidate(&opts.workspace_root) {
        append_regular_file_normalized(
            &mut outer_ar,
            &readme_path,
            &archive_name,
            reproducible_mtime_epoch(),
        )?;
    }

    outer_ar.finish()?;
    drop(outer_ar);

    let final_size = fs::metadata(&output_path)?.len();
    debug!(
        "Capsule created: {} ({})",
        output_path.display(),
        format_bytes(final_size as usize)
    );

    Ok(output_path)
}

async fn collect_payload_entries(
    src_root: &Path,
    prefix: &str,
    filter: &PackFilter,
    filter_prefix: Option<&Path>,
) -> CapsuleResult<Vec<PayloadEntry>> {
    let mut entries = Vec::new();
    let mut stack = vec![src_root.to_path_buf()];

    while let Some(dir_path) = stack.pop() {
        let mut read_dir = tokio::fs::read_dir(&dir_path).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;

            let rel = match path.strip_prefix(src_root) {
                Ok(rel) if !rel.as_os_str().is_empty() => Some(rel.to_path_buf()),
                _ => None,
            };

            if file_type.is_dir() {
                if rel
                    .as_deref()
                    .is_some_and(path_contains_workspace_state_dir)
                {
                    continue;
                }
                stack.push(path);
                continue;
            }

            let Some(rel) = rel.as_deref() else {
                continue;
            };
            let filter_rel = filter_prefix
                .map(|value| value.join(rel))
                .unwrap_or_else(|| rel.to_path_buf());
            if !filter.should_include_file(&filter_rel) || should_skip_reserved_file(&filter_rel) {
                continue;
            }

            let rel_str = normalize_relative_utf8_path(rel)?;
            let archive_path = format!("{}/{}", prefix, rel_str);
            if file_type.is_symlink() {
                let link_target = tokio::fs::read_link(&path).await?;
                entries.push(PayloadEntry::Symlink {
                    archive_path,
                    link_target,
                });
                continue;
            }
            if file_type.is_file() {
                let metadata = entry.metadata().await?;
                entries.push(PayloadEntry::File(PayloadFileEntry {
                    archive_path,
                    disk_path: path,
                    size: metadata.len(),
                    mode: metadata_mode(&metadata),
                }));
            }
        }
    }

    Ok(entries)
}

async fn payload_file_entry(path: &Path, archive_path: String) -> CapsuleResult<PayloadFileEntry> {
    let metadata = tokio::fs::metadata(path).await?;
    Ok(PayloadFileEntry {
        archive_path,
        disk_path: path.to_path_buf(),
        size: metadata.len(),
        mode: metadata_mode(&metadata),
    })
}

async fn stream_payload_entries_to_writer(
    entries: &[PayloadEntry],
    tar_tx: &mpsc::Sender<TarStreamCommand>,
) -> CapsuleResult<Vec<SbomFileInput>> {
    let mut sbom_inputs = Vec::new();
    for entry in entries {
        match entry {
            PayloadEntry::Symlink {
                archive_path,
                link_target,
            } => {
                tar_tx
                    .send(TarStreamCommand::Symlink {
                        archive_path: archive_path.clone(),
                        link_target: link_target.clone(),
                    })
                    .map_err(|e| {
                        CapsuleError::Pack(format!(
                            "Failed to enqueue symlink tar entry {archive_path}: {e}"
                        ))
                    })?;
            }
            PayloadEntry::File(file_entry) => {
                let sha256 = stream_file_to_tar_writer(tar_tx, file_entry).await?;
                sbom_inputs.push(SbomFileInput {
                    archive_path: file_entry.archive_path.clone(),
                    sha256,
                    disk_path: Some(file_entry.disk_path.clone()),
                });
            }
        }
    }
    Ok(sbom_inputs)
}

async fn stream_file_to_tar_writer(
    tar_tx: &mpsc::Sender<TarStreamCommand>,
    file_entry: &PayloadFileEntry,
) -> CapsuleResult<String> {
    let (chunk_tx, chunk_rx) = tokio_mpsc::channel::<PayloadChunk>(PAYLOAD_CHANNEL_DEPTH);
    tar_tx
        .send(TarStreamCommand::File {
            archive_path: file_entry.archive_path.clone(),
            size: file_entry.size,
            mode: file_entry.mode,
            chunks: chunk_rx,
        })
        .map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to enqueue tar file entry {}: {}",
                file_entry.archive_path, e
            ))
        })?;

    let mut file = tokio::fs::File::open(&file_entry.disk_path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; PAYLOAD_CHUNK_BYTES];
    let mut remaining = file_entry.size;

    while remaining > 0 {
        let to_read = std::cmp::min(buffer.len() as u64, remaining) as usize;
        let bytes_read = match file.read(&mut buffer[..to_read]).await {
            Ok(n) => n,
            Err(err) => {
                let _ = chunk_tx
                    .send(PayloadChunk::Error(format!(
                        "Failed to read {}: {}",
                        file_entry.disk_path.display(),
                        err
                    )))
                    .await;
                return Err(CapsuleError::Io(err));
            }
        };
        if bytes_read == 0 {
            let message = format!(
                "File changed while packaging (unexpected EOF): {}",
                file_entry.disk_path.display()
            );
            let _ = chunk_tx.send(PayloadChunk::Error(message.clone())).await;
            return Err(CapsuleError::Pack(message));
        }
        let chunk = &buffer[..bytes_read];
        hasher.update(chunk);
        if chunk_tx
            .send(PayloadChunk::Data(chunk.to_vec()))
            .await
            .is_err()
        {
            return Err(CapsuleError::Pack(
                "Payload writer thread disconnected while streaming file chunks".to_string(),
            ));
        }
        remaining -= bytes_read as u64;
    }
    if remaining != 0 {
        let message = format!(
            "File changed while packaging (size drift): {}",
            file_entry.disk_path.display()
        );
        let _ = chunk_tx.send(PayloadChunk::Error(message.clone())).await;
        return Err(CapsuleError::Pack(message));
    }
    if file_entry.size == 0 {
        // zero-length file: send no chunks, only EOF by dropping sender.
    } else {
        let mut probe = [0u8; 1];
        let probe_read = file.read(&mut probe).await?;
        if probe_read > 0 {
            let message = format!(
                "File changed while packaging (grew after metadata read): {}",
                file_entry.disk_path.display()
            );
            let _ = chunk_tx.send(PayloadChunk::Error(message.clone())).await;
            return Err(CapsuleError::Pack(message));
        }
    }
    drop(chunk_tx);

    Ok(hex::encode(hasher.finalize()))
}

fn write_payload_tar_zstd_stream(
    payload_zst_path: &Path,
    tar_rx: mpsc::Receiver<TarStreamCommand>,
) -> CapsuleResult<()> {
    let payload_file = fs::File::create(payload_zst_path)?;
    let mut encoder = ZstdEncoder::new(payload_file, ZSTD_COMPRESSION_LEVEL)?;
    let threads = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    if threads > 1 {
        encoder.multithread(threads)?;
    }
    let mut tar = Builder::new(encoder);
    let mtime = reproducible_mtime_epoch();

    while let Ok(command) = tar_rx.recv() {
        match command {
            TarStreamCommand::Symlink {
                archive_path,
                link_target,
            } => {
                let mut header = Header::new_gnu();
                header.set_entry_type(EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                header.set_mtime(mtime);
                header.set_uid(0);
                header.set_gid(0);
                tar.append_link(&mut header, &archive_path, link_target)?;
            }
            TarStreamCommand::File {
                archive_path,
                size,
                mode,
                chunks,
            } => {
                let mut header = Header::new_gnu();
                header.set_size(size);
                header.set_mode(normalize_file_mode(mode));
                header.set_mtime(mtime);
                header.set_uid(0);
                header.set_gid(0);
                header.set_cksum();
                let mut reader = PayloadChunkReader::new(chunks);
                tar.append_data(&mut header, &archive_path, &mut reader)?;
            }
        }
    }

    tar.finish()?;
    let encoder = tar.into_inner()?;
    let _ = encoder.finish()?;
    Ok(())
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn metadata_mode(_: &fs::Metadata) -> u32 {
    0o644
}

fn should_skip_reserved_file(rel: &Path) -> bool {
    let file_name = rel
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    if matches!(
        file_name.as_str(),
        "capsule.toml"
            | CAPSULE_LOCK_FILE_NAME
            | LEGACY_CAPSULE_LOCK_FILE_NAME
            | "config.json"
            | "signature.json"
            | "sbom.spdx.json"
            | "payload.v3.manifest.json"
            | "payload.tar"
            | "payload.tar.zst"
    ) {
        return true;
    }

    file_name.ends_with(".capsule") || file_name.ends_with(".sig")
}

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1}GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn append_regular_file_normalized<W: std::io::Write>(
    builder: &mut Builder<W>,
    source_path: &Path,
    archive_path: &str,
    mtime: u64,
) -> CapsuleResult<()> {
    let mut file = fs::File::open(source_path)?;
    let metadata = file.metadata()?;
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_size(metadata.len());
    header.set_mode(normalize_file_mode(metadata_mode(&metadata)));
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder.append_data(&mut header, archive_path, &mut file)?;
    Ok(())
}

fn normalize_file_mode(mode: u32) -> u32 {
    if mode & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

fn reproducible_mtime_epoch() -> u64 {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_REPRO_MTIME)
}

pub(crate) fn find_nearest_readme_candidate(start_dir: &Path) -> Option<(PathBuf, String)> {
    let mut current = Some(start_dir);
    while let Some(dir) = current {
        for candidate in README_CANDIDATES {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some((path, candidate.to_string()));
            }
        }
        current = dir.parent();
    }
    None
}

fn select_payload_source_root(manifest_dir: &Path) -> (PathBuf, Option<&'static str>) {
    let source_dir = manifest_dir.join("source");
    if !source_dir.exists() {
        return (manifest_dir.to_path_buf(), None);
    }

    // If both roots exist and only the project root has Next standalone node_modules,
    // prefer project root to avoid publishing stale source/ snapshots.
    if has_next_standalone_node_modules(manifest_dir)
        && !has_next_standalone_node_modules(source_dir.as_path())
    {
        return (
            manifest_dir.to_path_buf(),
            Some(
                "Detected source/ directory without Next standalone node_modules while project root has them; packaging project root to keep runtime-complete artifacts.",
            ),
        );
    }

    (source_dir, None)
}

fn select_payload_roots(
    plan: &crate::router::ManifestData,
    manifest_dir: &Path,
) -> CapsuleResult<Vec<PayloadRoot>> {
    if !plan.is_schema_v03() {
        let (source_root, warning) = select_payload_source_root(manifest_dir);
        ensure_payload_source_root(&source_root)?;
        let mut roots = vec![PayloadRoot {
            disk_root: source_root,
            archive_prefix: "source".to_string(),
            filter_prefix: None,
            warning,
        }];
        append_workspace_artifacts_root(&mut roots, manifest_dir);
        return Ok(roots);
    }

    let mut roots = plan
        .selected_target_package_order()
        .map_err(|err| CapsuleError::Pack(err.to_string()))?
        .into_iter()
        .map(|target_label| -> CapsuleResult<PayloadRoot> {
            let target_plan = plan.with_selected_target(target_label);
            let working_dir = target_plan.execution_working_directory();
            let relative = working_dir
                .strip_prefix(manifest_dir)
                .unwrap_or(working_dir.as_path())
                .to_path_buf();
            let (source_root, warning) = select_payload_source_root(&working_dir);
            ensure_payload_source_root(&source_root)?;
            Ok(PayloadRoot {
                disk_root: source_root,
                archive_prefix: if relative.as_os_str().is_empty() {
                    "source".to_string()
                } else {
                    format!(
                        "source/{}",
                        normalize_relative_utf8_path(&relative)
                            .map_err(|err| CapsuleError::Pack(err.to_string()))?
                    )
                },
                filter_prefix: Some(relative),
                warning,
            })
        })
        .collect::<CapsuleResult<Vec<_>>>()?;

    roots.sort_by(|a, b| a.archive_prefix.cmp(&b.archive_prefix));
    let mut deduped = Vec::new();
    for root in roots {
        let covered = deduped.iter().any(|existing: &PayloadRoot| {
            root.disk_root == existing.disk_root
                || root.disk_root.starts_with(&existing.disk_root)
                || root.archive_prefix == existing.archive_prefix
        });
        if !covered {
            deduped.push(root);
        }
    }
    if deduped.is_empty() {
        let (source_root, warning) = select_payload_source_root(manifest_dir);
        ensure_payload_source_root(&source_root)?;
        deduped.push(PayloadRoot {
            disk_root: source_root,
            archive_prefix: "source".to_string(),
            filter_prefix: None,
            warning,
        });
    }
    append_workspace_artifacts_root(&mut deduped, manifest_dir);
    Ok(deduped)
}

fn append_workspace_artifacts_root(roots: &mut Vec<PayloadRoot>, manifest_dir: &Path) {
    let artifacts_root = crate::common::paths::workspace_artifacts_dir(manifest_dir);
    if !artifacts_root.exists() {
        return;
    }

    roots.push(PayloadRoot {
        disk_root: artifacts_root,
        archive_prefix: "source/artifacts".to_string(),
        filter_prefix: None,
        warning: None,
    });
}

fn ensure_payload_source_root(path: &Path) -> CapsuleResult<()> {
    if path_contains_workspace_internal_subtree(path) {
        return Err(CapsuleError::Pack(format!(
            "refusing to package from workspace internal state root: {}",
            path.display()
        )));
    }

    Ok(())
}
fn has_next_standalone_node_modules(root: &Path) -> bool {
    if root.join(".next/standalone/node_modules").is_dir() {
        return true;
    }

    let apps_dir = root.join("apps");
    let Ok(entries) = fs::read_dir(&apps_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if entry.path().join(".next/standalone/node_modules").is_dir() {
            return true;
        }
    }
    false
}

fn read_payload_tar_bytes_from_zst(payload_zst_path: &Path) -> CapsuleResult<Vec<u8>> {
    let payload_file = fs::File::open(payload_zst_path)?;
    let mut decoder = zstd::stream::Decoder::new(payload_file)?;
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

fn maybe_build_payload_v3_manifest(payload_tar_bytes: &[u8]) -> CapsuleResult<Option<Vec<u8>>> {
    if !experimental_v3_pack_enabled()? {
        return Ok(None);
    }

    let cas = CasStore::from_env()?;
    let manifest_bytes = build_payload_v3_manifest_bytes_with_cas(
        payload_tar_bytes,
        cas,
        FastCdcWriterConfig::default(),
    )?;
    Ok(Some(manifest_bytes))
}

fn build_payload_v3_manifest_bytes_with_cas(
    payload_tar_bytes: &[u8],
    cas: CasStore,
    config: FastCdcWriterConfig,
) -> CapsuleResult<Vec<u8>> {
    let mut writer = FastCdcWriter::new(config, cas)?;
    writer.write_bytes(payload_tar_bytes)?;
    let report = writer.finalize()?;
    debug!(
        chunks = report.manifest.chunks.len(),
        inserted = report.chunks_inserted,
        reused = report.chunks_reused,
        total_raw_size = report.total_raw_size,
        "Generated payload v3 manifest and populated CAS"
    );

    serde_jcs::to_vec(&report.manifest).map_err(|err| {
        CapsuleError::Pack(format!(
            "Failed to serialize payload.v3.manifest.json (JCS): {err}"
        ))
    })
}

fn experimental_v3_pack_enabled() -> CapsuleResult<bool> {
    let raw = match std::env::var("ATO_EXPERIMENTAL_V3_PACK") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(false),
        Err(err) => {
            return Err(CapsuleError::Config(format!(
                "Failed to read ATO_EXPERIMENTAL_V3_PACK: {}",
                err
            )))
        }
    };

    parse_bool_env("ATO_EXPERIMENTAL_V3_PACK", &raw)
}

fn parse_bool_env(key: &str, raw: &str) -> CapsuleResult<bool> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        _ => Err(CapsuleError::Config(format!(
            "Invalid {} value '{}'; expected one of 1,true,yes,on,0,false,no,off",
            key, raw
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsule_v3::{verify_artifact_hash, CapsuleManifestV3};
    use crate::packers::payload::reconstruct_from_chunks;
    use crate::reporter::NoOpReporter;
    use crate::router::ExecutionProfile;
    use crate::types::CapsuleManifest;
    use std::io::Read;

    fn sha256_hex(data: &[u8]) -> String {
        let mut hasher = sha2::Sha256::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }

    #[test]
    fn select_payload_source_root_prefers_manifest_root_for_stale_source_snapshot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source");
        let root_standalone_nm = tmp
            .path()
            .join("apps/dashboard/.next/standalone/node_modules/next");
        let source_standalone = source_dir.join("apps/dashboard/.next/standalone/apps/dashboard");
        std::fs::create_dir_all(&root_standalone_nm).expect("mkdir root standalone node_modules");
        std::fs::create_dir_all(&source_standalone).expect("mkdir source standalone");

        let (selected, warning) = select_payload_source_root(tmp.path());
        assert_eq!(selected.as_path(), tmp.path());
        assert!(warning.is_some());
    }

    #[test]
    fn select_payload_source_root_uses_source_when_runtime_layout_is_consistent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source");
        let source_standalone_nm =
            source_dir.join("apps/dashboard/.next/standalone/node_modules/next");
        std::fs::create_dir_all(&source_standalone_nm)
            .expect("mkdir source standalone node_modules");

        let (selected, warning) = select_payload_source_root(tmp.path());
        assert_eq!(selected.as_path(), source_dir.as_path());
        assert!(warning.is_none());
    }

    #[test]
    fn find_nearest_readme_candidate_walks_up_to_parent_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_dir = tmp.path().join("apps/file2api");
        std::fs::create_dir_all(&app_dir).expect("mkdir app dir");
        std::fs::write(tmp.path().join("README.md"), "# monorepo readme")
            .expect("write root readme");

        let found = find_nearest_readme_candidate(&app_dir).expect("find readme");
        assert_eq!(found.0, tmp.path().join("README.md"));
        assert_eq!(found.1, "README.md");
    }

    #[test]
    fn select_payload_roots_for_v03_workspace_uses_selected_package_closure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::create_dir_all(tmp.path().join("apps/web")).expect("mkdir web");
        std::fs::create_dir_all(tmp.path().join("packages/ui")).expect("mkdir ui");
        std::fs::create_dir_all(tmp.path().join("apps/admin")).expect("mkdir admin");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "workspace-app"
default_target = "web"

[packages.web]
capsule_path = "./apps/web"

[packages.ui]
capsule_path = "./packages/ui"

[packages.admin]
capsule_path = "./apps/admin"
"#,
        )
        .expect("write manifest");
        std::fs::write(
            tmp.path().join("apps/web/capsule.toml"),
            "schema_version = \"0.3\"\nname = \"web\"\ntype = \"app\"\nruntime = \"source/node\"\nrun = \"node server.js\"\n[dependencies]\nui = \"workspace:ui\"\n",
        )
        .expect("write web manifest");
        std::fs::write(
            tmp.path().join("packages/ui/capsule.toml"),
            "schema_version = \"0.3\"\nname = \"ui\"\ntype = \"library\"\nruntime = \"source/node\"\nbuild = \"node build.js\"\n",
        )
        .expect("write ui manifest");
        std::fs::write(
            tmp.path().join("apps/admin/capsule.toml"),
            "schema_version = \"0.3\"\nname = \"admin\"\ntype = \"app\"\nruntime = \"source/node\"\nrun = \"node admin.js\"\n",
        )
        .expect("write admin manifest");

        let decision =
            crate::router::route_manifest(&manifest_path, ExecutionProfile::Release, None)
                .expect("route manifest");
        let roots = select_payload_roots(&decision.plan, tmp.path()).expect("select payload roots");
        let prefixes = roots
            .iter()
            .map(|root| root.archive_prefix.clone())
            .collect::<Vec<_>>();

        assert_eq!(
            prefixes,
            vec![
                "source/apps/web".to_string(),
                "source/packages/ui".to_string()
            ]
        );
    }

    #[test]
    fn select_payload_roots_rejects_internal_workspace_state_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let internal_root = tmp.path().join(".ato").join("tmp").join("generated-app");
        std::fs::create_dir_all(internal_root.join("source")).expect("mkdir source");
        let manifest_path = internal_root.join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.2"
name = "generated-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "source/main.sh"
"#,
        )
        .expect("write manifest");

        let decision =
            crate::router::route_manifest(&manifest_path, ExecutionProfile::Release, None)
                .expect("route manifest");
        let err = select_payload_roots(&decision.plan, &internal_root)
            .expect_err("internal workspace state root must fail");
        assert!(err
            .to_string()
            .contains("refusing to package from workspace internal state root"));
    }

    #[test]
    fn parse_bool_env_accepts_truthy_values() {
        for value in ["1", "true", "TRUE", "yes", "on"] {
            let parsed = parse_bool_env("TEST", value).expect("parse env");
            assert!(parsed, "value should be true: {}", value);
        }
    }

    #[test]
    fn parse_bool_env_accepts_falsey_values() {
        for value in ["0", "false", "FALSE", "no", "off", ""] {
            let parsed = parse_bool_env("TEST", value).expect("parse env");
            assert!(!parsed, "value should be false: {}", value);
        }
    }

    #[test]
    fn parse_bool_env_rejects_unknown_value() {
        let err = parse_bool_env("TEST", "maybe").expect_err("must reject unknown");
        assert!(err.to_string().contains("Invalid TEST value"));
    }

    #[test]
    fn build_payload_v3_manifest_populates_cas_and_produces_valid_manifest() {
        let cas_root = tempfile::tempdir().expect("cas tempdir");
        let cas = CasStore::new(cas_root.path()).expect("cas store");

        let manifest_bytes = build_payload_v3_manifest_bytes_with_cas(
            b"payload bytes for v3",
            cas.clone(),
            FastCdcWriterConfig::default(),
        )
        .expect("build payload v3 manifest");

        let manifest: CapsuleManifestV3 =
            serde_json::from_slice(&manifest_bytes).expect("parse v3 manifest");
        verify_artifact_hash(&manifest).expect("verify artifact hash");
        let fsck = cas.fsck_manifest(&manifest).expect("fsck manifest");
        assert!(fsck.is_ok(), "fsck report should be clean: {fsck:?}");
        assert!(
            !manifest.chunks.is_empty(),
            "manifest should contain chunks"
        );
    }

    #[tokio::test]
    async fn pack_source_is_reproducible_for_identical_inputs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.2"
name = "repro-source-pack"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "source/main.sh"
"#,
        )
        .expect("write manifest");

        std::fs::create_dir_all(tmp.path().join("source")).expect("mkdir source");
        std::fs::write(tmp.path().join("source/main.sh"), "echo repro").expect("write source");

        let lock_path = tmp.path().join("capsule.lock");
        std::fs::write(
            &lock_path,
            r#"version = "1"

[meta]
created_at = "2026-01-01T00:00:00Z"
manifest_hash = "sha256:dummy"
"#,
        )
        .expect("write lockfile");

        let config = Arc::new(
            crate::r3_config::generate_config(&manifest_path, Some("strict".to_string()), false)
                .expect("generate config"),
        );
        let config_path =
            crate::r3_config::write_config(&manifest_path, config.as_ref()).expect("write config");

        let decision =
            crate::router::route_manifest(&manifest_path, ExecutionProfile::Release, None)
                .expect("route");
        let out1 = tmp.path().join("repro-1.capsule");
        let out2 = tmp.path().join("repro-2.capsule");

        pack(
            &decision.plan,
            CapsulePackOptions {
                compat_input: decision.plan.compat_project_input().expect("compat input"),
                workspace_root: tmp.path().to_path_buf(),
                output: Some(out1.clone()),
                config_json: config.clone(),
                config_path: config_path.clone(),
                lockfile_path: lock_path.clone(),
            },
            Arc::new(NoOpReporter),
        )
        .await
        .expect("first pack");

        pack(
            &decision.plan,
            CapsulePackOptions {
                compat_input: decision.plan.compat_project_input().expect("compat input"),
                workspace_root: tmp.path().to_path_buf(),
                output: Some(out2.clone()),
                config_json: config,
                config_path,
                lockfile_path: lock_path,
            },
            Arc::new(NoOpReporter),
        )
        .await
        .expect("second pack");

        let first = std::fs::read(out1).expect("read first artifact");
        let second = std::fs::read(out2).expect("read second artifact");
        assert_eq!(sha256_hex(&first), sha256_hex(&second));
    }

    #[tokio::test]
    async fn pack_source_embeds_manifest_and_reconstructs_payload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.2"
name = "manifest-pack"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "source/main.sh"
"#,
        )
        .expect("write manifest");

        std::fs::create_dir_all(tmp.path().join("source")).expect("mkdir source");
        std::fs::write(tmp.path().join("source/main.sh"), "echo manifest").expect("write source");
        let lock_path = tmp.path().join("capsule.lock");
        std::fs::write(
            &lock_path,
            r#"version = "1"

[meta]
created_at = "2026-01-01T00:00:00Z"
manifest_hash = "sha256:dummy"
"#,
        )
        .expect("write lockfile");

        let config = Arc::new(
            crate::r3_config::generate_config(&manifest_path, Some("strict".to_string()), false)
                .expect("generate config"),
        );
        let config_path =
            crate::r3_config::write_config(&manifest_path, config.as_ref()).expect("write config");
        let decision =
            crate::router::route_manifest(&manifest_path, ExecutionProfile::Release, None)
                .expect("route");
        let out = tmp.path().join("manifest-pack.capsule");
        pack(
            &decision.plan,
            CapsulePackOptions {
                compat_input: decision.plan.compat_project_input().expect("compat input"),
                workspace_root: tmp.path().to_path_buf(),
                output: Some(out.clone()),
                config_json: config,
                config_path,
                lockfile_path: lock_path,
            },
            Arc::new(NoOpReporter),
        )
        .await
        .expect("pack");

        let mut outer = tar::Archive::new(std::fs::File::open(&out).expect("open"));
        let mut has_sbom = false;
        let mut payload_zst = Vec::new();
        let mut manifest_toml_bytes = None::<Vec<u8>>;
        let mut signature_bytes = None::<Vec<u8>>;
        for entry in outer.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let path = entry.path().expect("path").to_string_lossy().to_string();
            if path == "payload.tar.zst" {
                entry.read_to_end(&mut payload_zst).expect("read payload");
            } else if path == "capsule.toml" {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).expect("read manifest");
                manifest_toml_bytes = Some(bytes);
            } else if path == "signature.json" {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).expect("read signature");
                signature_bytes = Some(bytes);
            } else if path == SBOM_PATH {
                has_sbom = true;
            }
        }
        assert!(has_sbom);
        let manifest_toml_bytes = manifest_toml_bytes.expect("capsule.toml");
        let signature_bytes = signature_bytes.expect("signature.json");
        let manifest: CapsuleManifest =
            toml::from_str(std::str::from_utf8(&manifest_toml_bytes).expect("manifest utf8"))
                .expect("parse manifest");
        let signature: serde_json::Value =
            serde_json::from_slice(&signature_bytes).expect("parse signature");
        assert_eq!(
            signature
                .get("sbom")
                .and_then(|sbom| sbom.get("path"))
                .and_then(|path| path.as_str()),
            Some(SBOM_PATH)
        );

        let embedded_sbom = crate::packers::sbom::extract_and_verify_embedded_sbom(&out)
            .expect("failed to extract and verify embedded SBOM from packed capsule");
        let embedded_sbom_sha = sha256_hex(embedded_sbom.as_bytes());
        assert_eq!(
            signature
                .get("sbom")
                .and_then(|sbom| sbom.get("sha256"))
                .and_then(|sha| sha.as_str()),
            Some(embedded_sbom_sha.as_str())
        );
        let parsed_sbom: serde_json::Value =
            serde_json::from_str(&embedded_sbom).expect("parse embedded SBOM");
        assert_eq!(parsed_sbom["spdxVersion"], "SPDX-2.3");
        let files = parsed_sbom
            .get("files")
            .and_then(|files| files.as_array())
            .expect("embedded SBOM must contain a files array per SPDX-2.3 specification");
        assert!(!files.is_empty(), "SBOM files array should not be empty");

        let mut decoder =
            zstd::stream::Decoder::new(std::io::Cursor::new(payload_zst)).expect("decode payload");
        let mut payload_tar = Vec::new();
        decoder
            .read_to_end(&mut payload_tar)
            .expect("read tar bytes");
        let rebuilt = reconstruct_from_chunks(
            &payload_tar,
            &manifest
                .distribution
                .as_ref()
                .expect("distribution metadata")
                .chunk_list,
        )
        .expect("rebuild");
        assert_eq!(rebuilt, payload_tar);
    }

    #[tokio::test]
    async fn collect_payload_entries_keeps_python_lock_and_uv_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.2"
name = "python-demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
entrypoint = "main.py"
dependencies = "requirements.txt"
"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("main.py"), "print('ok')\n").unwrap();
        std::fs::write(tmp.path().join("requirements.txt"), "fastapi==0.115.0\n").unwrap();
        std::fs::write(
            tmp.path().join("uv.lock"),
            "version = 1\nrevision = 1\nrequires-python = \">=3.11\"\n",
        )
        .unwrap();
        let uv_cache = crate::common::paths::workspace_artifacts_dir(tmp.path())
            .join("macos-arm64")
            .join("uv-cache");
        std::fs::create_dir_all(&uv_cache).unwrap();
        std::fs::write(uv_cache.join("marker.txt"), "cached\n").unwrap();

        let parsed: toml::Value =
            toml::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        let plan = crate::router::execution_descriptor_from_manifest_parts(
            parsed,
            manifest_path.clone(),
            tmp.path().to_path_buf(),
            crate::router::ExecutionProfile::Release,
            Some("app"),
            std::collections::HashMap::new(),
        )
        .expect("execution descriptor");
        let loaded = crate::manifest::load_manifest(&manifest_path).unwrap();
        let pack_filter = PackFilter::from_manifest(&loaded.model).unwrap();

        let payload_roots = select_payload_roots(&plan, tmp.path()).unwrap();
        let mut names = Vec::new();
        for root in payload_roots {
            let entries = collect_payload_entries(
                root.disk_root.as_path(),
                &root.archive_prefix,
                &pack_filter,
                root.filter_prefix.as_deref(),
            )
            .await
            .unwrap();
            names.extend(
                entries
                    .into_iter()
                    .map(|entry| entry.archive_path().to_string()),
            );
        }
        names.sort();

        assert!(
            names.contains(&"source/main.py".to_string()),
            "names={names:?}"
        );
        assert!(
            names.contains(&"source/requirements.txt".to_string()),
            "names={names:?}"
        );
        assert!(
            names.contains(&"source/uv.lock".to_string()),
            "names={names:?}"
        );
        assert!(
            names.contains(&"source/artifacts/macos-arm64/uv-cache/marker.txt".to_string()),
            "names={names:?}"
        );
    }
}
