//! Ready-State build: Boot/Snapshot/Seal (E3/E4), driven against the selected
//! snapshot backend (Fake on a KVM-less host).
//!
//! The caller assembles raw [`BuildLayers`] from the frozen build outputs; this
//! module derives the restore/sanitizer contracts + declared secret markers
//! from the manifest, runs the GPU fail-closed guard, and calls
//! `build_ready_state` (whose no-secret gate fails the build closed). On success
//! it persists the sealed [`ReadyStateManifest`] next to its CAS store.

use std::path::Path;

use anyhow::{Context, Result};
use capsule::execution_contract::ExecutionId;
use capsule::types::CapsuleManifest;
use snapshot::{
    ArtifactEnvelopeV1, BuildLayers, BuildReadyStateInput, BuildReadyStateReceipt, RestoreContract,
    SanitizerContract, SanitizerLayer, SanitizerStep, SnapshotBackend, WarmupRecipe,
    accept_platform_verified_candidate, ensure_gpu_not_in_snapshot, migrate_legacy_manifest,
};

use super::store;

/// Derive the restore contract (ports / healthcheck / SLO / warmup) from the manifest.
pub(crate) fn restore_contract_from_manifest(m: &CapsuleManifest) -> RestoreContract {
    let mut ports: Vec<u16> = Vec::new();
    if let Some(targets) = m.targets.as_ref() {
        if let Some(p) = targets.port {
            ports.push(p);
        }
        for nt in targets.named_targets().values() {
            if let Some(p) = nt.port {
                ports.push(p);
            }
        }
    }
    ports.sort_unstable();
    ports.dedup();

    // Healthcheck: the first concrete http_get probe path on any target.
    let healthcheck = m.targets.as_ref().and_then(|t| {
        t.named_targets()
            .values()
            .find_map(|nt| nt.readiness_probe.as_ref().and_then(|p| p.http_get.clone()))
    });

    let snapshot_cfg = m.snapshot_config();
    let expected_ready_ms = snapshot_cfg
        .max_restore_seconds
        .map(|s| s.saturating_mul(1000));
    // The author's first-screen warmup recipe rides the sealed artifact. Paths
    // are enforced by the backend's warmup gate (one enforcement point, shared
    // with the builder lanes), so this stays a pure copy.
    let warmup = WarmupRecipe::from_snapshot_config(&snapshot_cfg);

    RestoreContract {
        expected_ready_ms,
        ports,
        healthcheck,
        warmup_paths: warmup.warmup_paths,
        stable_successes: warmup.stable_successes,
        stable_interval_ms: warmup.stable_interval_ms,
        content_ready_path: warmup.content_ready_path,
        ..Default::default()
    }
}

/// Derive the post-resume sanitizer steps. When `sanitize_after_restore` is on
/// (the default), emit the standard ordered step set (plan §8.2); else empty.
pub(crate) fn sanitizer_contract_from_manifest(m: &CapsuleManifest) -> SanitizerContract {
    if !m.snapshot_config().sanitize_after_restore {
        return SanitizerContract::default();
    }
    let steps = vec![
        SanitizerStep {
            step: "regenerate_ids".into(),
            layer: SanitizerLayer::GuestAgent,
        },
        SanitizerStep {
            step: "reseed_entropy".into(),
            layer: SanitizerLayer::GuestAgent,
        },
        SanitizerStep {
            step: "refresh_clock".into(),
            layer: SanitizerLayer::GuestAgent,
        },
        SanitizerStep {
            step: "reset_sockets".into(),
            layer: SanitizerLayer::GuestAgent,
        },
        SanitizerStep {
            step: "reconnect_net".into(),
            layer: SanitizerLayer::HostAndGuest,
        },
        SanitizerStep {
            step: "port_remap".into(),
            layer: SanitizerLayer::Host,
        },
    ];
    SanitizerContract { steps }
}

/// Declared secret markers to scan the sealed layers for: the `[secrets.*]`
/// names and their target env-var names (the build holds no values — these are
/// names a leaked value would likely be labeled with).
pub(crate) fn declared_secret_markers(m: &CapsuleManifest) -> Vec<String> {
    let mut markers = Vec::new();
    for (name, spec) in m.secrets.iter() {
        markers.push(name.clone());
        if let Some(env) = spec.env.as_ref() {
            markers.push(env.clone());
        }
    }
    markers.sort();
    markers.dedup();
    markers
}

/// Boot/Snapshot/Seal: GPU fail-closed guard → build_ready_state (no-secret gate
/// inside) → persist the sealed manifest. Returns the build receipt.
pub(crate) fn seal(
    state_root: &Path,
    capsule_manifest_hash: String,
    manifest: &CapsuleManifest,
    layers: BuildLayers,
    backend: &dyn SnapshotBackend,
    v1_execution_id: Option<ExecutionId>,
) -> Result<BuildReadyStateReceipt> {
    // C guard: never seal an in-VM GPU into the snapshot.
    ensure_gpu_not_in_snapshot(manifest.gpu_mode())
        .context("Ready-State build refused: GPU state is not snapshottable")?;

    // Phase 8 hard invariant: a Ready-State seal is ALWAYS produced from a pre-bind
    // boot — never from a bound running session (post-bind state is dirty). `ato build`
    // boots fresh with no bindings attached, so this is `false` here; the guard makes
    // the invariant explicit at the one place a seal is produced and fails closed if a
    // future path ever tries to seal a bound session.
    super::binding_host::ensure_pre_bind_before_seal(/* session_is_bound = */ false)?;

    let mut v1_staging = v1_execution_id
        .as_ref()
        .map(|execution_id| store::V1StagingArtifact::create(state_root, execution_id))
        .transpose()?;
    let store = match &v1_staging {
        Some(staging) => staging.open_store()?,
        None => store::open_store(state_root, &capsule_manifest_hash)?,
    };
    // Delegate runner-class resolution to the backend (same contract as the
    // snapshot-builder daemon and `runner serve`): `None` lets Firecracker pin
    // the seal to its real facts (snapshot format, VMM version, guest kernel
    // hash) instead of the KVM-free `from_host()` probe whose backend facets
    // are sentinels. The Fake backend seals unpinned, matching builder-driven
    // fake seals.
    let runner_class = None;
    let surface_requirement = manifest.resolve_default_target()?.surface.clone();

    let receipt = backend
        .build_ready_state(BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: capsule_manifest_hash.clone(),
            runner_class,
            surface_requirement,
            layers,
            restore_contract: restore_contract_from_manifest(manifest),
            sanitizer_contract: sanitizer_contract_from_manifest(manifest),
            declared_secret_markers: declared_secret_markers(manifest),
            execution_id: v1_execution_id
                .as_ref()
                .map(|execution_id| execution_id.to_string()),
            execution_identity_schema: v1_execution_id
                .as_ref()
                .map(|_| capsule::execution_contract::EXECUTION_CONTRACT_V1_SCHEMA.to_string()),
            supervisor: None,
        })
        .context("snapshot backend build_ready_state failed")?;

    if let Some(execution_id) = v1_execution_id {
        let compatibility = backend
            .snapshot_compatibility_contract()
            .context("resolve Snapshot v1 backend compatibility")?;
        let v1_manifest = migrate_legacy_manifest(&receipt.manifest, execution_id, compatibility)
            .context("create Snapshot v1 manifest")?;

        // Acceptance is a real disposable restore, not successful serialization.
        // The v1 manifest is persisted only after restore and teardown both pass.
        let accepted = accept_platform_verified_candidate(
            snapshot::CandidateSnapshot {
                manifest: v1_manifest,
            },
            || {
                let restored = backend
                    .restore(snapshot::RestoreReadyStateInput {
                        store: &store,
                        manifest: receipt.manifest.clone(),
                        overlay_root: v1_staging
                            .as_ref()
                            .expect("v1 staging exists while accepting a v1 Snapshot")
                            .artifact_dir()
                            .join("acceptance-overlay"),
                        host_runner_class: None,
                        uffd_preview: false,
                    })
                    .map_err(|error| error.to_string())?;
                backend
                    .stop(restored.session)
                    .map_err(|error| error.to_string())?;
                Ok(())
            },
        )
        .context("Snapshot v1 disposable acceptance failed")?;
        let envelope = ArtifactEnvelopeV1::accepted(&receipt.manifest, &accepted.manifest)
            .context("create authenticated Snapshot Artifact Envelope")?;
        v1_staging
            .take()
            .expect("v1 staging exists while publishing a v1 Snapshot")
            .commit(state_root, &receipt.manifest, &accepted.manifest, &envelope)?;
    } else {
        store::save_manifest(state_root, &receipt.manifest)?;
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(extra: &str) -> CapsuleManifest {
        let base = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python app.py"
port = 8080

[targets.app.readiness_probe]
type = "http"
path = "/health"
"#;
        CapsuleManifest::from_toml(&format!("{base}\n{extra}")).expect("parse")
    }

    #[test]
    fn restore_contract_maps_ports() {
        let c = restore_contract_from_manifest(&parse(
            "[snapshot]\nmode=\"warm\"\nmax_restore_seconds=8\n",
        ));
        assert!(c.ports.contains(&8080));
        assert_eq!(c.expected_ready_ms, Some(8000));
        // Defaults: no warmup, no content_ready_path ⇒ v1 healthcheck-only seal.
        assert!(c.warmup_paths.is_empty());
        assert_eq!(c.stable_successes, None);
        assert_eq!(c.stable_interval_ms, None);
        assert_eq!(c.content_ready_path, None);
    }

    #[test]
    fn restore_contract_copies_warmup_fields() {
        let c = restore_contract_from_manifest(&parse(
            "\
[snapshot]\n\
mode=\"warm\"\n\
warmup_paths=[\"/\",\"/api/health\"]\n\
stable_successes=3\n\
stable_interval_ms=200\n\
content_ready_path=\"/\"\n",
        ));
        assert_eq!(c.warmup_paths, vec!["/", "/api/health"]);
        assert_eq!(c.stable_successes, Some(3));
        assert_eq!(c.stable_interval_ms, Some(200));
        assert_eq!(c.content_ready_path.as_deref(), Some("/"));
    }

    #[test]
    fn sanitizer_contract_present_by_default_and_empty_when_disabled() {
        assert!(
            !sanitizer_contract_from_manifest(&parse("[snapshot]\nmode=\"warm\"\n"))
                .steps
                .is_empty()
        );
        let off = parse("[snapshot]\nmode=\"warm\"\nsanitize_after_restore=false\n");
        assert!(sanitizer_contract_from_manifest(&off).steps.is_empty());
    }

    #[test]
    fn declared_secret_markers_collects_names_and_env() {
        let m = parse("[secrets.openai_api_key]\nenv=\"OPENAI_API_KEY\"\n");
        let markers = declared_secret_markers(&m);
        assert!(markers.contains(&"openai_api_key".to_string()));
        assert!(markers.contains(&"OPENAI_API_KEY".to_string()));
    }

    #[test]
    fn seal_persists_manifest_and_runs_gates() {
        let dir = tempfile::tempdir().unwrap();
        let backend = snapshot::FakeSnapshotBackend::new();
        let m = parse("[snapshot]\nmode=\"warm\"\n");
        let layers = BuildLayers {
            rootfs: b"rootfs".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"the app".to_vec()),
            vmstate: vec![0xAB; 256],
            memory: (0..100_000u32).map(|i| (i % 256) as u8).collect(),
        };
        let receipt = seal(
            dir.path(),
            "blake3:capsule".to_string(),
            &m,
            layers,
            &backend,
            None,
        )
        .unwrap();
        assert!(receipt.no_secret_proof.is_clean());
        // The sealed manifest is loadable from disk.
        let loaded = store::load_manifest(dir.path(), "blake3:capsule")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.id(), receipt.manifest.id());
    }

    #[test]
    fn seal_refuses_in_vm_gpu() {
        let dir = tempfile::tempdir().unwrap();
        let backend = snapshot::FakeSnapshotBackend::new();
        let m = parse("[snapshot]\nmode=\"warm\"\n[requirements]\nvram_min=\"8GB\"\n");
        let layers = BuildLayers {
            rootfs: b"r".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"a".to_vec()),
            vmstate: vec![0u8; 16],
            memory: vec![0u8; 16],
        };
        let err = seal(
            dir.path(),
            "blake3:gpu".to_string(),
            &m,
            layers,
            &backend,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("GPU"));
    }

    /// Wraps the Fake backend and records the `runner_class` the CLI hands to
    /// `build_ready_state`, so delegation is asserted explicitly rather than
    /// inferred from the sealed output.
    struct RecordingBackend {
        inner: snapshot::FakeSnapshotBackend,
        seen_runner_class:
            std::sync::Mutex<Option<Option<capsule::foundation::install_lifecycle::RunnerClassId>>>,
    }

    impl SnapshotBackend for RecordingBackend {
        fn id(&self) -> &str {
            self.inner.id()
        }
        fn probe(&self) -> snapshot::BackendCapabilities {
            self.inner.probe()
        }
        fn snapshot_compatibility_contract(
            &self,
        ) -> Result<snapshot::SnapshotCompatibilityContract, snapshot::SnapshotError> {
            self.inner.snapshot_compatibility_contract()
        }
        fn build_ready_state(
            &self,
            input: BuildReadyStateInput<'_>,
        ) -> Result<BuildReadyStateReceipt, snapshot::SnapshotError> {
            *self.seen_runner_class.lock().unwrap() = Some(input.runner_class.clone());
            self.inner.build_ready_state(input)
        }
        fn inspect(
            &self,
            store: &capsulefs::CasStore,
            manifest: &snapshot::ReadyStateManifest,
        ) -> Result<snapshot::SnapshotInspection, snapshot::SnapshotError> {
            self.inner.inspect(store, manifest)
        }
        fn restore(
            &self,
            input: snapshot::RestoreReadyStateInput<'_>,
        ) -> Result<snapshot::RestoreReceipt, snapshot::SnapshotError> {
            self.inner.restore(input)
        }
        fn stop(
            &self,
            session: snapshot::RestoredSession,
        ) -> Result<snapshot::TeardownReceipt, snapshot::SnapshotError> {
            self.inner.stop(session)
        }
    }

    #[test]
    fn seal_delegates_runner_class_resolution_to_backend() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RecordingBackend {
            inner: snapshot::FakeSnapshotBackend::new(),
            seen_runner_class: std::sync::Mutex::new(None),
        };
        let m = parse("[snapshot]\nmode=\"warm\"\n");
        let layers = BuildLayers {
            rootfs: b"r".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"a".to_vec()),
            vmstate: vec![0u8; 64],
            memory: vec![0u8; 4096],
        };
        let receipt = seal(
            dir.path(),
            "blake3:rc".to_string(),
            &m,
            layers,
            &backend,
            None,
        )
        .unwrap();
        assert_eq!(
            *backend.seen_runner_class.lock().unwrap(),
            Some(None),
            "CLI seal must pass runner_class=None so the backend resolves its own class"
        );
        assert!(
            receipt.manifest.runner_class_id.is_none(),
            "Fake echoes the input verbatim: an unpinned seal proves the CLI delegated"
        );
    }

    #[test]
    fn v1_seal_requires_disposable_restore_before_persisting_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let backend = snapshot::FakeSnapshotBackend::new();
        let m = parse("[snapshot]\nmode=\"warm\"\n");
        let layers = BuildLayers {
            rootfs: b"rootfs".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"app".to_vec()),
            vmstate: vec![0u8; 64],
            memory: vec![1u8; 4096],
        };
        let execution_id = ExecutionId::new(format!("blake3:{}", "a".repeat(64))).unwrap();

        let receipt = seal(
            dir.path(),
            "blake3:v1".to_string(),
            &m,
            layers,
            &backend,
            Some(execution_id.clone()),
        )
        .unwrap();

        assert_eq!(
            receipt.manifest.execution_id.as_deref(),
            Some(execution_id.as_str())
        );
        let snapshots = store::load_v1_snapshots(dir.path(), &execution_id).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].snapshot_manifest.execution_id, execution_id);
        assert!(
            !snapshots[0]
                .artifact_dir
                .join("acceptance-overlay")
                .exists()
        );
        assert!(
            store::load_manifest(dir.path(), "blake3:v1")
                .unwrap()
                .is_none(),
            "v1 artifacts must not overwrite the legacy capsule-manifest keyed store"
        );
    }
}
