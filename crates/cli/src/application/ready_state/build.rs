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
use capsule::foundation::install_lifecycle::RunnerClassFacts;
use capsule::types::CapsuleManifest;
use snapshot::{
    BuildLayers, BuildReadyStateInput, BuildReadyStateReceipt, RestoreContract, SanitizerContract,
    SanitizerLayer, SanitizerStep, SnapshotBackend, ensure_gpu_not_in_snapshot,
};

use super::store;

/// Derive the restore contract (ports / healthcheck / SLO) from the manifest.
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

    let expected_ready_ms = m
        .snapshot_config()
        .max_restore_seconds
        .map(|s| s.saturating_mul(1000));

    RestoreContract {
        expected_ready_ms,
        ports,
        healthcheck,
    }
}

/// Derive the post-resume sanitizer steps. When `sanitize_after_restore` is on
/// (the default), emit the standard ordered step set (plan §8.2); else empty.
pub(crate) fn sanitizer_contract_from_manifest(m: &CapsuleManifest) -> SanitizerContract {
    if !m.snapshot_config().sanitize_after_restore {
        return SanitizerContract::default();
    }
    let steps = vec![
        SanitizerStep { step: "regenerate_ids".into(), layer: SanitizerLayer::GuestAgent },
        SanitizerStep { step: "reseed_entropy".into(), layer: SanitizerLayer::GuestAgent },
        SanitizerStep { step: "refresh_clock".into(), layer: SanitizerLayer::GuestAgent },
        SanitizerStep { step: "reset_sockets".into(), layer: SanitizerLayer::GuestAgent },
        SanitizerStep { step: "reconnect_net".into(), layer: SanitizerLayer::HostAndGuest },
        SanitizerStep { step: "port_remap".into(), layer: SanitizerLayer::Host },
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
) -> Result<BuildReadyStateReceipt> {
    // C guard: never seal an in-VM GPU into the snapshot.
    ensure_gpu_not_in_snapshot(manifest.gpu_mode())
        .context("Ready-State build refused: GPU state is not snapshottable")?;

    let store = store::open_store(state_root, &capsule_manifest_hash)?;
    let runner_class = Some(RunnerClassFacts::from_host().id());

    let receipt = backend
        .build_ready_state(BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash,
            runner_class,
            layers,
            restore_contract: restore_contract_from_manifest(manifest),
            sanitizer_contract: sanitizer_contract_from_manifest(manifest),
            declared_secret_markers: declared_secret_markers(manifest),
        })
        .context("snapshot backend build_ready_state failed")?;

    store::save_manifest(state_root, &receipt.manifest)?;
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
        let c = restore_contract_from_manifest(&parse("[snapshot]\nmode=\"warm\"\nmax_restore_seconds=8\n"));
        assert!(c.ports.contains(&8080));
        assert_eq!(c.expected_ready_ms, Some(8000));
    }

    #[test]
    fn sanitizer_contract_present_by_default_and_empty_when_disabled() {
        assert!(!sanitizer_contract_from_manifest(&parse("[snapshot]\nmode=\"warm\"\n")).steps.is_empty());
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
        let receipt = seal(dir.path(), "blake3:capsule".to_string(), &m, layers, &backend).unwrap();
        assert!(receipt.no_secret_proof.is_clean());
        // The sealed manifest is loadable from disk.
        let loaded = store::load_manifest(dir.path(), "blake3:capsule").unwrap().unwrap();
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
        let err = seal(dir.path(), "blake3:gpu".to_string(), &m, layers, &backend).unwrap_err();
        assert!(format!("{err:#}").contains("GPU"));
    }
}
