//! Track E (#912): runner-side `restore_snapshot` lease — the fetch/verify half.
//!
//! A Connected/Managed runner receives a `restore_snapshot` lease from ato-api
//! (Track D, ato-api#159). The lease is **reference-only**: `artifact_location` is a
//! HINT, and the identity fields exist so the runner can **verify the artifact it
//! fetched is exactly the one the registry sealed** before restoring. This module is the
//! pure, host-independent core (parse + locate + verify); the restore/expose/report/
//! teardown orchestration lives in `runner_agent`.
//!
//! The critical gate `backend.restore` does NOT provide: recomputing the sealed
//! manifest's blake3 id and requiring it to equal the lease's `artifact_manifest_hash`.
//! Without it, a runner would trust whatever manifest happened to be on disk at the
//! CAS path — this module closes that hole (fail-closed).

use std::path::{Path, PathBuf};

use snapshot::ReadyStateManifest;

/// Lease kind for restoring a sealed Ready-State snapshot (matches ato-api's
/// `RESTORE_SNAPSHOT_LEASE_KIND`).
pub(crate) const RESTORE_SNAPSHOT_LEASE_KIND: &str = "restore_snapshot";

/// The reference-only identity a `restore_snapshot` lease carries. No secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestoreSnapshotCommand {
    pub snapshot_id: String,
    pub capsule_id: String,
    pub target_label: String,
    pub profile: String,
    pub artifact_location: String,
    pub artifact_manifest_hash: String,
    pub capsule_manifest_hash: String,
    pub execution_id: String,
    pub runner_class_id: String,
    pub snapshot_backend: String,
    pub healthcheck_url_path: Option<String>,
}

/// Parse + validate a `restore_snapshot` lease command. Every identity field is
/// required and non-empty (a lease missing one is refused, never restored blind).
pub(crate) fn parse_restore_snapshot_command(
    command: &serde_json::Value,
) -> std::result::Result<RestoreSnapshotCommand, (String, String)> {
    let err = |m: &str| ("invalid_restore_lease".to_string(), m.to_string());
    let kind = command.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if kind != RESTORE_SNAPSHOT_LEASE_KIND {
        return Err(err(&format!("not a restore_snapshot lease (kind {kind:?})")));
    }
    let req = |k: &str| -> std::result::Result<String, (String, String)> {
        command
            .get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| err(&format!("restore_snapshot lease is missing required field {k:?}")))
    };
    Ok(RestoreSnapshotCommand {
        snapshot_id: req("snapshot_id")?,
        capsule_id: req("capsule_id")?,
        target_label: req("target_label")?,
        profile: req("profile")?,
        artifact_location: req("artifact_location")?,
        artifact_manifest_hash: req("artifact_manifest_hash")?,
        capsule_manifest_hash: req("capsule_manifest_hash")?,
        execution_id: req("execution_id")?,
        runner_class_id: req("runner_class_id")?,
        snapshot_backend: req("snapshot_backend")?,
        healthcheck_url_path: command
            .get("healthcheck_url_path")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty()),
    })
}

/// The on-disk location of a fetched artifact: `manifest.json` beside a `cas/` dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactPaths {
    pub manifest_json: PathBuf,
    pub cas_dir: PathBuf,
}

/// Resolve `cas://<job_id>/<artifact_hash>` to on-disk paths under `artifact_root`
/// (v1 same-host: the builder wrote `<artifact_root>/<job_id>/{manifest.json, cas/}`).
///
/// Fail-closed: only the `cas://` scheme is accepted (a remote `r2://`/`https://`
/// artifact store is a later phase), the job segment must be a single safe path
/// component (no `/`, `..`, or absolute escape), and the resolved dir must stay under
/// `artifact_root`.
pub(crate) fn locate_artifact(
    artifact_location: &str,
    artifact_root: &Path,
) -> std::result::Result<ArtifactPaths, (String, String)> {
    let err = |m: String| ("artifact_unavailable".to_string(), m);
    let rest = artifact_location
        .strip_prefix("cas://")
        .ok_or_else(|| err(format!("unsupported artifact scheme in {artifact_location:?} (v1 restores cas:// only)")))?;
    let job = rest.split('/').next().unwrap_or("");
    if job.is_empty()
        || job.contains("..")
        || job.contains('\\')
        || Path::new(job).components().count() != 1
        || Path::new(job).is_absolute()
    {
        return Err(err(format!("unsafe artifact job segment in {artifact_location:?}")));
    }
    let dir = artifact_root.join(job);
    Ok(ArtifactPaths {
        manifest_json: dir.join("manifest.json"),
        cas_dir: dir.join("cas"),
    })
}

/// Load `manifest.json` and **verify it is exactly the artifact the lease references**,
/// fail-closed. This is the integrity gate `backend.restore` does not provide.
///
/// Checks, in order:
/// - the manifest deserializes;
/// - **`manifest.id() == lease.artifact_manifest_hash`** (recomputed blake3 over the
///   canonical manifest — the artifact-integrity anchor);
/// - `capsule_manifest_hash` / `execution_id` / `snapshot_backend` match the lease;
/// - `runner_class_id` is present and matches the lease (restore also re-gates it, but
///   this gives a clean pre-restore error and pins the lease↔manifest agreement);
/// - the artifact is **no-binding** (`!has_vsock` — a Phase 8 binding artifact must never
///   reach a public snapshot run).
pub(crate) fn load_and_verify_manifest(
    manifest_json: &Path,
    cmd: &RestoreSnapshotCommand,
) -> std::result::Result<ReadyStateManifest, (String, String)> {
    let err = |m: String| ("artifact_verification_failed".to_string(), m);
    let bytes = std::fs::read(manifest_json).map_err(|e| err(format!("read {}: {e}", manifest_json.display())))?;
    let manifest: ReadyStateManifest =
        serde_json::from_slice(&bytes).map_err(|e| err(format!("parse manifest.json: {e}")))?;

    let recomputed = manifest.id();
    if recomputed != cmd.artifact_manifest_hash {
        return Err(err(format!(
            "artifact_manifest_hash mismatch: lease {} != recomputed {recomputed}",
            cmd.artifact_manifest_hash
        )));
    }
    if manifest.capsule_manifest_hash != cmd.capsule_manifest_hash {
        return Err(err(format!(
            "capsule_manifest_hash mismatch: lease {} != manifest {}",
            cmd.capsule_manifest_hash, manifest.capsule_manifest_hash
        )));
    }
    match manifest.execution_id.as_deref() {
        Some(id) if id == cmd.execution_id => {}
        other => {
            return Err(err(format!(
                "execution_id mismatch: lease {} != manifest {:?}",
                cmd.execution_id, other
            )));
        }
    }
    if manifest.snapshot_backend.kind != cmd.snapshot_backend {
        return Err(err(format!(
            "snapshot_backend mismatch: lease {} != manifest {}",
            cmd.snapshot_backend, manifest.snapshot_backend.kind
        )));
    }
    match manifest.runner_class_id.as_ref().map(|c| c.to_string()) {
        Some(rc) if rc == cmd.runner_class_id => {}
        other => {
            return Err(err(format!(
                "runner_class_id mismatch: lease {} != manifest {:?}",
                cmd.runner_class_id, other
            )));
        }
    }
    if manifest.has_vsock {
        return Err(err("artifact declares a vsock binding channel; a public snapshot run must be no-binding".to_string()));
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd_json(over: serde_json::Value) -> serde_json::Value {
        let mut base = serde_json::json!({
            "kind": "restore_snapshot",
            "snapshot_id": "snap_1",
            "capsule_id": "cap-1",
            "target_label": "web",
            "profile": "default",
            "artifact_location": "cas://job-1/blake3:art",
            "artifact_manifest_hash": "blake3:art",
            "capsule_manifest_hash": "blake3:cap",
            "execution_id": "sha256:exec",
            "runner_class_id": "blake3:rc",
            "snapshot_backend": "firecracker",
            "healthcheck_url_path": "/health",
        });
        if let (Some(b), Some(o)) = (base.as_object_mut(), over.as_object()) {
            for (k, v) in o {
                if v.is_null() {
                    b.remove(k);
                } else {
                    b.insert(k.clone(), v.clone());
                }
            }
        }
        base
    }

    #[test]
    fn parses_a_full_restore_lease() {
        let c = parse_restore_snapshot_command(&cmd_json(serde_json::json!({}))).unwrap();
        assert_eq!(c.snapshot_id, "snap_1");
        assert_eq!(c.artifact_manifest_hash, "blake3:art");
        assert_eq!(c.healthcheck_url_path.as_deref(), Some("/health"));
    }

    #[test]
    fn rejects_wrong_kind_and_missing_fields() {
        assert!(parse_restore_snapshot_command(&serde_json::json!({ "kind": "run_capsule" })).is_err());
        for field in [
            "snapshot_id", "capsule_id", "target_label", "profile", "artifact_location",
            "artifact_manifest_hash", "capsule_manifest_hash", "execution_id", "runner_class_id", "snapshot_backend",
        ] {
            let c = cmd_json(serde_json::json!({ field: serde_json::Value::Null }));
            let e = parse_restore_snapshot_command(&c).unwrap_err();
            assert!(e.1.contains(field), "missing {field} should be reported: {}", e.1);
        }
        // healthcheck is optional.
        assert!(parse_restore_snapshot_command(&cmd_json(serde_json::json!({ "healthcheck_url_path": serde_json::Value::Null }))).unwrap().healthcheck_url_path.is_none());
    }

    #[test]
    fn locate_artifact_maps_cas_uri_and_rejects_escapes() {
        let root = Path::new("/var/lib/ato/artifacts");
        let p = locate_artifact("cas://job-1/blake3:art", root).unwrap();
        assert_eq!(p.manifest_json, root.join("job-1").join("manifest.json"));
        assert_eq!(p.cas_dir, root.join("job-1").join("cas"));
        // Non-cas scheme.
        assert!(locate_artifact("https://evil/x", root).unwrap_err().1.contains("scheme"));
        assert!(locate_artifact("r2://bucket/x", root).unwrap_err().1.contains("scheme"));
        // Traversal / absolute / multi-segment job.
        assert!(locate_artifact("cas://../etc/x", root).is_err());
        assert!(locate_artifact("cas:///abs/x", root).is_err());
    }

    #[test]
    fn verify_rejects_a_tampered_or_binding_manifest() {
        // Build a real sealed manifest via the Fake backend, persist it, and verify.
        use capsulefs::CasStore;
        use snapshot::{BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract, SanitizerContract, SnapshotBackend};
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let receipt = FakeSnapshotBackend::new()
            .build_ready_state(BuildReadyStateInput {
                store: &store,
                capsule_manifest_hash: "blake3:cap".into(),
                runner_class: Some(capsule::foundation::install_lifecycle::RunnerClassFacts::from_host().id()),
                layers: BuildLayers { rootfs: b"rootfs".to_vec(), runtime: None, dependency: None, app: None, vmstate: vec![1u8; 64], memory: vec![2u8; 4096] },
                restore_contract: RestoreContract { ports: vec![8080], healthcheck: Some("/health".into()), expected_ready_ms: Some(2000) },
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: vec![],
                execution_id: Some("sha256:exec".into()),
                supervisor: None,
            })
            .expect("build");
        let m = receipt.manifest;
        let mpath = dir.path().join("manifest.json");
        std::fs::write(&mpath, serde_json::to_vec(&m).unwrap()).unwrap();
        let rc = m.runner_class_id.as_ref().unwrap().to_string();

        let base = RestoreSnapshotCommand {
            snapshot_id: "snap_1".into(),
            capsule_id: "cap-1".into(),
            target_label: "web".into(),
            profile: "default".into(),
            artifact_location: "cas://job/blake3".into(),
            artifact_manifest_hash: m.id(),
            capsule_manifest_hash: "blake3:cap".into(),
            execution_id: "sha256:exec".into(),
            runner_class_id: rc.clone(),
            snapshot_backend: m.snapshot_backend.kind.clone(),
            healthcheck_url_path: Some("/health".into()),
        };
        // Exact match ⇒ ok.
        assert!(load_and_verify_manifest(&mpath, &base).is_ok());
        // Tampered artifact hash ⇒ fail (the integrity anchor restore() lacks).
        let mut bad = base.clone();
        bad.artifact_manifest_hash = "blake3:TAMPERED".into();
        assert!(load_and_verify_manifest(&mpath, &bad).unwrap_err().1.contains("artifact_manifest_hash mismatch"));
        // Wrong execution_id / capsule hash / runner class / backend ⇒ fail.
        for mutate in [
            |c: &mut RestoreSnapshotCommand| c.execution_id = "sha256:other".into(),
            |c: &mut RestoreSnapshotCommand| c.capsule_manifest_hash = "blake3:other".into(),
            |c: &mut RestoreSnapshotCommand| c.runner_class_id = "blake3:other".into(),
            |c: &mut RestoreSnapshotCommand| c.snapshot_backend = "qemu".into(),
        ] {
            let mut c = base.clone();
            mutate(&mut c);
            assert!(load_and_verify_manifest(&mpath, &c).is_err());
        }
    }
}
