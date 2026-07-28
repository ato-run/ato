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

use capsule::snapshot_manifest::{
    PortabilityTier, SNAPSHOT_COMPATIBILITY_V1_SCHEMA, SnapshotBackendKind,
    SnapshotCompatibilityContractV1,
};
use capsulefs::{CasStore, HotsetRecorder};

use crate::backend::{
    BackendCapabilities, BuildReadyStateInput, BuildReadyStateReceipt, DeviceProfile,
    FilesystemModel, GpuMode, IsolationBoundary, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, SnapshotError, SnapshotInspection, SnapshotKind,
    compatibility_class_identity,
};
use crate::manifest::{NoSecretProof, READY_STATE_SCHEMA, ReadyStateManifest, SnapshotBackendInfo};
use crate::scanner;

/// Fake backend's fixed Snapshot-v1 format generation. Kept in lock-step with
/// the "fake-v1" spelling used in its legacy [`SnapshotBackendInfo`].
const FAKE_SNAPSHOT_FORMAT_VERSION: u32 = 1;

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
            // The Fake backend never boots a VMM, so it has no UFFD mem-backend.
            supports_uffd_mem_backend: false,
            uffd_reason: Some("fake backend has no Firecracker UFFD mem-backend".to_string()),
            binding: Default::default(),
        }
    }

    fn snapshot_compatibility_contract(
        &self,
    ) -> Result<SnapshotCompatibilityContractV1, SnapshotError> {
        // Real (not placeholder) facts for a backend that captures no VM state:
        // fixed identity strings matching its legacy `SnapshotBackendInfo`
        // (kind="fake", version="fake-0.1.0", format="fake-v1"), a fixed "raw"
        // codec (no additional encoding layer), no CPU template (it boots
        // nothing), and a runner-restore-contract scoped by the host's real
        // architecture (mirrors the legacy `ato-fake-runner/<arch>/v1` id).
        let vmm_identity = "fake-0.1.0".to_string();
        let state_codec = "raw".to_string();
        let guest_kernel_identity = "none:fake-backend".to_string();
        let cpu_template = "none".to_string();
        let runner_restore_contract = format!("ato-fake-runner/{}/v1", std::env::consts::ARCH);
        let compatibility_class_identity = compatibility_class_identity(
            SnapshotBackendKind::Fake,
            FAKE_SNAPSHOT_FORMAT_VERSION,
            &vmm_identity,
            &state_codec,
            &guest_kernel_identity,
            &cpu_template,
            &runner_restore_contract,
        )?;
        Ok(SnapshotCompatibilityContractV1 {
            schema: SNAPSHOT_COMPATIBILITY_V1_SCHEMA.to_string(),
            backend: SnapshotBackendKind::Fake,
            format_version: FAKE_SNAPSHOT_FORMAT_VERSION,
            vmm_identity,
            state_codec,
            guest_kernel_identity,
            cpu_template,
            runner_restore_contract,
            portability_tier: PortabilityTier::ClassPortable,
            compatibility_class_identity,
        })
    }

    fn build_ready_state(
        &self,
        input: BuildReadyStateInput<'_>,
    ) -> Result<BuildReadyStateReceipt, SnapshotError> {
        // ── no-secret gate (plan §8.1) ──────────────────────────────────────
        // Fail CLOSED on declared markers (verbatim) and the high-precision
        // heuristics (provider-key prefixes, secret-named env). High-entropy
        // findings are ADVISORY only — they false-positive on lockfile hashes /
        // minified assets / binaries in real layers, so they are recorded in the
        // proof, not gated.
        // Seal + scan via the shared orchestration (same policy as Firecracker):
        // declared markers fail closed on every layer; provider/env block on
        // app/dependency; large opaque layers are advisory + content-cached +
        // budgeted. High-entropy is advisory everywhere.
        let cache = crate::scan_cache::ScanCache::open(input.store.root());
        let out = crate::seal::seal_and_scan(
            input.store,
            crate::seal::SealLayersRef {
                rootfs: &input.layers.rootfs,
                runtime: input.layers.runtime.as_deref(),
                dependency: input.layers.dependency.as_deref(),
                app: input.layers.app.as_deref(),
                vmstate: &input.layers.vmstate,
                memory: &input.layers.memory,
            },
            &input.declared_secret_markers,
            &cache,
            crate::seal::advisory_budget_from_env(),
            None,
        )?; // seal_and_scan fails closed (nothing stored) on declared/blocking hits
        let advisories = scanner::advisory_summaries_capped(&out.report, 50);
        let coverage = out.coverage;
        let sealed_bytes = out.sealed_bytes;
        let layers = out.layers;

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
            advisories,
            verdict: "clean".to_string(),
            coverage,
        };

        let manifest = ReadyStateManifest {
            schema: READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: input.capsule_manifest_hash,
            has_vsock: false, // Fake backend has no vsock device
            runner_class_id: input.runner_class,
            execution_id: input.execution_id.clone(),
            // `BuildReadyStateInput` does not yet carry a schema tag for the
            // declared execution id — that wiring is later, separate work.
            // Until then every sealed manifest is honestly legacy.
            execution_identity_schema: None,
            surface_requirement: input.surface_requirement,
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
            // v1.2 PR 3d: the Fake backend boots nothing, so it models the
            // supervisor input as pure plumbing — names recorded, no hygiene
            // cmdline, placeholder scan not evaluated (`None`, honestly).
            supervisor_build: input.supervisor.as_ref().map(|s| {
                crate::manifest::SupervisorBuildReceipt {
                    binding_names: s.binding_names.clone(),
                    page_hygiene_boot_args: false,
                    placeholder_absent_from_seal: None,
                    // v1.6 (ato#983) Slice 2: recorded honestly (round-trip fidelity for
                    // tests asserting receipt content) — the Fake backend attaches no
                    // real device, same as it boots no real kernel.
                    state_volumes: s.state_volumes.clone(),
                    state_owner_scope: s.state_owner_scope.clone(),
                }
            }),
        };

        Ok(BuildReadyStateReceipt {
            manifest,
            sealed_bytes,
            no_secret_proof,
            // The Fake backend boots nothing, so there is no live guest to
            // screenshot — honestly `None`.
            screenshot_png_base64: None,
        })
    }

    fn inspect(
        &self,
        store: &CasStore,
        manifest: &ReadyStateManifest,
    ) -> Result<SnapshotInspection, SnapshotError> {
        let all_present = manifest
            .layers
            .iter()
            .all(|(_, blob)| store.has_all_chunks(blob));
        Ok(SnapshotInspection {
            manifest_id: manifest.id(),
            backend_kind: manifest.snapshot_backend.kind.clone(),
            layers: manifest.layers.iter().map(|(n, _)| n.to_string()).collect(),
            total_bytes: manifest.total_layer_bytes(),
            all_chunks_present: all_present,
        })
    }

    fn restore(&self, input: RestoreReadyStateInput<'_>) -> Result<RestoreReceipt, SnapshotError> {
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
            manifest_id
                .strip_prefix("blake3:")
                .unwrap_or(&manifest_id)
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
            vmm_pid: None, // Fake has no serving process.
            vsock_uds: None,
            workload_addr: None, // Fake has no live listener — nothing to honestly expose.
        };

        Ok(RestoreReceipt {
            session,
            ready_state_manifest_id: manifest_id,
            // Fake never boots a guest, so nothing HTTP-probes a content-ready path.
            content_ready_ms: None,
        })
    }

    fn stop(
        &self,
        session: RestoredSession,
    ) -> Result<crate::backend::TeardownReceipt, SnapshotError> {
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

    fn build_input<'a>(
        store: &'a CasStore,
        secret_markers: Vec<String>,
    ) -> BuildReadyStateInput<'a> {
        BuildReadyStateInput {
            store,
            capsule_manifest_hash: "blake3:capsule".to_string(),
            runner_class: None,
            surface_requirement: None,
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
                ..Default::default()
            },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: secret_markers,
            execution_id: None,
            supervisor: None,
        }
    }

    #[test]
    fn probe_is_available_without_kvm() {
        let p = FakeSnapshotBackend::new().probe();
        assert!(p.available);
        assert_eq!(p.backend_id, FAKE_BACKEND_ID);
    }

    /// Track C (#912): a caller-supplied declared execution id is stamped VERBATIM into
    /// the sealed manifest — and absent when the caller passes `None` (a registry builder
    /// then fails closed instead of synthesizing one).
    #[test]
    fn execution_id_is_stamped_verbatim_into_the_sealed_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let mut input = build_input(&store, vec![]);
        let id = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        input.execution_id = Some(id.to_string());
        let receipt = FakeSnapshotBackend::new()
            .build_ready_state(input)
            .expect("build");
        assert_eq!(receipt.manifest.execution_id.as_deref(), Some(id));

        // None ⇒ the sealed manifest carries no execution id (nothing is invented).
        let store2 = CasStore::open(dir.path().join("cas2")).unwrap();
        let receipt2 = FakeSnapshotBackend::new()
            .build_ready_state(build_input(&store2, vec![]))
            .expect("build");
        assert_eq!(receipt2.manifest.execution_id, None);
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
                uffd_preview: false,
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
                !bytes
                    .windows(runtime_secret.len())
                    .any(|w| w == runtime_secret),
                "a post-restore secret must never appear in a sealed chunk"
            );
        }
        assert!(receipt.no_secret_proof.is_clean());
    }

    #[test]
    fn supervisor_input_is_recorded_honestly_in_the_manifest() {
        // v1.2 PR 3d plumbing (T3): the Fake backend carries the supervisor input
        // through build→manifest so KVM-less CI exercises the new field end to end —
        // names recorded, and the boot-dependent facts honestly absent (no hygiene
        // cmdline, placeholder scan not evaluated) because Fake boots nothing.
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let mut input = build_input(&store, vec![]);
        input.supervisor = Some(crate::backend::SupervisorBindings {
            binding_names: vec!["openai_api_key".to_string()],
            state_volumes: vec![],
            state_owner_scope: None,
        });
        let receipt = FakeSnapshotBackend::new().build_ready_state(input).unwrap();
        let sup = receipt
            .manifest
            .supervisor_build
            .as_ref()
            .expect("supervisor receipt");
        assert_eq!(sup.binding_names, vec!["openai_api_key"]);
        assert!(!sup.page_hygiene_boot_args);
        assert_eq!(sup.placeholder_absent_from_seal, None);
        // The manifest round-trips through serde with the new optional field.
        let json = serde_json::to_string(&receipt.manifest).unwrap();
        let back: ReadyStateManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.supervisor_build, receipt.manifest.supervisor_build);
        // A no-binding build stays byte-compatible: no supervisor_build key at all.
        let plain = FakeSnapshotBackend::new()
            .build_ready_state(build_input(&store, vec![]))
            .unwrap();
        assert!(plain.manifest.supervisor_build.is_none());
        assert!(
            !serde_json::to_string(&plain.manifest)
                .unwrap()
                .contains("supervisor_build")
        );
        // ato#1002 D4: an EMPTY binding set is an accepted supervisor build (a
        // zero-binding dockerfile import still runs guest-agent + supervisor) —
        // the manifest records supervisor_build honestly with the empty name set,
        // distinct from the no-binding recipe shape above (field absent).
        let mut empty_input = build_input(&store, vec![]);
        empty_input.supervisor = Some(crate::backend::SupervisorBindings {
            binding_names: vec![],
            state_volumes: vec![],
            state_owner_scope: None,
        });
        let empty = FakeSnapshotBackend::new()
            .build_ready_state(empty_input)
            .unwrap();
        let sup = empty
            .manifest
            .supervisor_build
            .as_ref()
            .expect("empty supervisor receipt recorded");
        assert!(sup.binding_names.is_empty());
        let json = serde_json::to_string(&empty.manifest).unwrap();
        assert!(json.contains(r#""supervisor_build""#), "{json}");
        let back: ReadyStateManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.supervisor_build, empty.manifest.supervisor_build);
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
                uffd_preview: false,
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
                uffd_preview: false,
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
            uffd_preview: false,
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
                uffd_preview: false,
            })
            .unwrap();
        assert_eq!(restore.session.restored_bytes, expected_total);
    }

    // ── real no-secret scanner integration (Workstream D) ──────────────────

    #[test]
    fn provider_key_in_memory_is_advisory_not_blocking() {
        // A heuristic hit in the MEMORY image is advisory, not gating: a real
        // guest-RAM image legitimately contains coincidental sk-+token runs, so
        // byte-heuristics there false-positive. The build succeeds; the finding
        // is recorded as an advisory; the no-secret guarantee for memory comes
        // from seal-before-bind + the cross-restore/no-secret invariant tests.
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();
        let mut input = build_input(&store, vec![]);
        let mut mem = vec![b' '; 64];
        mem.extend_from_slice(b"sk-proj-ABCDEFGHIJ1234567890abcdef");
        mem.extend_from_slice(&[b' '; 64]);
        input.layers.memory = mem;
        let receipt = backend
            .build_ready_state(input)
            .expect("memory heuristic must not block");
        assert!(receipt.no_secret_proof.is_clean());
        assert!(
            receipt
                .no_secret_proof
                .advisories
                .iter()
                .any(|a| a.contains("memory") && a.contains("provider-key")),
            "advisories: {:?}",
            receipt.no_secret_proof.advisories
        );
    }

    #[test]
    fn provider_key_in_app_still_fails_closed() {
        // The build-authored app/dependency layers DO fail closed on a provider key.
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();
        let mut input = build_input(&store, vec![]);
        input.layers.app = Some(b"token sk-proj-ABCDEFGHIJ1234567890abcdef end".to_vec());
        let err = backend.build_ready_state(input).unwrap_err();
        assert!(
            matches!(err, SnapshotError::SecretScanFindings(_)),
            "{err:?}"
        );
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
        assert!(
            matches!(err, SnapshotError::SecretScanFindings(_)),
            "{err:?}"
        );
        assert!(store.list_chunks().unwrap().is_empty());
    }

    #[test]
    fn clean_build_emits_real_no_secret_proof() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();
        let receipt = backend
            .build_ready_state(build_input(&store, vec![]))
            .unwrap();
        assert!(receipt.no_secret_proof.is_clean());
        assert_eq!(
            receipt.no_secret_proof.scanner_version,
            scanner::SCANNER_VERSION
        );
        assert!(receipt.no_secret_proof.findings.is_empty());
        assert!(
            receipt
                .no_secret_proof
                .scanned_layers
                .contains(&"memory".to_string())
        );
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

    #[test]
    fn realistic_dependency_and_app_layers_do_not_block_build() {
        // Regression: high-entropy content that is NOT a secret — lockfile
        // integrity hashes, a minified asset with a base64 data string — must
        // NOT fail the build closed. (High-entropy is advisory, not blocking.)
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let backend = FakeSnapshotBackend::new();
        let mut input = build_input(&store, vec![]);
        input.layers.dependency = Some(
            br#"{"packages":{"node_modules/left-pad":{"integrity":"sha512-aB3xY9kPqRsTuVwXyZ0123456789abcdefghijklmnopqrstuvwXYZ12=="},"node_modules/lodash":{"integrity":"sha512-Zz9YxWvUtSrQpOnMlKjIhGfEdCbA0987654321zyxwvutsrqponmlkj=="}}}"#
                .to_vec(),
        );
        input.layers.app = Some(
            b"var f=function(t){return t*2};const data=\"aGVsbG8gd29ybGQgdGhpcyBpcyBhIGJhc2U2NCBhc3NldA==\";".to_vec(),
        );
        // Build must SUCCEED (no blocking finding); the proof is clean.
        let receipt = backend
            .build_ready_state(input)
            .expect("realistic lockfile/minified layers must not block the build");
        assert!(
            receipt.no_secret_proof.is_clean(),
            "no blocking findings on realistic layers; advisories: {:?}",
            receipt.no_secret_proof.advisories
        );
    }

    #[test]
    fn scan_cache_hit_skips_rescan_on_second_build() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        // Build #1 (cold cache) scans + caches the base layers.
        let r1 = backend
            .build_ready_state(build_input(&store, vec![]))
            .unwrap();
        let rootfs1 = r1
            .no_secret_proof
            .coverage
            .iter()
            .find(|c| c.layer == "rootfs")
            .unwrap();
        assert_eq!(rootfs1.source, "scanned");
        // Build #2 (same store, identical layers) reuses the cache — no rescan.
        let r2 = backend
            .build_ready_state(build_input(&store, vec![]))
            .unwrap();
        let rootfs2 = r2
            .no_secret_proof
            .coverage
            .iter()
            .find(|c| c.layer == "rootfs")
            .unwrap();
        assert_eq!(
            rootfs2.source, "cache_hit",
            "identical rootfs must hit the scan cache"
        );
        // app stays "scanned" — build-authored layers are never cache-consulted.
        let app2 = r2
            .no_secret_proof
            .coverage
            .iter()
            .find(|c| c.layer == "app")
            .unwrap();
        assert_eq!(app2.source, "scanned");
    }

    #[test]
    fn poisoned_cache_cannot_suppress_app_blocking() {
        use capsulefs::{ChunkingKind, LayerKind, store_blob};
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        let app_bytes = b"token sk-proj-ABCDEFGHIJ1234567890abcdef end".to_vec();
        // Pre-write a "clean" cache entry at the app blob's id. app is NEVER
        // cache-consulted, so this poison must not suppress the fail-closed gate.
        let app_blob = store_blob(
            &store,
            LayerKind::App,
            &app_bytes,
            ChunkingKind::ContentDefined,
        )
        .unwrap();
        crate::scan_cache::ScanCache::open(store.root()).put(app_blob.id().hex(), false, &[]);
        let mut input = build_input(&store, vec![]);
        input.layers.app = Some(app_bytes);
        let err = backend.build_ready_state(input).unwrap_err();
        assert!(
            matches!(err, SnapshotError::SecretScanFindings(_)),
            "app must fail closed despite poisoned cache: {err:?}"
        );
    }

    #[test]
    fn advisory_budget_caps_large_opaque_layer() {
        // SAFETY: single-threaded test body; the var is restored at the end.
        let prev = std::env::var("ATO_SCAN_ADVISORY_BUDGET_BYTES").ok();
        unsafe { std::env::set_var("ATO_SCAN_ADVISORY_BUDGET_BYTES", "1024") };
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        // build_input's memory layer is 300_000 bytes > 1024 budget → capped.
        let r = backend
            .build_ready_state(build_input(&store, vec![]))
            .unwrap();
        let mem = r
            .no_secret_proof
            .coverage
            .iter()
            .find(|c| c.layer == "memory")
            .unwrap();
        assert_eq!(mem.coverage, "budget_capped");
        assert!(
            r.no_secret_proof.is_clean(),
            "budget cap is advisory — build stays clean"
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ATO_SCAN_ADVISORY_BUDGET_BYTES", v),
                None => std::env::remove_var("ATO_SCAN_ADVISORY_BUDGET_BYTES"),
            }
        }
    }
}
