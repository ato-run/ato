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

/// v1.2 PR 3e: lease kind for restoring a SUPERVISOR (binding-required) snapshot.
/// A separate kind — not an additive field — so the control plane capability-gates
/// dispatch on `supported_lease_kinds` and an older runner is NEVER handed a
/// binding artifact it cannot serve. Payload shape is identical to
/// `restore_snapshot`; the binding names come from the sealed manifest
/// (`supervisor_build.binding_names`), the single source of truth — a lease field
/// would not be trusted.
pub(crate) const RESTORE_SNAPSHOT_WITH_BINDINGS_LEASE_KIND: &str = "restore_snapshot_with_bindings";

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
    /// v1.2 PR 3e: true iff the lease kind is `restore_snapshot_with_bindings`.
    /// The kind PROMISES a supervisor artifact; `classify_restore_artifact`
    /// fail-closes any kind↔artifact mismatch in either direction.
    pub with_bindings: bool,
}

/// Parse + validate a `restore_snapshot` / `restore_snapshot_with_bindings` lease
/// command. Every identity field is required and non-empty (a lease missing one is
/// refused, never restored blind).
pub(crate) fn parse_restore_snapshot_command(
    command: &serde_json::Value,
) -> std::result::Result<RestoreSnapshotCommand, (String, String)> {
    let err = |m: &str| ("invalid_restore_lease".to_string(), m.to_string());
    let kind = command.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let with_bindings = match kind {
        RESTORE_SNAPSHOT_LEASE_KIND => false,
        RESTORE_SNAPSHOT_WITH_BINDINGS_LEASE_KIND => true,
        _ => return Err(err(&format!("not a restore_snapshot lease (kind {kind:?})"))),
    };
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
        with_bindings,
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

/// v1.2 PR 3e: what kind of restore this artifact is, decided fail-closed by
/// [`classify_restore_artifact`]. `Supervisor` carries the binding names read from
/// the sealed manifest — the ONLY source of truth (a lease field is never trusted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestoreArtifactClass {
    /// The v1 no-binding artifact: restore + expose directly.
    NoBinding,
    /// A supervisor (binding-required) artifact: the runner must resolve + deliver
    /// every named binding over vsock BEFORE exposing traffic. 3e MVP: ALL names
    /// are required — optional-secret semantics do not exist in this path yet.
    Supervisor { binding_names: Vec<String> },
}

/// v1.2 PR 3e: the NARROW supervisor exception to the "no vsock artifact" rule.
/// A supervisor restore is allowed ONLY when every one of these holds — anything
/// else fails closed:
/// - the lease kind is `restore_snapshot_with_bindings` (the capability-gated kind);
/// - this runner is opted in (`ATO_RUNNER_SUPERVISOR=1`);
/// - `manifest.has_vsock == true` AND `manifest.supervisor_build` is present
///   (either without the other = an inconsistent artifact, rejected);
/// - `binding_names` is non-empty and every name parses as a `BindingName`.
/// The plain `restore_snapshot` kind still restores ONLY a no-binding artifact
/// (`!has_vsock`, no supervisor receipt) — the old rejection is unchanged for it.
/// (The backend binding capability is re-checked in the handler, where a backend
/// exists to probe.)
pub(crate) fn classify_restore_artifact(
    manifest: &ReadyStateManifest,
    with_bindings_kind: bool,
    supervisor_enabled: bool,
) -> std::result::Result<RestoreArtifactClass, (String, String)> {
    let err = |m: String| ("artifact_verification_failed".to_string(), m);
    match (&manifest.supervisor_build, manifest.has_vsock) {
        (None, false) => {
            if with_bindings_kind {
                return Err(err(
                    "restore_snapshot_with_bindings lease references a no-binding artifact \
                     (kind/artifact mismatch)"
                        .to_string(),
                ));
            }
            Ok(RestoreArtifactClass::NoBinding)
        }
        (None, true) => Err(err(
            "artifact declares a vsock binding channel but carries no supervisor_build \
             receipt; refusing to restore an inconsistent artifact"
                .to_string(),
        )),
        (Some(_), false) => Err(err(
            "artifact carries a supervisor_build receipt but has_vsock=false; refusing to \
             restore an inconsistent artifact"
                .to_string(),
        )),
        (Some(sup), true) => {
            if !with_bindings_kind {
                return Err(err(
                    "supervisor (binding-required) artifact needs a restore_snapshot_with_bindings \
                     lease; a plain restore_snapshot lease cannot launch it"
                        .to_string(),
                ));
            }
            if !supervisor_enabled {
                return Err(err(
                    "supervisor artifact refused: this runner is not opted into supervisor \
                     restores (set ATO_RUNNER_SUPERVISOR=1)"
                        .to_string(),
                ));
            }
            if sup.binding_names.is_empty() {
                return Err(err(
                    "supervisor artifact carries an empty binding_names list; refusing to \
                     restore (nothing to bind = inconsistent artifact)"
                        .to_string(),
                ));
            }
            for name in &sup.binding_names {
                if let Err(e) = protocol::binding_lease::BindingName::parse(name.as_str()) {
                    return Err(err(format!("supervisor artifact binding name {name:?}: {e}")));
                }
            }
            Ok(RestoreArtifactClass::Supervisor { binding_names: sup.binding_names.clone() })
        }
    }
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
/// - the artifact class is admissible for THIS lease kind + runner
///   ([`classify_restore_artifact`]: no-binding for `restore_snapshot`, the narrow
///   supervisor exception for `restore_snapshot_with_bindings`).
pub(crate) fn load_and_verify_manifest(
    manifest_json: &Path,
    cmd: &RestoreSnapshotCommand,
    supervisor_enabled: bool,
) -> std::result::Result<(ReadyStateManifest, RestoreArtifactClass), (String, String)> {
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
    let class = classify_restore_artifact(&manifest, cmd.with_bindings, supervisor_enabled)?;
    Ok((manifest, class))
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
        assert!(!c.with_bindings);
        // v1.2 PR 3e: the with-bindings kind parses identically, flagged.
        let c = parse_restore_snapshot_command(&cmd_json(serde_json::json!({ "kind": "restore_snapshot_with_bindings" }))).unwrap();
        assert!(c.with_bindings);
    }

    // ── v1.2 PR 3e: the narrow supervisor gate matrix ─────────────────────────

    fn manifest_with(supervisor: Option<Vec<&str>>, has_vsock: bool) -> ReadyStateManifest {
        // Minimal structurally-valid manifest — classify only reads has_vsock +
        // supervisor_build, but build it via serde to stay honest to the schema.
        let mut v = serde_json::json!({
            "schema": "ato.ready-state/v1",
            "capsule_manifest_hash": "blake3:cap",
            "has_vsock": has_vsock,
            "layers": {},
            "snapshot_backend": { "kind": "firecracker", "version": "1", "snapshot_format_version": "fc-v2" },
            "restore_contract": {},
            "sanitizer_contract": { "steps": [] },
        });
        if let Some(names) = supervisor {
            v["supervisor_build"] = serde_json::json!({
                "binding_names": names,
                "page_hygiene_boot_args": true,
            });
        }
        serde_json::from_value(v).expect("manifest")
    }

    #[test]
    fn old_kind_cannot_launch_a_supervisor_artifact() {
        let m = manifest_with(Some(vec!["openai_api_key"]), true);
        // Even with the flag ON: the plain kind must never launch a supervisor artifact.
        let e = classify_restore_artifact(&m, false, true).unwrap_err();
        assert!(e.1.contains("restore_snapshot_with_bindings"), "{}", e.1);
    }

    #[test]
    fn with_bindings_kind_rejects_a_no_binding_artifact() {
        let m = manifest_with(None, false);
        let e = classify_restore_artifact(&m, true, true).unwrap_err();
        assert!(e.1.contains("kind/artifact mismatch"), "{}", e.1);
        // The plain kind still restores it.
        assert_eq!(classify_restore_artifact(&m, false, true).unwrap(), RestoreArtifactClass::NoBinding);
        assert_eq!(classify_restore_artifact(&m, false, false).unwrap(), RestoreArtifactClass::NoBinding);
    }

    #[test]
    fn supervisor_artifact_with_flag_off_is_rejected() {
        let m = manifest_with(Some(vec!["openai_api_key"]), true);
        let e = classify_restore_artifact(&m, true, false).unwrap_err();
        assert!(e.1.contains("ATO_RUNNER_SUPERVISOR"), "{}", e.1);
    }

    #[test]
    fn inconsistent_vsock_supervisor_combinations_are_rejected_both_ways() {
        // has_vsock without a supervisor receipt: the ORIGINAL rejection, kept.
        let m = manifest_with(None, true);
        assert!(classify_restore_artifact(&m, false, true).unwrap_err().1.contains("no supervisor_build"));
        assert!(classify_restore_artifact(&m, true, true).unwrap_err().1.contains("no supervisor_build"));
        // supervisor receipt without vsock: also inconsistent.
        let m = manifest_with(Some(vec!["openai_api_key"]), false);
        assert!(classify_restore_artifact(&m, true, true).unwrap_err().1.contains("has_vsock=false"));
    }

    #[test]
    fn supervisor_binding_names_must_be_non_empty_and_valid() {
        let m = manifest_with(Some(vec![]), true);
        assert!(classify_restore_artifact(&m, true, true).unwrap_err().1.contains("empty binding_names"));
        // An uppercase (invalid BindingName) name fails closed.
        let m = manifest_with(Some(vec!["OPENAI_API_KEY"]), true);
        assert!(classify_restore_artifact(&m, true, true).unwrap_err().1.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn supervisor_artifact_with_every_prerequisite_classifies_with_manifest_names() {
        let m = manifest_with(Some(vec!["openai_api_key", "db_url"]), true);
        let class = classify_restore_artifact(&m, true, true).unwrap();
        assert_eq!(
            class,
            RestoreArtifactClass::Supervisor {
                binding_names: vec!["openai_api_key".into(), "db_url".into()]
            }
        );
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
            with_bindings: false,
        };
        // Exact match ⇒ ok, classified NoBinding.
        let (_, class) = load_and_verify_manifest(&mpath, &base, false).unwrap();
        assert_eq!(class, RestoreArtifactClass::NoBinding);
        // Tampered artifact hash ⇒ fail (the integrity anchor restore() lacks).
        let mut bad = base.clone();
        bad.artifact_manifest_hash = "blake3:TAMPERED".into();
        assert!(load_and_verify_manifest(&mpath, &bad, false).unwrap_err().1.contains("artifact_manifest_hash mismatch"));
        // Wrong execution_id / capsule hash / runner class / backend ⇒ fail.
        for mutate in [
            |c: &mut RestoreSnapshotCommand| c.execution_id = "sha256:other".into(),
            |c: &mut RestoreSnapshotCommand| c.capsule_manifest_hash = "blake3:other".into(),
            |c: &mut RestoreSnapshotCommand| c.runner_class_id = "blake3:other".into(),
            |c: &mut RestoreSnapshotCommand| c.snapshot_backend = "qemu".into(),
        ] {
            let mut c = base.clone();
            mutate(&mut c);
            assert!(load_and_verify_manifest(&mpath, &c, false).is_err());
        }
    }
}
