//! Track C PR 2b (#912): the **snapshot builder daemon**.
//!
//! Ties the rootfs builder (#925) to the ato-api claim loop (#156/#157). It claims a
//! queued capsule-snapshot job, materializes the **server-resolved** source (never a
//! client `source_ref`), builds a bootable rootfs, runs the Ready-State build (boot →
//! verify healthcheck → snapshot → seal), verifies the sealed artifact restores, runs the
//! reusable L4 no-secret scan, collects non-secret artifact metadata, and acks the job
//! (`sealed` on success, `failed` with a structured stage/reason otherwise).
//!
//! Hard constraints (v1): NO `capsule_snapshots` write (PR 3 does that from the sealed
//! ack); never trust the job's `source_ref` / any client source; no binding-required
//! capsules (the spec derivation fails those closed); no Phase 8 BindingLease path; UFFD
//! is not enabled; no traffic is ever exposed — the daemon only builds + seals + verifies.
//!
//! ```sh
//! ATO_API_URL=https://api… SNAPSHOT_BUILDER_AGENT_TOKEN=… ATO_FC_BIN=… ATO_FC_KERNEL=… \
//!   snapshot-builder --agent-id builder-1 [--once]
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use capsule::engine::execution_graph::{
    ReadyStateDeclaredEnvelope, declared_dependencies_from_manifest_toml, store_source_identifier,
};
use capsule::foundation::types::manifest::CapsuleManifest;
use capsulefs::CasStore;
use serde::{Deserialize, Serialize};
use snapshot::rootfs_builder::{SourceProbe, build_rootfs, derive_build_spec, materialize_source};
use snapshot::{
    BuildLayers, BuildReadyStateInput, FirecrackerBackend, RestoreContract, RestoreReadyStateInput,
    SanitizerContract, SnapshotBackend, no_secret_scan,
};

struct Config {
    api_url: String,
    token: String,
    agent_id: String,
    work: PathBuf,
    rootfs_size_mib: u64,
    once: bool,
    poll_secs: u64,
}

impl Config {
    fn from_env_args() -> Result<Self> {
        let args: Vec<String> = std::env::args().collect();
        let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
        let has = |name: &str| args.iter().any(|a| a == name);
        Ok(Config {
            api_url: std::env::var("ATO_API_URL").ok().or_else(|| flag("--api-url")).context("ATO_API_URL (or --api-url) required")?,
            token: std::env::var("SNAPSHOT_BUILDER_AGENT_TOKEN").context("SNAPSHOT_BUILDER_AGENT_TOKEN required")?,
            agent_id: flag("--agent-id").context("--agent-id required")?,
            work: flag("--work").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("snapshot-builder")),
            rootfs_size_mib: flag("--rootfs-size-mib").and_then(|s| s.parse().ok()).unwrap_or(1024),
            once: has("--once"),
            poll_secs: flag("--poll-secs").and_then(|s| s.parse().ok()).unwrap_or(15),
        })
    }
}

/// The server-resolved source identity on a claimed job (owner/repo/commit) — the only
/// authoritative source. A client-provided `source_ref` never appears here.
#[derive(Debug, Clone, Deserialize)]
struct ClaimedSource {
    github_owner: String,
    github_repo: String,
    commit_sha: String,
    subdirectory: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ClaimedJob {
    id: String,
    capsule_id: String,
    /// The target/profile this snapshot job was enqueued FOR (Track A/B: both
    /// `capsule_snapshot_jobs` and `capsule_snapshots` are target/profile-scoped).
    /// v1 seals only `target_label == manifest.default_target` with
    /// `profile == "default"` — anything else fails closed; the builder never
    /// silently substitutes the manifest default for a different requested target.
    target_label: String,
    profile: String,
    source: ClaimedSource,
    /// #932: the APPROVED store recipe manifest (`capsule_source_recipes.recipe_toml`),
    /// server-resolved with `source`. When present it is AUTHORITATIVE — materialized
    /// as `capsule.toml` at the source root (upstream repos deliberately carry none).
    /// Absent/None (older ato-api, or a recipe with no stored toml) ⇒ the repo's own
    /// capsule.toml is required, fail-closed exactly as before.
    #[serde(default)]
    recipe_toml: Option<String>,
}

/// The narrow v1 target/profile gate: a job may only build when it requests the
/// manifest's default target with the default profile. Otherwise the artifact identity
/// (and the `capsule_snapshots` row PR 3 writes from it) would be registered under a
/// target/profile that does NOT match what was actually built — fail closed instead.
fn v1_target_profile_gate(
    job_target_label: &str,
    job_profile: &str,
    manifest_default_target: &str,
) -> std::result::Result<(), (String, String)> {
    if job_profile != "default" || job_target_label != manifest_default_target {
        return Err((
            "eligibility".into(),
            format!(
                "requested target/profile is not supported by Ready-State builder v1 (requested {job_target_label}/{job_profile}; v1 builds only the manifest default target '{manifest_default_target}' with profile 'default')"
            ),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct ClaimResponse {
    jobs: Vec<ClaimedJob>,
}

/// Non-secret sealed-artifact metadata reported on a `sealed` ack (mirrors the ato-api
/// artifact schema). NEVER a secret.
#[derive(Debug, Clone, Serialize)]
struct Artifact {
    capsule_manifest_hash: String,
    execution_id: String,
    artifact_manifest_hash: String,
    runner_class_id: String,
    snapshot_backend: String,
    artifact_location: String,
    healthcheck_url_path: String,
    no_secret_scan_clean: bool,
    rootfs_bytes: u64,
    mem_bytes: u64,
    vmstate_bytes: u64,
    // ── #932 non-secret build provenance (diagnostics; never registry identity) ──
    /// Which manifest built this artifact: "recipe_toml" | "repo_capsule_toml".
    manifest_source: String,
    /// True when the readiness probe was synthesized from the declared port.
    synthesized_probe: bool,
    /// The manifest-declared run command, verbatim.
    declared_command: String,
    /// The command actually embedded into the guest init (post normalization).
    normalized_guest_command: String,
}

fn claim(cfg: &Config) -> Result<Vec<ClaimedJob>> {
    let resp: ClaimResponse = ureq::post(&format!("{}/v1/capsule-snapshots/jobs/claim", cfg.api_url))
        .set("authorization", &format!("Bearer {}", cfg.token))
        .send_json(ureq::json!({ "agent_id": cfg.agent_id, "capacity": 1 }))
        .map_err(|e| anyhow!("claim request: {e}"))?
        .into_json()
        .context("parse claim response")?;
    Ok(resp.jobs)
}

fn ack_sealed(cfg: &Config, job_id: &str, artifact: &Artifact) -> Result<()> {
    ureq::post(&format!("{}/v1/capsule-snapshots/jobs/{job_id}/ack", cfg.api_url))
        .set("authorization", &format!("Bearer {}", cfg.token))
        .send_json(ureq::json!({ "agent_id": cfg.agent_id, "status": "sealed", "artifact": artifact }))
        .map_err(|e| anyhow!("sealed ack: {e}"))?;
    Ok(())
}

fn ack_failed(cfg: &Config, job_id: &str, stage: &str, reason: &str) -> Result<()> {
    // Truncate the reason to a sane length; it is non-secret build output.
    let reason: String = reason.chars().take(1800).collect();
    ureq::post(&format!("{}/v1/capsule-snapshots/jobs/{job_id}/ack", cfg.api_url))
        .set("authorization", &format!("Bearer {}", cfg.token))
        .send_json(ureq::json!({ "agent_id": cfg.agent_id, "status": "failed", "failure_stage": stage, "failure_reason": reason }))
        .map_err(|e| anyhow!("failed ack: {e}"))?;
    Ok(())
}

/// Resolve the registry identity fields from a SEALED manifest — **never synthesized**.
///
/// `execution_id` must be the real Ato Execution Identity carried by the sealed manifest
/// (docs/execution-identity.md): it identifies *launch conditions*, so a value fabricated
/// from the job id / artifact hash would be a build-job identity, not an execution
/// identity — the same capsule/source/target/profile rebuilt would get a different id,
/// breaking runner-side verification against `capsule_snapshots.execution_id`. Until the
/// Ready-State build path stamps the true declared execution id into the manifest, a
/// missing value **fails closed** here (`failure_stage = artifact_metadata`).
fn sealed_identity(
    execution_id: Option<&str>,
    runner_class_id: Option<String>,
) -> std::result::Result<(String, String), (String, String)> {
    let exec = match execution_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => id.to_string(),
        None => {
            return Err(("artifact_metadata".into(), "missing execution_id in sealed Ready-State manifest".into()));
        }
    };
    let rc = match runner_class_id.filter(|s| !s.trim().is_empty()) {
        Some(rc) => rc,
        None => {
            return Err(("artifact_metadata".into(), "missing runner_class_id (build did not pin a runner class)".into()));
        }
    };
    Ok((exec, rc))
}

/// Build + seal + verify one claimed job. Returns the non-secret artifact metadata on
/// success, or `(failure_stage, failure_reason)` — never a panic, never a secret.
fn process_job(cfg: &Config, backend: &FirecrackerBackend, job: &ClaimedJob) -> std::result::Result<Artifact, (String, String)> {
    let fail = |stage: &str, e: String| (stage.to_string(), e);
    let jobdir = cfg.work.join(&job.id);
    let _ = std::fs::remove_dir_all(&jobdir);

    // 1. Materialize the SERVER-RESOLVED source (pinned commit; identity/subdir validated).
    // #932: a Store-recipe job carries the APPROVED recipe manifest on the claim — it is
    // materialized as capsule.toml at the source root (authoritative over any repo file,
    // because the Store-apply publish model stores the manifest server-side and upstream
    // repos carry none). A raw-GitHub job (no recipe_toml) requires the repo's own
    // capsule.toml, fail-closed exactly as before.
    let manifest_source = if job.recipe_toml.is_some() { "recipe_toml" } else { "repo_capsule_toml" };
    let src = materialize_source(
        &job.source.github_owner,
        &job.source.github_repo,
        &job.source.commit_sha,
        job.source.subdirectory.as_deref(),
        job.recipe_toml.as_deref(),
        &jobdir.join("src"),
    )
    .map_err(|e| fail("source", e))?;

    // 2. Parse the capsule.toml + derive a fail-closed build spec (rejects bindings/etc.).
    let toml_bytes = std::fs::read(src.join("capsule.toml")).map_err(|e| fail("manifest", e.to_string()))?;
    let toml_text = String::from_utf8_lossy(&toml_bytes).into_owned();
    let manifest = CapsuleManifest::from_toml(&toml_text).map_err(|e| fail("manifest", e.to_string()))?;
    // v1 target/profile gate: only the manifest default target with profile "default"
    // may seal (never silently substitute the default for a different requested target).
    v1_target_profile_gate(&job.target_label, &job.profile, manifest.default_target.trim())?;
    let spec = derive_build_spec(&manifest, &SourceProbe::scan(&src)).map_err(|e| fail("eligibility", e))?;

    // 2b. The declared Ato Execution Identity for this build — computed from DECLARED,
    // host-independent facts only (the server-resolved pinned source + the manifest's
    // default target/runtime/working-dir/dependencies), via the same graph
    // canonicalization the launch path uses. Never from the job id / artifact hash /
    // builder-host state. Stamped into the sealed manifest by build_ready_state.
    let target = manifest.resolve_default_target().map_err(|e| fail("manifest", e.to_string()))?;
    let envelope = ReadyStateDeclaredEnvelope {
        source_identifier: store_source_identifier(
            &job.source.github_owner,
            &job.source.github_repo,
            &job.source.commit_sha,
            job.source.subdirectory.as_deref(),
        ),
        // The REQUESTED target (gate-validated == manifest.default_target): the identity
        // is computed for the target actually being built, never a substituted one.
        target_label: job.target_label.clone(),
        runtime: target.runtime.clone(),
        working_directory: target.working_dir.clone(),
        dependencies: declared_dependencies_from_manifest_toml(&toml_text).map_err(|e| fail("artifact_metadata", e))?,
        network_policy_hash: None,
        capability_policy_hash: None,
    };
    let declared_execution_id = envelope.declared_execution_id();

    // 3. Build the bootable rootfs (Docker→ext4; commands run only in Docker/guest).
    let ext4 = jobdir.join("rootfs.ext4");
    build_rootfs(&src, &spec, &ext4, cfg.rootfs_size_mib).map_err(|e| fail("rootfs_build", e))?;
    let rootfs = std::fs::read(&ext4).map_err(|e| fail("rootfs_build", e.to_string()))?;

    // 4. Ready-State build: boot → verify healthcheck → snapshot → seal (no bindings/UFFD).
    let store = CasStore::open(jobdir.join("cas")).map_err(|e| fail("build_ready_state", e.to_string()))?;
    let capsule_manifest_hash = format!("blake3:{}", blake3::hash(&toml_bytes).to_hex());
    let receipt = backend
        .build_ready_state(BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: capsule_manifest_hash.clone(),
            runner_class: None,
            layers: BuildLayers { rootfs, runtime: None, dependency: None, app: None, vmstate: Vec::new(), memory: Vec::new() },
            restore_contract: RestoreContract { ports: vec![spec.port], healthcheck: Some(spec.healthcheck.clone()), expected_ready_ms: Some(8000) },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: vec![],
            execution_id: Some(declared_execution_id),
        })
        .map_err(|e| fail("build_ready_state", e.to_string()))?;
    let manifest_out = receipt.manifest.clone();

    // 5. Verify the sealed artifact RESTORES before we call it sealed (no traffic exposed).
    let restored = backend
        .restore(RestoreReadyStateInput { store: &store, manifest: manifest_out.clone(), overlay_root: jobdir.join("verify-ov"), host_runner_class: None, uffd_preview: false })
        .map_err(|e| fail("restore_verify", e.to_string()))?;
    let _ = backend.stop(restored.session);

    // 6. No-secret scan: the build gate + the reusable L4 scanner over the CAS (canaries).
    let l4 = no_secret_scan::scan(
        &no_secret_scan::ScanTargets { cas: Some(jobdir.join("cas")), ..Default::default() },
        &[b"BEGIN PRIVATE KEY", b"BEGIN RSA PRIVATE KEY", b"AKIA"],
    );
    let no_secret_scan_clean = receipt.no_secret_proof.is_clean() && l4.clean;
    if !no_secret_scan_clean {
        return Err(fail("no_secret_scan", "sealed artifact failed the no-secret scan".into()));
    }

    // Registry identity/location fields (capsule_snapshots contract, #154/#157). All must
    // be REAL — never synthesized (see sealed_identity). artifact_location is the CAS URI
    // PR 3 records (the artifact lives in this job's content-addressed store; job-scoped
    // storage, not identity).
    let artifact_manifest_hash = manifest_out.id();
    let (execution_id, runner_class_id) = sealed_identity(
        manifest_out.execution_id.as_deref(),
        manifest_out.runner_class_id.as_ref().map(|c| c.to_string()),
    )?;
    let artifact_location = format!("cas://{}/{}", job.id, artifact_manifest_hash);

    // 7. Persist the sealed manifest beside the CAS (Track E): `cas://<job_id>/<hash>`
    // names <work>/<job_id>/{manifest.json, cas/}, and a runner restores by loading
    // manifest.json, verifying `manifest.id() == artifact_manifest_hash` (fail-closed),
    // then restoring from the co-located CAS. The manifest is derived entirely from
    // already-scanned sealed content + non-secret metadata (hashes, contracts, sizes) —
    // it carries no layer bytes and no secrets.
    let manifest_json = serde_json::to_vec_pretty(&manifest_out).map_err(|e| fail("artifact_metadata", format!("serialize sealed manifest: {e}")))?;
    if !no_secret_scan::blob_is_clean(&manifest_json, &[b"BEGIN PRIVATE KEY", b"BEGIN RSA PRIVATE KEY", b"AKIA"]) {
        return Err(fail("no_secret_scan", "sealed manifest json failed the no-secret scan".into()));
    }
    std::fs::write(jobdir.join("manifest.json"), &manifest_json).map_err(|e| fail("artifact_metadata", format!("persist sealed manifest: {e}")))?;

    Ok(Artifact {
        capsule_manifest_hash,
        execution_id,
        artifact_manifest_hash,
        runner_class_id,
        snapshot_backend: manifest_out.snapshot_backend.kind.clone(),
        artifact_location,
        healthcheck_url_path: spec.healthcheck,
        no_secret_scan_clean: true,
        rootfs_bytes: manifest_out.layers.rootfs.as_ref().map(|m| m.total_len).unwrap_or(0),
        mem_bytes: manifest_out.layers.memory.as_ref().map(|m| m.total_len).unwrap_or(0),
        vmstate_bytes: manifest_out.layers.vmstate.as_ref().map(|m| m.total_len).unwrap_or(0),
        // #932 build provenance — lands in receipt_json via the sealed ack (diagnostics
        // only; the ato-api registry identity comparison never reads these).
        manifest_source: manifest_source.to_string(),
        synthesized_probe: spec.probe_synthesized,
        declared_command: spec.declared_start_cmd,
        normalized_guest_command: spec.start_cmd,
    })
}

fn run_once(cfg: &Config, backend: &FirecrackerBackend) -> Result<usize> {
    let jobs = claim(cfg)?;
    for job in &jobs {
        eprintln!("[builder] claimed {} (capsule {})", job.id, job.capsule_id);
        match process_job(cfg, backend, job) {
            Ok(artifact) => {
                eprintln!("[builder] sealed {} (artifact {})", job.id, artifact.artifact_manifest_hash);
                ack_sealed(cfg, &job.id, &artifact)?;
            }
            Err((stage, reason)) => {
                eprintln!("[builder] failed {} at {stage}: {reason}", job.id);
                ack_failed(cfg, &job.id, &stage, &reason)?;
            }
        }
    }
    Ok(jobs.len())
}

fn main() -> Result<()> {
    let cfg = Config::from_env_args()?;
    if !FirecrackerBackend::kvm_present() {
        eprintln!("snapshot-builder: /dev/kvm absent — this must run on a KVM+Docker builder host");
        std::process::exit(2);
    }
    std::fs::create_dir_all(&cfg.work)?;
    let backend = FirecrackerBackend::new();
    loop {
        match run_once(&cfg, &backend) {
            Ok(n) => {
                if cfg.once {
                    eprintln!("[builder] --once: processed {n} job(s), exiting");
                    break;
                }
                if n == 0 {
                    std::thread::sleep(std::time::Duration::from_secs(cfg.poll_secs));
                }
            }
            Err(e) => {
                eprintln!("[builder] loop error: {e}");
                if cfg.once {
                    return Err(e);
                }
                std::thread::sleep(std::time::Duration::from_secs(cfg.poll_secs));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_ato_api_claim_response() {
        // Shape emitted by ato-api claimSnapshotJobs (#156): extra fields are ignored.
        let body = serde_json::json!({
            "jobs": [{
                "id": "job_1", "capsule_id": "cap_1",
                "source": { "source_kind": "github", "github_owner": "acme", "github_repo": "app", "commit_sha": "a".repeat(40), "subdirectory": null },
                "target_label": "web", "profile": "default", "claim_expires_at": "2026-01-01T00:00:00.000Z"
            }]
        });
        let resp: ClaimResponse = serde_json::from_value(body).unwrap();
        assert_eq!(resp.jobs.len(), 1);
        assert_eq!(resp.jobs[0].source.github_owner, "acme");
        assert_eq!(resp.jobs[0].source.commit_sha.len(), 40);
        assert!(resp.jobs[0].source.subdirectory.is_none());
        // target_label/profile are REQUIRED claim fields (target/profile-scoped registry).
        assert_eq!(resp.jobs[0].target_label, "web");
        assert_eq!(resp.jobs[0].profile, "default");
        // #932: an OLDER ato-api without recipe_toml parses as None (repo-manifest path).
        assert!(resp.jobs[0].recipe_toml.is_none());
    }

    #[test]
    fn parses_a_claim_with_a_recipe_manifest() {
        // #932: a Store-recipe job carries the approved recipe manifest on the claim.
        let body = serde_json::json!({
            "jobs": [{
                "id": "job_2", "capsule_id": "cap_2",
                "source": { "source_kind": "github", "github_owner": "acme", "github_repo": "app", "commit_sha": "b".repeat(40), "subdirectory": null },
                "recipe_toml": "schema_version = \"0.3\"\ndefault_target = \"app\"\n",
                "target_label": "app", "profile": "default", "claim_expires_at": "2026-01-01T00:00:00.000Z"
            }]
        });
        let resp: ClaimResponse = serde_json::from_value(body).unwrap();
        assert_eq!(resp.jobs[0].recipe_toml.as_deref(), Some("schema_version = \"0.3\"\ndefault_target = \"app\"\n"));
        // An explicit null is also the repo-manifest path (recipe stored no toml).
        let body = serde_json::json!({
            "jobs": [{
                "id": "job_3", "capsule_id": "cap_3",
                "source": { "source_kind": "github", "github_owner": "acme", "github_repo": "app", "commit_sha": "c".repeat(40), "subdirectory": null },
                "recipe_toml": null,
                "target_label": "app", "profile": "default", "claim_expires_at": "2026-01-01T00:00:00.000Z"
            }]
        });
        let resp: ClaimResponse = serde_json::from_value(body).unwrap();
        assert!(resp.jobs[0].recipe_toml.is_none());
    }

    #[test]
    fn v1_seals_only_the_default_target_with_the_default_profile() {
        // Match ⇒ Ok.
        assert!(v1_target_profile_gate("app", "default", "app").is_ok());
        // Requested target ≠ manifest default ⇒ fail closed at eligibility — the builder
        // must NOT silently substitute the default target for the requested one.
        let err = v1_target_profile_gate("web", "default", "app").unwrap_err();
        assert_eq!(err.0, "eligibility");
        assert!(err.1.contains("not supported by Ready-State builder v1"), "{}", err.1);
        assert!(err.1.contains("web/default"), "{}", err.1);
        // Non-default profile ⇒ fail closed.
        let err = v1_target_profile_gate("app", "gpu", "app").unwrap_err();
        assert_eq!(err.0, "eligibility");
        assert!(err.1.contains("app/gpu"), "{}", err.1);
    }

    #[test]
    fn execution_id_is_never_synthesized() {
        // Missing / blank execution_id ⇒ fail closed at artifact_metadata — the builder
        // must NOT fabricate an id from job/artifact hashes (review: a synthetic id is a
        // build-job identity, not an Ato Execution Identity).
        let err = sealed_identity(None, Some("rc".into())).unwrap_err();
        assert_eq!(err.0, "artifact_metadata");
        assert!(err.1.contains("missing execution_id"), "{}", err.1);
        let err = sealed_identity(Some("   "), Some("rc".into())).unwrap_err();
        assert_eq!(err.0, "artifact_metadata");

        // Missing runner_class_id also fails closed (never "unknown").
        let err = sealed_identity(Some("real-declared-id"), None).unwrap_err();
        assert_eq!(err.0, "artifact_metadata");
        assert!(err.1.contains("runner_class_id"), "{}", err.1);

        // A real manifest identity passes through VERBATIM (no rewriting).
        let (exec, rc) = sealed_identity(Some("real-declared-id"), Some("blake3:rc".into())).unwrap();
        assert_eq!(exec, "real-declared-id");
        assert_eq!(rc, "blake3:rc");
    }

    #[test]
    fn artifact_matches_the_sealed_ack_schema() {
        // The ato-api artifactSchema (#157, extended by #932) is .strict(): the
        // sealed-ack body must carry exactly these keys and nothing else.
        let a = Artifact {
            capsule_manifest_hash: "blake3:c".into(),
            execution_id: "exec-1".into(),
            artifact_manifest_hash: "blake3:a".into(),
            runner_class_id: "rc".into(),
            snapshot_backend: "firecracker".into(),
            artifact_location: "cas://job/blake3:a".into(),
            healthcheck_url_path: "/health".into(),
            no_secret_scan_clean: true,
            rootfs_bytes: 1,
            mem_bytes: 2,
            vmstate_bytes: 3,
            manifest_source: "recipe_toml".into(),
            synthesized_probe: true,
            declared_command: "app.py".into(),
            normalized_guest_command: "python3 app.py".into(),
        };
        let v = serde_json::to_value(&a).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "artifact_location", "artifact_manifest_hash", "capsule_manifest_hash", "declared_command", "execution_id",
                "healthcheck_url_path", "manifest_source", "mem_bytes", "no_secret_scan_clean", "normalized_guest_command",
                "rootfs_bytes", "runner_class_id", "snapshot_backend", "synthesized_probe", "vmstate_bytes"
            ]
        );
        assert_eq!(obj["no_secret_scan_clean"], serde_json::json!(true));
        // #932 provenance values are enum-safe for the ato-api schema.
        assert!(matches!(obj["manifest_source"].as_str().unwrap(), "recipe_toml" | "repo_capsule_toml"));
        // No placeholder identity/location fields.
        for k in ["execution_id", "runner_class_id", "artifact_location"] {
            assert_ne!(obj[k].as_str().unwrap(), "unknown");
        }
    }
}
