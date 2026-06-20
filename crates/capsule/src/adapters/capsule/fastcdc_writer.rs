use std::io::Write;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::capsule::hash::set_artifact_hash;
use crate::capsule::manifest::blake3_digest;
use crate::capsule::{CasStore, CdcParams, ChunkMeta, PayloadManifest};
use crate::error::{CapsuleError, Result};

const COMPACTION_THRESHOLD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FastCdcWriterConfig {
    pub cdc_min: u32,
    pub cdc_avg: u32,
    pub cdc_max: u32,
    pub cdc_seed: u64,
    pub worker_count: usize,
    pub queue_depth: usize,
    pub zstd_level: i32,
}

impl Default for FastCdcWriterConfig {
    fn default() -> Self {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self {
            cdc_min: 2 * 1024 * 1024,
            cdc_avg: 4 * 1024 * 1024,
            cdc_max: 8 * 1024 * 1024,
            cdc_seed: 0,
            worker_count: std::cmp::max(1, workers / 2),
            queue_depth: 16,
            zstd_level: 19,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FastCdcWriteReport {
    pub manifest: PayloadManifest,
    pub chunks_inserted: usize,
    pub chunks_reused: usize,
    pub total_raw_size: u64,
}

#[derive(Debug)]
struct ChunkTask {
    seq_no: usize,
    raw: Vec<u8>,
}

#[derive(Debug)]
enum WorkerResult {
    Ok {
        seq_no: usize,
        meta: ChunkMeta,
        inserted: bool,
    },
    Err {
        seq_no: usize,
        message: String,
    },
}

pub struct FastCdcWriter {
    config: FastCdcWriterConfig,
    buffer: Vec<u8>,
    start_idx: usize,
    scan_idx: usize,
    chunk_start_idx: usize,
    fingerprint: u64,
    boundary_mask: u64,
    task_tx: Option<SyncSender<ChunkTask>>,
    result_rx: Receiver<WorkerResult>,
    workers: Vec<std::thread::JoinHandle<()>>,
    pending_results: usize,
    next_seq_no: usize,
    chunk_slots: Vec<Option<ChunkMeta>>,
    chunks_inserted: usize,
    chunks_reused: usize,
    total_raw_size: u64,
    sticky_error: Option<String>,
}

impl FastCdcWriter {
    pub fn new(config: FastCdcWriterConfig, cas: CasStore) -> Result<Self> {
        validate_config(&config)?;

        let (task_tx, task_rx) = mpsc::sync_channel::<ChunkTask>(config.queue_depth);
        let (result_tx, result_rx) = mpsc::channel::<WorkerResult>();
        let shared_rx = Arc::new(Mutex::new(task_rx));

        let mut workers = Vec::with_capacity(config.worker_count);
        for _ in 0..config.worker_count {
            let worker_rx = Arc::clone(&shared_rx);
            let worker_tx = result_tx.clone();
            let worker_cas = cas.clone();
            let zstd_level = config.zstd_level;
            workers.push(thread::spawn(move || {
                worker_loop(worker_rx, worker_tx, worker_cas, zstd_level);
            }));
        }
        drop(result_tx);

        Ok(Self {
            boundary_mask: compute_boundary_mask(config.cdc_avg),
            config,
            buffer: Vec::new(),
            start_idx: 0,
            scan_idx: 0,
            chunk_start_idx: 0,
            fingerprint: 0,
            task_tx: Some(task_tx),
            result_rx,
            workers,
            pending_results: 0,
            next_seq_no: 0,
            chunk_slots: Vec::new(),
            chunks_inserted: 0,
            chunks_reused: 0,
            total_raw_size: 0,
            sticky_error: None,
        })
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.ensure_healthy()?;
        if bytes.is_empty() {
            return Ok(());
        }

        self.buffer.extend_from_slice(bytes);
        self.scan_and_emit_chunks()?;
        self.drain_results_nonblocking()?;
        self.ensure_healthy()?;
        Ok(())
    }

    pub fn finalize(mut self) -> Result<FastCdcWriteReport> {
        self.ensure_healthy()?;
        self.emit_tail_if_present()?;
        self.close_input_channel();
        self.collect_all_results_blocking()?;
        self.join_workers()?;
        self.ensure_healthy()?;

        let mut chunks = Vec::with_capacity(self.chunk_slots.len());
        for (idx, slot) in self.chunk_slots.into_iter().enumerate() {
            let chunk = slot.ok_or_else(|| {
                CapsuleError::Pack(format!("missing chunk metadata for sequence {}", idx))
            })?;
            chunks.push(chunk);
        }

        let mut manifest = PayloadManifest::new(chunks);
        manifest.cdc_params = CdcParams {
            algorithm: "fastcdc".to_string(),
            min_size: self.config.cdc_min,
            avg_size: self.config.cdc_avg,
            max_size: self.config.cdc_max,
            seed: self.config.cdc_seed,
        };
        manifest.total_raw_size = self.total_raw_size;
        set_artifact_hash(&mut manifest)?;

        Ok(FastCdcWriteReport {
            manifest,
            chunks_inserted: self.chunks_inserted,
            chunks_reused: self.chunks_reused,
            total_raw_size: self.total_raw_size,
        })
    }

    fn scan_and_emit_chunks(&mut self) -> Result<()> {
        while self.scan_idx < self.buffer.len() {
            let byte = self.buffer[self.scan_idx];
            self.fingerprint = next_fingerprint(self.fingerprint, byte, self.config.cdc_seed);
            self.scan_idx += 1;

            let chunk_len = self.scan_idx - self.chunk_start_idx;
            let should_cut = if chunk_len as u32 >= self.config.cdc_max {
                true
            } else {
                chunk_len as u32 >= self.config.cdc_min
                    && ((self.fingerprint ^ self.config.cdc_seed) & self.boundary_mask) == 0
            };

            if should_cut {
                let start = self.chunk_start_idx;
                let end = self.scan_idx;
                self.emit_chunk_range(start, end)?;
                self.chunk_start_idx = end;
                self.start_idx = end;
                self.fingerprint = 0;
                self.compact_if_needed();
            }
        }
        Ok(())
    }

    fn emit_tail_if_present(&mut self) -> Result<()> {
        if self.chunk_start_idx < self.buffer.len() {
            let start = self.chunk_start_idx;
            let end = self.buffer.len();
            self.emit_chunk_range(start, end)?;
            self.chunk_start_idx = end;
            self.start_idx = end;
            self.scan_idx = end;
        }
        Ok(())
    }

    fn emit_chunk_range(&mut self, start: usize, end: usize) -> Result<()> {
        if end <= start {
            return Ok(());
        }

        let raw = self.buffer[start..end].to_vec();
        self.total_raw_size += raw.len() as u64;

        let seq_no = self.next_seq_no;
        self.next_seq_no += 1;
        self.chunk_slots.push(None);

        let tx = self.task_tx.as_ref().ok_or_else(|| {
            CapsuleError::Pack("FastCDC input channel is already closed".to_string())
        })?;
        tx.send(ChunkTask { seq_no, raw }).map_err(|e| {
            self.sticky_error = Some(format!("failed to enqueue chunk task: {}", e));
            CapsuleError::Pack("failed to enqueue chunk task".to_string())
        })?;
        self.pending_results += 1;
        Ok(())
    }

    fn compact_if_needed(&mut self) {
        if self.start_idx < COMPACTION_THRESHOLD_BYTES {
            return;
        }
        if self.start_idx == 0 || self.start_idx > self.buffer.len() {
            return;
        }

        let tail_len = self.buffer.len().saturating_sub(self.start_idx);
        if tail_len > 0 {
            self.buffer.copy_within(self.start_idx.., 0);
        }
        self.buffer.truncate(tail_len);
        self.scan_idx = self.scan_idx.saturating_sub(self.start_idx);
        self.chunk_start_idx = self.chunk_start_idx.saturating_sub(self.start_idx);
        self.start_idx = 0;
    }

    fn close_input_channel(&mut self) {
        self.task_tx.take();
    }

    fn ensure_healthy(&self) -> Result<()> {
        if let Some(message) = &self.sticky_error {
            return Err(CapsuleError::Pack(message.clone()));
        }
        Ok(())
    }

    fn drain_results_nonblocking(&mut self) -> Result<()> {
        loop {
            match self.result_rx.try_recv() {
                Ok(result) => {
                    self.pending_results = self.pending_results.saturating_sub(1);
                    self.apply_result(result);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.pending_results > 0 && self.sticky_error.is_none() {
                        self.sticky_error =
                            Some("FastCDC worker channel disconnected unexpectedly".to_string());
                    }
                    break;
                }
            }
        }
        self.ensure_healthy()
    }

    fn collect_all_results_blocking(&mut self) -> Result<()> {
        while self.pending_results > 0 {
            let result = self.result_rx.recv().map_err(|e| {
                CapsuleError::Pack(format!("failed to receive FastCDC worker result: {}", e))
            })?;
            self.pending_results -= 1;
            self.apply_result(result);
        }
        self.ensure_healthy()
    }

    fn apply_result(&mut self, result: WorkerResult) {
        match result {
            WorkerResult::Ok {
                seq_no,
                meta,
                inserted,
            } => {
                if let Some(slot) = self.chunk_slots.get_mut(seq_no) {
                    if slot.is_some() {
                        if self.sticky_error.is_none() {
                            self.sticky_error = Some(format!(
                                "duplicate FastCDC worker result for sequence {}",
                                seq_no
                            ));
                        }
                        return;
                    }
                    *slot = Some(meta);
                    if inserted {
                        self.chunks_inserted += 1;
                    } else {
                        self.chunks_reused += 1;
                    }
                } else if self.sticky_error.is_none() {
                    self.sticky_error = Some(format!(
                        "out-of-range FastCDC worker result sequence {}",
                        seq_no
                    ));
                }
            }
            WorkerResult::Err { seq_no, message } => {
                if self.sticky_error.is_none() {
                    self.sticky_error = Some(format!(
                        "FastCDC worker failed at sequence {}: {}",
                        seq_no, message
                    ));
                }
            }
        }
    }

    fn join_workers(&mut self) -> Result<()> {
        for handle in self.workers.drain(..) {
            handle
                .join()
                .map_err(|_| CapsuleError::Pack("FastCDC worker thread panicked".to_string()))?;
        }
        Ok(())
    }
}

fn validate_config(config: &FastCdcWriterConfig) -> Result<()> {
    if config.cdc_min == 0 || config.cdc_avg == 0 || config.cdc_max == 0 {
        return Err(CapsuleError::Config(
            "FastCDC min/avg/max must be non-zero".to_string(),
        ));
    }
    if !(config.cdc_min <= config.cdc_avg && config.cdc_avg <= config.cdc_max) {
        return Err(CapsuleError::Config(format!(
            "invalid FastCDC config: min={} avg={} max={}",
            config.cdc_min, config.cdc_avg, config.cdc_max
        )));
    }
    if config.worker_count == 0 {
        return Err(CapsuleError::Config(
            "FastCDC worker_count must be greater than zero".to_string(),
        ));
    }
    if config.queue_depth == 0 {
        return Err(CapsuleError::Config(
            "FastCDC queue_depth must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

fn compute_boundary_mask(avg: u32) -> u64 {
    let target = avg.next_power_of_two().max(2);
    (target as u64) - 1
}

fn next_fingerprint(current: u64, byte: u8, seed: u64) -> u64 {
    let mixed = ((byte as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .rotate_left((byte as u32 % 63) + 1)
        ^ 0xA076_1D64_78BD_642F
        ^ seed;
    current.rotate_left(1).wrapping_add(mixed)
}

fn worker_loop(
    task_rx: Arc<Mutex<Receiver<ChunkTask>>>,
    result_tx: mpsc::Sender<WorkerResult>,
    cas: CasStore,
    zstd_level: i32,
) {
    loop {
        let task = match task_rx.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => {
                let _ = result_tx.send(WorkerResult::Err {
                    seq_no: 0,
                    message: "FastCDC worker receiver lock poisoned".to_string(),
                });
                return;
            }
        };
        let task = match task {
            Ok(task) => task,
            Err(_) => return,
        };

        let send_result = match process_chunk_task(task, &cas, zstd_level) {
            Ok(result) => result_tx.send(result),
            Err((seq_no, message)) => result_tx.send(WorkerResult::Err { seq_no, message }),
        };
        if send_result.is_err() {
            return;
        }
    }
}

fn process_chunk_task(
    task: ChunkTask,
    cas: &CasStore,
    zstd_level: i32,
) -> std::result::Result<WorkerResult, (usize, String)> {
    let raw_hash = blake3_digest(&task.raw);
    let raw_size = u32::try_from(task.raw.len()).map_err(|_| {
        (
            task.seq_no,
            format!("raw chunk size exceeds u32::MAX: {} bytes", task.raw.len()),
        )
    })?;

    let mut encoder = zstd::Encoder::new(Vec::new(), zstd_level).map_err(|e| {
        (
            task.seq_no,
            format!("failed to create zstd encoder for chunk: {}", e),
        )
    })?;
    encoder
        .write_all(&task.raw)
        .map_err(|e| (task.seq_no, format!("failed to compress chunk: {}", e)))?;
    let compressed = encoder
        .finish()
        .map_err(|e| (task.seq_no, format!("failed to finish zstd chunk: {}", e)))?;

    let put = cas
        .put_chunk_zstd(&raw_hash, &compressed)
        .map_err(|e| (task.seq_no, format!("failed to write chunk to CAS: {}", e)))?;
    let zstd_size_hint = u32::try_from(put.zstd_size).ok();

    Ok(WorkerResult::Ok {
        seq_no: task.seq_no,
        meta: ChunkMeta {
            raw_hash,
            raw_size,
            zstd_size_hint,
        },
        inserted: put.inserted,
    })
}

#[cfg(test)]
mod tests {
    use tempfile;

    use super::{CasStore, FastCdcWriter, FastCdcWriterConfig};

    fn deterministic_bytes(len: usize, mut seed: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            // xorshift64*
            seed ^= seed >> 12;
            seed ^= seed << 25;
            seed ^= seed >> 27;
            let v = seed.wrapping_mul(0x2545_F491_4F6C_DD1D);
            out.push((v & 0xFF) as u8);
        }
        out
    }

    fn make_writer(tmp: &tempfile::TempDir) -> FastCdcWriter {
        let cas = CasStore::new(tmp.path()).expect("cas");
        let cfg = FastCdcWriterConfig {
            cdc_min: 1024,
            cdc_avg: 2048,
            cdc_max: 4096,
            cdc_seed: 0,
            worker_count: 2,
            queue_depth: 8,
            zstd_level: 3,
        };
        FastCdcWriter::new(cfg, cas).expect("writer")
    }

    fn feed_pattern(writer: &mut FastCdcWriter, payload: &[u8], chunk_sizes: &[usize]) {
        let mut offset = 0usize;
        let mut i = 0usize;
        while offset < payload.len() {
            let next = chunk_sizes[i % chunk_sizes.len()];
            let end = std::cmp::min(payload.len(), offset + next.max(1));
            writer
                .write_bytes(&payload[offset..end])
                .expect("write_bytes");
            offset = end;
            i += 1;
        }
    }

    #[test]
    fn test_fastcdc_segmentation_is_input_chunking_invariant() {
        let payload = deterministic_bytes(10 * 1024 * 1024, 0xA11C_E5E5_D15C_A5E0);

        let tmp_a = tempfile::tempdir().expect("tmp");
        let mut writer_a = make_writer(&tmp_a);
        feed_pattern(&mut writer_a, &payload, &[4 * 1024]);
        let report_a = writer_a.finalize().expect("finalize a");

        let tmp_b = tempfile::tempdir().expect("tmp");
        let mut writer_b = make_writer(&tmp_b);
        feed_pattern(&mut writer_b, &payload, &[1024 * 1024]);
        let report_b = writer_b.finalize().expect("finalize b");

        assert_eq!(report_a.total_raw_size, report_b.total_raw_size);
        assert_eq!(
            report_a.manifest.artifact_hash,
            report_b.manifest.artifact_hash
        );
        assert_eq!(
            report_a
                .manifest
                .chunks
                .iter()
                .map(|c| (c.raw_hash.clone(), c.raw_size))
                .collect::<Vec<_>>(),
            report_b
                .manifest
                .chunks
                .iter()
                .map(|c| (c.raw_hash.clone(), c.raw_size))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn test_fastcdc_finalize_flushes_tail_chunk() {
        let payload = b"tail-chunk-data";
        let tmp = tempfile::tempdir().expect("tmp");
        let mut writer = make_writer(&tmp);
        writer.write_bytes(payload).expect("write");
        let report = writer.finalize().expect("finalize");
        assert_eq!(report.total_raw_size, payload.len() as u64);
        assert_eq!(report.manifest.chunks.len(), 1);
        assert_eq!(report.manifest.chunks[0].raw_size, payload.len() as u32);
    }

    #[test]
    fn test_fastcdc_buffer_compaction_does_not_change_output() {
        let payload = (0..(17 * 1024 * 1024))
            .map(|i| (i as u8).wrapping_mul(11).wrapping_add(3))
            .collect::<Vec<_>>();

        let tmp_a = tempfile::tempdir().expect("tmp");
        let mut writer_a = make_writer(&tmp_a);
        feed_pattern(&mut writer_a, &payload, &[4096]);
        let report_a = writer_a.finalize().expect("finalize a");

        let tmp_b = tempfile::tempdir().expect("tmp");
        let mut writer_b = make_writer(&tmp_b);
        feed_pattern(&mut writer_b, &payload, &[payload.len()]);
        let report_b = writer_b.finalize().expect("finalize b");

        assert_eq!(
            report_a.manifest.artifact_hash,
            report_b.manifest.artifact_hash
        );
        assert_eq!(
            report_a
                .manifest
                .chunks
                .iter()
                .map(|chunk| (&chunk.raw_hash, chunk.raw_size))
                .collect::<Vec<_>>(),
            report_b
                .manifest
                .chunks
                .iter()
                .map(|chunk| (&chunk.raw_hash, chunk.raw_size))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_fastcdc_forces_cut_at_max_size() {
        let max_size = 4096usize;
        let payload_len = 32 * 1024 * 1024 + 123;
        let payload = vec![0u8; payload_len];
        let tmp = tempfile::tempdir().expect("tmp");
        let cas = CasStore::new(tmp.path()).expect("cas");
        let cfg = FastCdcWriterConfig {
            cdc_min: max_size as u32,
            cdc_avg: max_size as u32,
            cdc_max: max_size as u32,
            cdc_seed: 0,
            worker_count: 2,
            queue_depth: 8,
            zstd_level: 3,
        };
        let mut writer = FastCdcWriter::new(cfg, cas).expect("writer");
        feed_pattern(&mut writer, &payload, &[16 * 1024, 1024, 8192]);
        let report = writer.finalize().expect("finalize");
        assert_eq!(report.total_raw_size, payload_len as u64);

        for chunk in &report.manifest.chunks {
            assert!(
                (chunk.raw_size as usize) <= max_size,
                "chunk raw_size={} exceeded max_size={}",
                chunk.raw_size,
                max_size
            );
        }

        let expected_chunks = payload_len.div_ceil(max_size);
        assert_eq!(report.manifest.chunks.len(), expected_chunks);
        assert_eq!(
            report.manifest.chunks.last().map(|c| c.raw_size as usize),
            Some(payload_len % max_size)
                .filter(|v| *v != 0)
                .or(Some(max_size))
        );
    }
}
