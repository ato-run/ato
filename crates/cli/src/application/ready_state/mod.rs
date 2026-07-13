//! Ready-State use-case layer (E7): orchestrates the snapshot + capsulefs crates
//! behind the `ATO_READY_STATE_ENABLED` flag. The build seal branch
//! ([`build::seal`]) and the run restore sub-mode ([`restore::restore_and_expose`]
//! with [`runtime_adapter::RestoredRuntimeHandle`]) are wired from
//! `cli/commands/build.rs` and `application/pipeline/phases/run.rs` respectively;
//! everything here is additive and a legacy run never touches it.
//!
//! The build seal branch is wired from `cli/commands/build.rs`
//! (`seal_ready_state_if_enabled`) and the run restore sub-mode from
//! `application/pipeline/phases/run.rs` (Execute phase), both behind
//! `ATO_READY_STATE_ENABLED`; a legacy run never touches this.

pub(crate) mod ai_grant;
pub(crate) mod backend;
pub(crate) mod binding_grants;
pub(crate) mod binding_host;
pub(crate) mod bindings;
pub(crate) mod build;
pub(crate) mod diagnostics;
pub(crate) mod flags;
pub(crate) mod restore;
pub(crate) mod restore_lease;
pub(crate) mod runtime_adapter;
pub(crate) mod secret_resolver;
pub(crate) mod store;

#[cfg(test)]
mod docker_import_kvm_smoke;
#[cfg(test)]
mod durable_state_kvm_smoke;
#[cfg(test)]
mod kvm_smoke;

use std::path::{Path, PathBuf};

use anyhow::Result;
use capsule::foundation::install_lifecycle::RunnerClassId;
use capsule::types::CapsuleManifest;

/// The Ready-State artifact key for a capsule manifest: `blake3:<hex>` over the
/// JCS-canonical manifest. **Both** the `ato build` seal branch and the `ato run`
/// restore gate compute this the SAME way (this single helper) — if they diverged,
/// the run would silently miss the sealed artifact and fall back to the cold path.
pub(crate) fn capsule_manifest_hash(manifest: &toml::Value) -> Result<String> {
    capsule::foundation::install_lifecycle::canonical_hash(manifest)
}

/// State root holding `ready-state/<hash>/{manifest.json,cas/}` — `~/.ato` (or the
/// workspace tmp fallback). Shared by build (seal) and run (restore).
pub(crate) fn state_root() -> PathBuf {
    capsule::common::paths::ato_path_or_workspace_tmp(".")
}

/// Source `BuildLayers.rootfs` for the seal, per backend (developer-preview):
///
/// - **fake** (KVM-free, never boots): content-address the just-built `.capsule`
///   artifact bytes — ties the sealed Ready-State artifact to the real build
///   output and exercises the full CapsuleFS round-trip without `/dev/kvm`.
/// - **firecracker** (needs a bootable ext4): read an env-supplied image
///   (`ATO_FC_ROOTFS`, falling back to `ATO_FC_TEST_ROOTFS` for parity with the
///   benchmarks/KVM smoke). Automated from-source rootfs construction is a
///   follow-up; a missing image is a **clear error**, never a silent Fake seal.
///
/// `vmstate`/`memory` are left empty: the backend produces them (Fake synthesizes,
/// Firecracker boots+snapshots).
pub(crate) fn assemble_build_layers(
    backend_id: &str,
    artifact: Option<&Path>,
) -> Result<snapshot::BuildLayers> {
    let rootfs = if backend_id == "fake" {
        let path = artifact.ok_or_else(|| {
            anyhow::anyhow!("Ready-State seal: the build produced no artifact to seal")
        })?;
        std::fs::read(path).map_err(|e| {
            anyhow::anyhow!(
                "Ready-State seal: read build artifact {}: {e}",
                path.display()
            )
        })?
    } else {
        // firecracker (qemu/kata never reach here — select_backend fails closed).
        let path = std::env::var("ATO_FC_ROOTFS")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                std::env::var("ATO_FC_TEST_ROOTFS")
                    .ok()
                    .filter(|v| !v.is_empty())
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Ready-State Firecracker build requires a bootable ext4 via ATO_FC_ROOTFS \
                     (developer-preview); set it to a prebuilt rootfs image. Automated \
                     from-source rootfs build is a follow-up."
                )
            })?;
        std::fs::read(&path)
            .map_err(|e| anyhow::anyhow!("Ready-State seal: read ATO_FC_ROOTFS {path}: {e}"))?
    };
    Ok(snapshot::BuildLayers {
        rootfs,
        runtime: None,
        dependency: None,
        app: None,
        vmstate: Vec::new(),
        memory: Vec::new(),
    })
}

/// The Execute-phase sub-mode carrier: everything the run pipeline needs to
/// restore instead of cold-spawn. Present only when the flag is on, the capsule
/// is Ready-State-eligible, and a sealed artifact exists — otherwise the run
/// falls through to the legacy cold path.
#[derive(Debug)]
pub(crate) struct ReadyStateRunPlan {
    /// The sealed artifact to restore.
    pub(crate) manifest: snapshot::ReadyStateManifest,
    /// State root holding the `ready-state/<hash>/cas` store.
    pub(crate) state_root: PathBuf,
    /// `blake3:<hex>` of the capsule manifest (the artifact key).
    pub(crate) capsule_manifest_hash: String,
    /// Whether to run the post-resume sanitizer before exposing. (The
    /// developer-preview run gate does not yet apply host-side sanitizer steps —
    /// a fast follow; for the Fake backend they are no-ops.)
    #[allow(dead_code)]
    pub(crate) sanitize_after_restore: bool,
    /// This host's runner class (fed to the fail-closed restore gate).
    pub(crate) host_runner_class: Option<RunnerClassId>,
}

/// Decide whether this run is a Ready-State restore.
///
/// - flag **off** → `None` (legacy cold path, unchanged).
/// - flag **on**, capsule **not** Ready-State-eligible → `None` (legacy).
/// - flag **on**, eligible, **no sealed artifact** → **`Err`** (fail closed): the
///   user explicitly enabled Ready-State as a validation mode, so a missing
///   artifact must NOT silently degrade to a cold run.
/// - flag **on**, eligible, artifact exists → `Some(plan)` (restore).
pub(crate) fn decide_ready_state_run(
    manifest: &CapsuleManifest,
    capsule_manifest_hash: &str,
    state_root: &Path,
) -> anyhow::Result<Option<ReadyStateRunPlan>> {
    if !flags::ready_state_enabled() {
        return Ok(None);
    }
    if !manifest.is_ready_state_eligible() {
        return Ok(None);
    }
    let Some(sealed) = store::load_manifest(state_root, capsule_manifest_hash)? else {
        anyhow::bail!(
            "Ready-State artifact not found for {capsule_manifest_hash}. Run `ato build` with \
             ATO_READY_STATE_ENABLED=1 first, or unset ATO_READY_STATE_ENABLED to use the legacy \
             cold path."
        );
    };
    Ok(Some(ReadyStateRunPlan {
        manifest: sealed,
        state_root: state_root.to_path_buf(),
        capsule_manifest_hash: capsule_manifest_hash.to_string(),
        sanitize_after_restore: manifest.snapshot_config().sanitize_after_restore,
        // `None` delegates host-class resolution to the backend at restore
        // time (Firecracker recomputes its real facts; same contract as
        // `runner serve`). See `build::seal` for the build-side counterpart.
        host_runner_class: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eligible_manifest() -> CapsuleManifest {
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

    fn non_eligible_manifest() -> CapsuleManifest {
        // No `[snapshot]` section ⇒ not Ready-State-eligible.
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
"#,
        )
        .unwrap()
    }

    /// Seal a Fake artifact so `decide` can find it for the "exists" case.
    fn seal_fake_artifact(root: &Path, hash: &str) {
        let backend = snapshot::FakeSnapshotBackend::new();
        let layers = snapshot::BuildLayers {
            rootfs: b"rootfs-bytes".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"app-bytes".to_vec()),
            vmstate: vec![1u8; 128],
            memory: (0..2000u32).map(|i| (i % 256) as u8).collect(),
        };
        build::seal(
            root,
            hash.to_string(),
            &eligible_manifest(),
            layers,
            &backend,
        )
        .unwrap();
    }

    // `ATO_READY_STATE_ENABLED` is process-global, so ALL flag-dependent `decide`
    // cases live in this one serial test (never split across parallel tests).
    #[test]
    fn decide_ready_state_run_matrix() {
        let prev = std::env::var("ATO_READY_STATE_ENABLED").ok();
        let set = |v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var("ATO_READY_STATE_ENABLED", v),
                None => std::env::remove_var("ATO_READY_STATE_ENABLED"),
            }
        };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // flag OFF → None (legacy), even for an eligible capsule with no artifact.
        set(None);
        assert!(
            decide_ready_state_run(&eligible_manifest(), "blake3:x", root)
                .unwrap()
                .is_none()
        );

        // flag ON, NOT eligible → None (legacy).
        set(Some("1"));
        assert!(
            decide_ready_state_run(&non_eligible_manifest(), "blake3:x", root)
                .unwrap()
                .is_none()
        );

        // flag ON, eligible, NO artifact → Err (fail closed, no silent cold run).
        let err = decide_ready_state_run(&eligible_manifest(), "blake3:missing", root).unwrap_err();
        assert!(
            err.to_string().contains("Ready-State artifact not found"),
            "{err}"
        );

        // flag ON, eligible, artifact EXISTS → Some(plan).
        let hash = "blake3:present";
        seal_fake_artifact(root, hash);
        assert!(
            decide_ready_state_run(&eligible_manifest(), hash, root)
                .unwrap()
                .is_some()
        );

        set(prev.as_deref());
    }

    #[test]
    fn capsule_manifest_hash_is_deterministic_and_prefixed() {
        let m: toml::Value = toml::from_str("name='x'\nversion='1.0.0'").unwrap();
        let h1 = capsule_manifest_hash(&m).unwrap();
        assert_eq!(
            h1,
            capsule_manifest_hash(&m).unwrap(),
            "build/run must agree on the key"
        );
        assert!(h1.starts_with("blake3:"), "{h1}");
    }

    #[test]
    fn assemble_build_layers_fake_seals_the_built_artifact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let art = dir.path().join("app.capsule");
        std::fs::write(&art, b"capsule-archive-bytes").unwrap();
        let layers = assemble_build_layers("fake", Some(&art)).unwrap();
        assert_eq!(layers.rootfs, b"capsule-archive-bytes");
        assert!(layers.vmstate.is_empty() && layers.memory.is_empty());
    }

    #[test]
    fn assemble_build_layers_fake_errors_without_artifact() {
        assert!(assemble_build_layers("fake", None).is_err());
    }

    #[test]
    fn assemble_build_layers_firecracker_requires_rootfs_env_no_silent_fake() {
        // SAFETY: single-threaded test body; vars restored at the end.
        let prev = (
            std::env::var("ATO_FC_ROOTFS").ok(),
            std::env::var("ATO_FC_TEST_ROOTFS").ok(),
        );
        unsafe {
            std::env::remove_var("ATO_FC_ROOTFS");
            std::env::remove_var("ATO_FC_TEST_ROOTFS");
        }
        let err = assemble_build_layers("firecracker", Some(Path::new("/ignored")))
            .expect_err("firecracker without a rootfs env must fail closed");
        assert!(err.to_string().contains("ATO_FC_ROOTFS"), "{err}");
        unsafe {
            match prev.0 {
                Some(v) => std::env::set_var("ATO_FC_ROOTFS", v),
                None => std::env::remove_var("ATO_FC_ROOTFS"),
            }
            match prev.1 {
                Some(v) => std::env::set_var("ATO_FC_TEST_ROOTFS", v),
                None => std::env::remove_var("ATO_FC_TEST_ROOTFS"),
            }
        }
    }
}
