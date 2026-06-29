//! `FakeSnapshotBackend` — a KVM-free backend that exercises the entire
//! Ready-State build→seal→restore→teardown pipeline through CapsuleFS, with no
//! VMM at all.
//!
//! It is the integration vehicle for hosts without `/dev/kvm` (e.g. the OCI A1
//! box): it content-addresses the layer bytes, runs the no-secret gate, seals a
//! real [`ReadyStateManifest`], reads the layers back on restore (proving the
//! round-trip), and manages a disposable overlay — everything except actually
//! booting a microVM. Swapping in `FirecrackerBackend` on a KVM host changes
//! only where the bytes come from, not the surrounding pipeline.

use std::path::Path;

use capsulefs::{
    BlobManifest, CasStore, ChunkingKind, HotsetRecorder, LayerKind, MEMORY_PAGE_CHUNK_SIZE,
    store_blob,
};

use crate::backend::{
    BackendCapabilities, BuildReadyStateInput, BuildReadyStateReceipt, DeviceProfile,
    FilesystemModel, GpuMode, IsolationBoundary, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, SnapshotError, SnapshotInspection, SnapshotKind,
};
use crate::manifest::{
    NoSecretProof, ReadyStateLayers, ReadyStateManifest, SnapshotBackendInfo, READY_STATE_SCHEMA,
};
use crate::scanner;

/// Backend id reported by [`FakeSnapshotBackend`].
pub const FAKE_BACKEND_ID: &str = "fake";

/// A deterministic, KVM-free snapshot backend for tests and KVM-less hosts.
#[derive(Debug, Clone, Default)]
pub struct FakeSnapshotBackend;

impl FakeSnapshotBackend {
    pub fn new() -> Self {
        Self
    }
}

/// Store one optional layer, returning its `BlobManifest`.
fn seal_layer(
    store: &CasStore,
    kind: LayerKind,
    bytes: Option<&[u8]>,
    chunking: ChunkingKind,
) -> Result<Option<BlobManifest>, SnapshotError> {
    match bytes {
        Some(b) => Ok(Some(store_blob(store, kind, b, chunking)?)),
        None => Ok(None),
    }
}

impl SnapshotBackend for FakeSnapshotBackend {
    fn id(&self) -> &str {
        FAKE_BACKEND_ID
    }

    fn probe(&self) -> BackendCapabilities {
        BackendCapabilities {
            backend_id: FAKE_BACKEND_ID.to_string(),
            // The fake backend is always available; it needs no KVM.
            available: true,
            reason: None,
            arch: std::env::consts::ARCH.to_string(),
            kvm_present: Path::new("/dev/kvm").exists(),
            vmm_version: Some("fake-0.1.0".to_string()),
            // It stands in for a sealable microVM so the KVM-free E2E exercises
            // the real Ready-State path (microvm + seal-before-bind + overlay).
            snapshot_kind: SnapshotKind::MicroVm,
            memory_snapshot: true,
            filesystem_model: FilesystemModel::Block,
            device_profile: DeviceProfile::Minimal,
            gpu_mode: GpuMode::None,
            oci_native: false,
            isolation_boundary: IsolationBoundary::MicroVm,
            supports_seal_before_bind: true,
            supports_disposable_overlay: true,
        }
    }

    fn build_ready_state(
        &self,
        input: BuildReadyStateInput<'_>,
    ) -> Result<BuildReadyStateReceipt, SnapshotError> {
        // ── no-secret gate (plan §8.1): fail closed on any finding ──────────
        // Declared markers first (verbatim, legacy error), then the heuristic
        // all-layer scanner (provider keys / secret env / high-entropy tokens).
        let report = scanner::scan_build_layers(&input.layers, &input.declared_secret_markers);
        if !report.declared_hits.is_empty() {
            return Err(SnapshotError::SecretFoundInSnapshot(report.declared_hits));
        }
        if !report.heuristic.is_empty() {
            return Err(SnapshotError::SecretScanFindings(report.heuristic));
        }

        let cd = ChunkingKind::ContentDefined;
        let page = ChunkingKind::PageAligned {
            page_size: MEMORY_PAGE_CHUNK_SIZE as u64,
        };

        let layers = ReadyStateLayers {
            rootfs: seal_layer(input.store, LayerKind::Rootfs, Some(&input.layers.rootfs), cd)?,
            runtime: seal_layer(
                input.store,
                LayerKind::Runtime,
                input.layers.runtime.as_deref(),
                cd,
            )?,
            dependency: seal_layer(
                input.store,
                LayerKind::Dependency,
                input.layers.dependency.as_deref(),
                cd,
            )?,
            app: seal_layer(input.store, LayerKind::App, input.layers.app.as_deref(), cd)?,
            // VM state is small + structured; content-defined is fine.
            vmstate: seal_layer(
                input.store,
                LayerKind::VmState,
                Some(&input.layers.vmstate),
                cd,
            )?,
            // Memory image is page-chunked for demand paging.
            memory: seal_layer(input.store, LayerKind::Memory, Some(&input.layers.memory), page)?,
        };

        // Hotset: memory pages first (the demand-paging hot path), then rootfs.
        let mut rec = HotsetRecorder::new();
        if let Some(mem) = &layers.memory {
            for c in &mem.chunks {
                rec.record(&c.hash);
            }
        }
        if let Some(rootfs) = &layers.rootfs {
            for c in &rootfs.chunks {
                rec.record(&c.hash);
            }
        }
        let hotset_profile = rec.finish();

        let no_secret_proof = NoSecretProof {
            scanner_version: scanner::SCANNER_VERSION.to_string(),
            scanned_layers: layers.iter().map(|(n, _)| n.to_string()).collect(),
            findings: Vec::new(),
            verdict: "clean".to_string(),
        };

        let sealed_bytes = layers.iter().map(|(_, m)| m.total_len).sum();

        let manifest = ReadyStateManifest {
            schema: READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: input.capsule_manifest_hash,
            runner_class_id: input.runner_class,
            execution_id: None,
            layers,
            hotset_profile,
            snapshot_backend: SnapshotBackendInfo {
                kind: FAKE_BACKEND_ID.to_string(),
                version: "0.1.0".to_string(),
                snapshot_format_version: "fake-v1".to_string(),
                cpu_template: None,
            },
            restore_contract: input.restore_contract,
            sanitizer_contract: input.sanitizer_contract,
            no_secret_proof: Some(no_secret_proof.clone()),
            build_receipt_id: None,
        };

        Ok(BuildReadyStateReceipt {
            manifest,
            sealed_bytes,
            no_secret_proof,
        })
    }

    fn inspect(
        &self,
        store: &CasStore,
        manifest: &ReadyStateManifest,
    ) -> Result<SnapshotInspection, SnapshotError> {
        let mut all_present = true;
        for (_, blob) in manifest.layers.iter() {
            for chunk in &blob.chunks {
                if !store.has_chunk(&chunk.hash) {
                    all_present = false;
                }
            }
        }
        Ok(SnapshotInspection {
            manifest_id: manifest.id(),
            backend_kind: manifest.snapshot_backend.kind.clone(),
            layers: manifest.layers.iter().map(|(n, _)| n.to_string()).collect(),
            total_bytes: manifest.total_layer_bytes(),
            all_chunks_present: all_present,
        })
    }

    fn restore(
        &self,
        input: RestoreReadyStateInput<'_>,
    ) -> Result<RestoreReceipt, SnapshotError> {
        // ── runner-class gate (fail-closed) ─────────────────────────────────
        // A snapshot pinned to a runner class is only restorable on a host
        // *proven* to match it. Unknown host class is NOT compatible — it is
        // rejected, never waved through. With only ids in hand (facts are a
        // build-host concern) the divergent field is the id itself.
        match (&input.manifest.runner_class_id, &input.host_runner_class) {
            (Some(expected), Some(actual)) if expected != actual => {
                return Err(SnapshotError::RunnerClassMismatch(
                    capsule::foundation::install_lifecycle::RunnerClassMismatch {
                        expected: expected.clone(),
                        actual: actual.clone(),
                        first_divergent_field: "runner_class_id".to_string(),
                    },
                ));
            }
            (Some(expected), None) => {
                return Err(SnapshotError::MissingHostRunnerClass {
                    expected: expected.clone(),
                });
            }
            // Both present and equal, or the snapshot pins no class → ok.
            _ => {}
        }

        // ── rehydrate layers (proves the CapsuleFS round-trip) ──────────────
        let mut restored_bytes = 0u64;
        for (_, blob) in input.manifest.layers.iter() {
            let reader = capsulefs::LazyBlobReader::new(input.store, blob);
            // Warm the hot pages first, then fully read the layer.
            reader.prefetch_hotset(&input.manifest.hotset_profile)?;
            let bytes = reader.read_all()?;
            restored_bytes += bytes.len() as u64;
        }

        // ── disposable overlay (writable scratch over the read-only base) ────
        std::fs::create_dir_all(&input.overlay_root)?;

        let manifest_id = input.manifest.id();
        let overlay_name = input
            .overlay_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("overlay");
        let session_id = format!(
            "fake-{}-{}",
            manifest_id.strip_prefix("blake3:").unwrap_or(&manifest_id)
                .get(..12)
                .unwrap_or("000000000000"),
            overlay_name
        );

        let session = RestoredSession {
            session_id,
            backend_id: FAKE_BACKEND_ID.to_string(),
            guest_port: input.manifest.restore_contract.ports.first().copied(),
            overlay_root: input.overlay_root,
            restored_bytes,
        };

        Ok(RestoreReceipt {
            session,
            ready_state_manifest_id: manifest_id,
        })
    }

    fn stop(&self, session: RestoredSession) -> Result<crate::backend::TeardownReceipt, SnapshotError> {
        let overlay_removed = if session.overlay_root.exists() {
            std::fs::remove_dir_all(&session.overlay_root)?;
            true
        } else {
            false
        };
        Ok(crate::backend::TeardownReceipt {
            session_id: session.session_id,
            overlay_removed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BuildLayers;
    use crate::manifest::{RestoreContract, SanitizerContract};

    fn build_input<'a>(store: &'a CasStore, secret_markers: Vec<String>) -> BuildReadyStateInput<'a> {
        BuildReadyStateInput {
            store,
            capsule_manifest_hash: "blake3:capsule".to_string(),
            runner_class: None,
            layers: BuildLayers {
                rootfs: b"fake rootfs image bytes".to_vec(),
                runtime: Some(b"python runtime layer".to_vec()),
                dependency: None,
                app: Some(b"the app build output".to_vec()),
                vmstate: vec![0xABu8; 4096],
                memory: (0..300_000u32).map(|i| (i % 256) as u8).collect(),
            },
            restore_contract: RestoreContract {
                expected_ready_ms: Some(2000),
                ports: vec![8080],
                healthcheck: Some("/health".to_string()),
            },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: secret_markers,
        }
    }

    #[test]
    fn probe_is_available_without_kvm() {
        let p = FakeSnapshotBackend::new().probe();
        assert!(p.available);
        assert_eq!(p.backend_id, FAKE_BACKEND_ID);
    }

    #[test]
    fn build_restore_teardown_e2e() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();

        // Build / seal.
        let receipt = backend
            .build_ready_state(build_input(&store, vec![]))
            .expect("build should succeed");
        assert!(receipt.no_secret_proof.is_clean());
        assert!(receipt.sealed_bytes > 300_000);
        let manifest = receipt.manifest.clone();
        assert_eq!(manifest.schema, READY_STATE_SCHEMA);
        assert!(manifest.layers.rootfs.is_some());
        assert!(manifest.layers.memory.is_some());
        assert!(!manifest.hotset_profile.is_empty());

        // Inspect: all chunks present.
        let inspection = backend.inspect(&store, &manifest).unwrap();
        assert!(inspection.all_chunks_present);
        assert!(inspection.layers.contains(&"memory".to_string()));

        // Restore.
        let overlay = dir.path().join("overlays").join("sess-1");
        let restore = backend
            .restore(RestoreReadyStateInput {
                store: &store,
                manifest: manifest.clone(),
                overlay_root: overlay.clone(),
                host_runner_class: None,
            })
            .expect("restore should succeed");
        assert_eq!(restore.session.guest_port, Some(8080));
        assert_eq!(restore.session.restored_bytes, manifest.total_layer_bytes());
        assert!(overlay.exists(), "overlay must be created");
        assert_eq!(restore.ready_state_manifest_id, manifest.id());

        // Teardown: overlay destroyed.
        let teardown = backend.stop(restore.session.clone()).unwrap();
        assert!(teardown.overlay_removed);
        assert!(!overlay.exists(), "overlay must be destroyed on stop");
    }

    #[test]
    fn no_secret_gate_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();

        // Plant the secret marker in a layer and declare it.
        let mut input = build_input(&store, vec!["SECRET_TOKEN_abc123".to_string()]);
        input.layers.app = Some(b"config: token=SECRET_TOKEN_abc123;".to_vec());

        let err = backend.build_ready_state(input).unwrap_err();
        match err {
            SnapshotError::SecretFoundInSnapshot(found) => {
                assert_eq!(found, vec!["SECRET_TOKEN_abc123".to_string()]);
            }
            other => panic!("expected SecretFoundInSnapshot, got {other:?}"),
        }
        // Nothing sealed.
        assert!(store.list_chunks().unwrap().is_empty());
    }

    #[test]
    fn seal_before_bind_keeps_runtime_secret_out_of_snapshot() {
        // The no-secret invariant: a secret that only appears at restore/bind
        // time is never present in the sealed layers. Here the build input has
        // no secret; we scan the sealed chunks afterwards for a value that the
        // app will only receive post-restore.
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();

        let runtime_secret = b"OPENAI_API_KEY=sk-post-restore-only";
        let receipt = backend
            .build_ready_state(build_input(&store, vec![]))
            .unwrap();

        // Grep every sealed chunk for the post-restore secret — must be absent.
        for hash in store.list_chunks().unwrap() {
            let bytes = store.get_chunk(&hash).unwrap();
            assert!(
                !bytes.windows(runtime_secret.len()).any(|w| w == runtime_secret),
                "a post-restore secret must never appear in a sealed chunk"
            );
        }
        assert!(receipt.no_secret_proof.is_clean());
    }

    #[test]
    fn runner_class_mismatch_fails_closed_on_restore() {
        use capsule::foundation::install_lifecycle::RunnerClassId;
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();

        let mut input = build_input(&store, vec![]);
        input.runner_class = Some(RunnerClassId::from_hash("blake3:class-A"));
        let manifest = backend.build_ready_state(input).unwrap().manifest;

        let err = backend
            .restore(RestoreReadyStateInput {
                store: &store,
                manifest,
                overlay_root: dir.path().join("ov"),
                host_runner_class: Some(RunnerClassId::from_hash("blake3:class-B")),
            })
            .unwrap_err();
        assert!(matches!(err, SnapshotError::RunnerClassMismatch(_)));
    }

    #[test]
    fn restore_fails_closed_when_host_runner_class_unknown() {
        use capsule::foundation::install_lifecycle::RunnerClassId;
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();

        // Snapshot pins a class; the restore host's class is unknown (None).
        // Unknown != compatible → must reject.
        let mut input = build_input(&store, vec![]);
        input.runner_class = Some(RunnerClassId::from_hash("blake3:class-A"));
        let manifest = backend.build_ready_state(input).unwrap().manifest;

        let err = backend
            .restore(RestoreReadyStateInput {
                store: &store,
                manifest,
                overlay_root: dir.path().join("ov"),
                host_runner_class: None,
            })
            .unwrap_err();
        assert!(
            matches!(err, SnapshotError::MissingHostRunnerClass { .. }),
            "unknown host class must fail closed, got {err:?}"
        );
    }

    #[test]
    fn restore_succeeds_when_runner_class_matches() {
        use capsule::foundation::install_lifecycle::RunnerClassId;
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();

        let class = RunnerClassId::from_hash("blake3:same-class");
        let mut input = build_input(&store, vec![]);
        input.runner_class = Some(class.clone());
        let manifest = backend.build_ready_state(input).unwrap().manifest;

        let ok = backend.restore(RestoreReadyStateInput {
            store: &store,
            manifest,
            overlay_root: dir.path().join("ov"),
            host_runner_class: Some(class),
        });
        assert!(ok.is_ok());
    }

    #[test]
    fn restored_bytes_match_a_separate_full_read() {
        // Cross-check the round-trip: the bytes restore reports equal a direct
        // read of every layer.
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();
        let original = build_input(&store, vec![]);
        let expected_total: u64 = original.layers.rootfs.len() as u64
            + original.layers.runtime.as_ref().unwrap().len() as u64
            + original.layers.app.as_ref().unwrap().len() as u64
            + original.layers.vmstate.len() as u64
            + original.layers.memory.len() as u64;

        let manifest = backend.build_ready_state(original).unwrap().manifest;
        let restore = backend
            .restore(RestoreReadyStateInput {
                store: &store,
                manifest,
                overlay_root: dir.path().join("ov2"),
                host_runner_class: None,
            })
            .unwrap();
        assert_eq!(restore.session.restored_bytes, expected_total);
    }

    // ── real no-secret scanner integration (Workstream D) ──────────────────

    #[test]
    fn build_fails_closed_on_planted_provider_key_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();
        let mut input = build_input(&store, vec![]);
        // Plant a provider-key-shaped token in the memory image (no declared marker).
        let mut mem = vec![b' '; 64];
        mem.extend_from_slice(b"sk-proj-ABCDEFGHIJ1234567890abcdef");
        mem.extend_from_slice(&[b' '; 64]);
        input.layers.memory = mem;
        let err = backend.build_ready_state(input).unwrap_err();
        assert!(matches!(err, SnapshotError::SecretScanFindings(_)), "{err:?}");
        assert!(store.list_chunks().unwrap().is_empty(), "nothing sealed");
    }

    #[test]
    fn build_fails_closed_on_env_style_secret_in_app() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();
        let mut input = build_input(&store, vec![]);
        input.layers.app = Some(b"API_KEY=sk-live-deadbeefcafef00d12345".to_vec());
        let err = backend.build_ready_state(input).unwrap_err();
        assert!(matches!(err, SnapshotError::SecretScanFindings(_)), "{err:?}");
        assert!(store.list_chunks().unwrap().is_empty());
    }

    #[test]
    fn clean_build_emits_real_no_secret_proof() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();
        let receipt = backend.build_ready_state(build_input(&store, vec![])).unwrap();
        assert!(receipt.no_secret_proof.is_clean());
        assert_eq!(receipt.no_secret_proof.scanner_version, scanner::SCANNER_VERSION);
        assert!(receipt.no_secret_proof.findings.is_empty());
        assert!(receipt
            .no_secret_proof
            .scanned_layers
            .contains(&"memory".to_string()));
    }

    #[test]
    fn scan_findings_error_is_non_leaking() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();
        let mut input = build_input(&store, vec![]);
        let secret_suffix = "DEADBEEFcafef00d12345678";
        input.layers.app = Some(format!("ghp_{secret_suffix}").into_bytes());
        let err = backend.build_ready_state(input).unwrap_err();
        // Neither the Debug nor the Display of the error may contain the secret.
        assert!(!format!("{err:?}").contains(secret_suffix));
        assert!(!err.to_string().contains(secret_suffix));
    }
}
