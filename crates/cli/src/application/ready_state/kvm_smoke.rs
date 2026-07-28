//! KVM-gated smoke test for the Ready-State ENGINE wired to the real
//! `FirecrackerBackend` (#830 task 10): a minimal build(seal) → restore → stop
//! flow driven through [`backend::select_backend`] + [`build::seal`] +
//! [`restore::restore_and_expose`] / [`restore::teardown`] — i.e. through the
//! E/F engine, not the backend's own unit tests.
//!
//! `#[ignore]`d and self-skips unless `/dev/kvm` + `ATO_FC_TEST_ROOTFS` are
//! present, so a normal `cargo test` never runs it. On a KVM host:
//!
//! ```sh
//! sudo -E env ATO_SNAPSHOT_BACKEND=firecracker \
//!   ATO_FC_BIN=$PWD/firecracker ATO_FC_KERNEL=$PWD/vmlinux \
//!   ATO_FC_TEST_ROOTFS=$PWD/rootfs.ext4 ATO_FC_ROOTFS_READONLY=0 \
//!   cargo test -p cli -- --ignored --test-threads=1 --nocapture fc_engine_smoke
//! ```

use capsule::types::CapsuleManifest;
use snapshot::{BuildLayers, FirecrackerBackend};

use super::{backend, build, restore, store};

fn smoke_manifest() -> CapsuleManifest {
    CapsuleManifest::from_toml(
        r#"
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

[snapshot]
mode = "warm"
"#,
    )
    .unwrap()
}

#[test]
#[ignore]
fn fc_engine_smoke_build_restore_stop() {
    if !FirecrackerBackend::kvm_present() {
        eprintln!("SKIP: /dev/kvm absent");
        return;
    }
    // Accept ATO_FC_ROOTFS (the CLI developer-preview var) or ATO_FC_TEST_ROOTFS.
    let rootfs_path = std::env::var("ATO_FC_ROOTFS")
        .ok()
        .filter(|p| !p.is_empty())
        .or_else(|| {
            std::env::var("ATO_FC_TEST_ROOTFS")
                .ok()
                .filter(|p| !p.is_empty())
        });
    let rootfs = match rootfs_path {
        Some(p) => std::fs::read(&p).expect("read ATO_FC_ROOTFS / ATO_FC_TEST_ROOTFS"),
        None => {
            eprintln!("SKIP: ATO_FC_ROOTFS / ATO_FC_TEST_ROOTFS unset");
            return;
        }
    };

    // Selection goes through the flag (exercises the fail-closed path too).
    // SAFETY: single-threaded (`--test-threads=1` for the KVM suite).
    unsafe { std::env::set_var("ATO_SNAPSHOT_BACKEND", "firecracker") };
    let be = backend::select_backend().expect("firecracker must select on a KVM host");
    assert_eq!(
        be.id(),
        "firecracker",
        "engine must use the real backend, not a fallback"
    );

    let dir = tempfile::tempdir().unwrap();
    let state_root = dir.path();
    let hash = "blake3:fc-engine-smoke".to_string();
    let manifest = smoke_manifest();
    let layers = BuildLayers {
        rootfs,
        runtime: None,
        dependency: None,
        app: None,
        vmstate: Vec::new(), // produced by the boot→snapshot, not supplied
        memory: Vec::new(),
    };

    // BUILD (Boot/Snapshot/Seal) through the engine.
    let receipt = build::seal(
        state_root,
        hash.clone(),
        &manifest,
        layers,
        be.as_ref(),
        None,
    )
    .expect("engine seal failed");
    assert!(
        receipt.no_secret_proof.is_clean(),
        "no-secret gate must pass on a clean rootfs"
    );
    assert!(
        receipt.manifest.layers.memory.is_some(),
        "memory layer sealed"
    );
    assert!(
        receipt.manifest.runner_class_id.is_some(),
        "runner class pinned"
    );

    // RESTORE (Restore/Bind/Expose) through the engine, from the persisted seal.
    let cas = store::open_store(state_root, &hash).unwrap();
    let sealed = store::load_manifest(state_root, &hash)
        .unwrap()
        .expect("sealed manifest present");
    let overlay = dir.path().join("ov");
    let restored = restore::restore_and_expose(
        be.as_ref(),
        &cas,
        sealed,
        restore::RestoreVerification::LegacyLocal,
        overlay.clone(),
        None,
        false,
    )
    .expect("engine restore failed");
    assert_eq!(
        restored.session.guest_port,
        Some(8080),
        "restored session exposes the app port"
    );
    assert!(restored.session.restored_bytes > 0);

    // STOP (Teardown) through the engine: VM gone, disposable overlay destroyed.
    let td = restore::teardown(be.as_ref(), restored.session).expect("engine teardown failed");
    assert!(td.overlay_removed);
    assert!(!overlay.exists(), "disposable overlay not removed");
}
