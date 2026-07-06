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
//! ack); never trust the job's `source_ref` / any client source; no Phase 8 BindingLease
//! path; UFFD is not enabled; no traffic is ever exposed — the daemon only builds +
//! seals + verifies.
//!
//! v1.2 PR 3d-2: a `[secrets.*]` capsule may build via the SUPERVISOR path (agent-as-
//! init rootfs + the backend's placeholder build drive, #962) — but ONLY when the
//! operator opts this builder in with `ATO_BUILDER_SUPERVISOR=1` AND `ATO_GUEST_AGENT_BIN`
//! + `ATO_FC_VSOCK=1` are set; otherwise secret capsules keep failing closed at
//! eligibility exactly as before. No secret VALUE ever reaches this daemon either way —
//! the build uses backend-internal placeholders and the seal stays pre-bind.
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
use snapshot::rootfs_builder::{
    RootfsBuildSpec, SourceProbe, build_rootfs, derive_build_spec, derive_supervisor_build_spec,
    materialize_source,
};
use snapshot::state_volume::DurableVolumeSpec;
use snapshot::{
    BuildLayers, BuildReadyStateInput, FirecrackerBackend, RestoreContract, RestoreReadyStateInput,
    SanitizerContract, SnapshotBackend, SupervisorBindings, no_secret_scan,
};

/// PEM-marker literals: a GATE for the sealed `manifest.json` (small, structured,
/// builder-authored — a PEM marker there is always wrong) and an ADVISORY sweep over
/// the CAS.
///
/// They must NOT gate the CAS. #932 finding 4, measured twice on a real capsule:
/// the 4-byte `AKIA` literal hit random binary offsets of a 1 GiB rootfs (no key
/// material — context inspection), and even these long PEM literals hit the string
/// constant tables of ordinary ssh/crypto libraries (`…openssh-key-v1…-----BEGIN
/// OPENSSH PRIVATE KEY-----…-----END OPENSSH PRI…` — header adjacent to footer,
/// i.e. format constants, not a key). This mirrors the seal-side scanner's
/// empirically-derived policy (`snapshot::scanner`, `ato-rs-policy/1`): literal/
/// heuristic hits over opaque images (rootfs/memory) are advisory, never gating.
/// What DOES gate the CAS is [`live_secret_canaries`] — exact values of the
/// builder's own credentials, the actual leak threat, with zero false positives.
/// `l4_canaries_are_long_enough_to_gate_binaries` enforces the minimum length.
const L4_CANARIES: &[&[u8]] = &[
    b"BEGIN PRIVATE KEY",
    b"BEGIN RSA PRIVATE KEY",
    b"BEGIN OPENSSH PRIVATE KEY",
    b"BEGIN EC PRIVATE KEY",
];

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
    /// v1.2 PR 3e-2c: SUPERVISOR (binding-required) artifact facts — binding NAMES
    /// only, never a value. `Some` ⇒ ato-api registers the row with
    /// `no_binding_required=false` (+ persists the names) and the firewall CHECK
    /// keeps it permanently non-public. Omitted entirely for a no-binding artifact
    /// (`skip_serializing_if`), so those acks stay byte-identical against the
    /// `.strict()` ack schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    supervisor_build: Option<SupervisorAck>,
}

/// v1.2 PR 3e-2c: the supervisor facet of a sealed ack — names only.
#[derive(Debug, Clone, Serialize)]
struct SupervisorAck {
    binding_names: Vec<String>,
}

/// The builder host's own LIVE secrets, as exact-value canaries for the L4 CAS gate:
/// "did MY credentials leak into the sealed artifact?" — the concrete threat a builder
/// host adds (env reaching a build layer). Exact long random values cannot
/// false-positive on library constants or binary noise (#932 finding 4). Values are
/// compared in-memory only; scan results carry paths, never content. Trivially short
/// values are excluded — they could only produce noise, never a real credential.
fn live_secret_canaries(cfg: &Config) -> Vec<&[u8]> {
    let mut v: Vec<&[u8]> = Vec::new();
    if cfg.token.len() >= 16 {
        v.push(cfg.token.as_bytes());
    }
    v
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

/// v1.2 PR 3d-2: whether this builder is opted into SUPERVISOR builds for
/// `[secrets.*]` capsules. Off by default — secret capsules then keep failing
/// closed at eligibility exactly as v1 did.
fn supervisor_builds_enabled() -> bool {
    matches!(std::env::var("ATO_BUILDER_SUPERVISOR").ok().as_deref(), Some("1" | "true" | "yes" | "on"))
}

/// Mirror of the snapshot backend's `ATO_FC_VSOCK` gate (kept private there); the
/// backend re-checks and fails closed regardless — this early copy only exists to
/// fail a supervisor job at ELIGIBILITY with an actionable message instead of
/// after a rootfs build.
fn builder_vsock_enabled() -> bool {
    matches!(std::env::var("ATO_FC_VSOCK").ok().as_deref(), Some("1" | "true" | "yes" | "on"))
}

/// v1.2 PR 3d-2: choose the build-spec derivation for a job. A `[secrets.*]` capsule
/// takes the SUPERVISOR path (agent-as-init rootfs; the backend then drives
/// placeholder-deliver → health → StopWorkload → Revoke before the seal) — but only
/// when the operator opted in AND the prerequisites hold, each checked fail-closed
/// with an actionable reason. A no-secret capsule keeps the v1 derivation untouched.
fn derive_job_spec(
    manifest: &CapsuleManifest,
    probe: &SourceProbe,
    supervisor_enabled: bool,
    guest_agent_bin_set: bool,
    vsock_on: bool,
) -> std::result::Result<RootfsBuildSpec, (String, String)> {
    let fail = |e: String| ("eligibility".to_string(), e);
    if manifest.secrets.is_empty() {
        // v1 no-binding path, byte-for-byte unchanged (it also rejects any stray
        // bindings/external/GPU itself).
        return derive_build_spec(manifest, probe).map_err(fail);
    }
    if !supervisor_enabled {
        return Err(fail(
            "capsule declares [secrets.*]: supervisor builds are disabled on this builder \
             (operator opt-in: ATO_BUILDER_SUPERVISOR=1)"
                .into(),
        ));
    }
    if !guest_agent_bin_set {
        return Err(fail(
            "supervisor build requires ATO_GUEST_AGENT_BIN (path to the guest-agent binary \
             staged into the rootfs as /sbin-init supervisor)"
                .into(),
        ));
    }
    if !vsock_on {
        return Err(fail(
            "supervisor build requires the vsock binding channel: set ATO_FC_VSOCK=1 \
             (the guest-agent gates workload start on placeholder delivery)"
                .into(),
        ));
    }
    derive_supervisor_build_spec(manifest, probe).map_err(fail)
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
    // v1.2 PR 3d-2: secret capsules dispatch to the supervisor derivation when this
    // builder is opted in (each prerequisite fail-closed with an actionable reason);
    // no-secret capsules keep the v1 derivation untouched.
    let spec = derive_job_spec(
        &manifest,
        &SourceProbe::scan(&src),
        supervisor_builds_enabled(),
        std::env::var("ATO_GUEST_AGENT_BIN").map(|v| !v.trim().is_empty()).unwrap_or(false),
        builder_vsock_enabled(),
    )?;

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

    // 4. Ready-State build: boot → verify healthcheck → snapshot → seal (no UFFD). For
    // a supervisor spec the backend drives the whole placeholder protocol itself
    // (deliver → health → StopWorkload → Revoke → seal, #962); the daemon only passes
    // the binding NAMES — no secret value exists anywhere in this process.
    let store = CasStore::open(jobdir.join("cas")).map_err(|e| fail("build_ready_state", e.to_string()))?;
    let capsule_manifest_hash = format!("blake3:{}", blake3::hash(&toml_bytes).to_hex());
    // v1.2 PR 3e-2c: capture the supervisor binding names for the SEALED ACK. ato-api's
    // artifactSchema now accepts an optional `supervisor_build` (3e-2), so the ack must
    // carry the names — otherwise ato-api registers the row as no-binding + PUBLIC
    // (the E2E caught exactly this). A no-binding capsule keeps the field absent, so
    // those acks stay byte-identical against the .strict() schema.
    let supervisor_ack = spec
        .supervisor
        .as_ref()
        .map(|s| SupervisorAck { binding_names: s.binding_names.clone() });
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
            supervisor: spec.supervisor.as_ref().map(|s| {
                // v1.6 (ato#983) Slice 2: flatten every service's durable state
                // volumes into one list (the backend attaches them as drives; it
                // doesn't need per-service association — that's Slice 3's job,
                // via the target path baked into supervisor.json). owner_scope
                // reuses the SAME identity the OCI/container persistent-state
                // path already keys its registry on
                // (`capsule::foundation::types::manifest_validation::persistent_state_owner_scope`)
                // — one definition of "whose durable state is this", not two.
                let volumes: Vec<DurableVolumeSpec> = s
                    .services
                    .iter()
                    .flatten()
                    .flat_map(|svc| &svc.volumes)
                    .map(|v| DurableVolumeSpec { state_name: v.state_name.clone(), size_mb: v.size_mb })
                    .collect();
                let state_owner_scope =
                    if volumes.is_empty() { None } else { manifest.persistent_state_owner_scope() };
                SupervisorBindings { binding_names: s.binding_names.clone(), state_volumes: volumes, state_owner_scope }
            }),
        })
        .map_err(|e| fail("build_ready_state", e.to_string()))?;
    let manifest_out = receipt.manifest.clone();

    // 5. Verify the sealed artifact RESTORES before we call it sealed (no traffic
    // exposed). A supervisor artifact's restore-readiness is the backend's agent
    // probe (reachable + NOT bound-ready, #962) — no health wait, no binding needed.
    let restored = backend
        .restore(RestoreReadyStateInput { store: &store, manifest: manifest_out.clone(), overlay_root: jobdir.join("verify-ov"), host_runner_class: None, uffd_preview: false })
        .map_err(|e| fail("restore_verify", e.to_string()))?;
    let _ = backend.stop(restored.session);

    // 6. No-secret scan — two GATES + one ADVISORY, each failure says which tripped
    // (path-only, never content):
    //  (a) the seal-side proof: the policy-versioned `snapshot::scanner` (declared
    //      markers block everywhere; provider-key/env heuristics block on the
    //      build-authored layers, advisory on opaque images — empirical policy);
    //  (b) the L4 LIVE-SECRET gate: the builder's own credentials as exact-value
    //      canaries over the CAS. This is the real leak threat a builder host adds
    //      (its env reaching the image), and exact long random values cannot
    //      false-positive. The values are compared in-memory only — never logged,
    //      never persisted (hits carry paths only);
    //  (c) the PEM-marker sweep, ADVISORY on the CAS (#932 finding 4: ordinary
    //      ssh/crypto libraries carry these strings as format constants).
    if !receipt.no_secret_proof.is_clean() {
        return Err(fail(
            "no_secret_scan",
            format!("seal-side no-secret proof is not clean ({} finding(s), verdict {:?})", receipt.no_secret_proof.findings.len(), receipt.no_secret_proof.verdict),
        ));
    }
    let cas_targets = no_secret_scan::ScanTargets { cas: Some(jobdir.join("cas")), ..Default::default() };
    let live: Vec<&[u8]> = live_secret_canaries(cfg);
    let leak = no_secret_scan::scan(&cas_targets, &live);
    if !leak.clean {
        let first = leak.hits.first().map(|h| format!("{}:{}", h.target, h.path)).unwrap_or_default();
        return Err(fail(
            "no_secret_scan",
            format!("builder credential found in the sealed artifact: {} file(s) across {} scanned; first: {first}", leak.hits.len(), leak.files_scanned),
        ));
    }
    let pem = no_secret_scan::scan(&cas_targets, L4_CANARIES);
    if !pem.clean {
        let first = pem.hits.first().map(|h| format!("{}:{}", h.target, h.path)).unwrap_or_default();
        eprintln!(
            "[builder] advisory: PEM-format markers in {} of {} CAS file(s) (library string constants are common — not gating; #932 finding 4) first: {first}",
            pem.hits.len(),
            pem.files_scanned
        );
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
    if !no_secret_scan::blob_is_clean(&manifest_json, L4_CANARIES) {
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
        supervisor_build: supervisor_ack,
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

    // ── v1.2 PR 3d-2: supervisor dispatch ────────────────────────────────────

    fn probe_python() -> SourceProbe {
        SourceProbe {
            has_package_json: false,
            has_requirements_txt: false,
            has_pyproject: false,
            has_index_html: false,
            has_py_files: true,
        }
    }

    fn manifest(secrets: bool) -> CapsuleManifest {
        let base = "schema_version = \"0.3\"\nname = \"t\"\nversion = \"0.1.0\"\ntype = \"app\"\n\
                    default_target = \"app\"\n[targets.app]\nruntime = \"source\"\n\
                    run = \"python3 app.py\"\nport = 8080\n\
                    readiness_probe = { http_get = \"/health\" }\n";
        let toml = if secrets {
            format!("{base}[secrets.openai_api_key]\nrequired = true\nenv = \"OPENAI_API_KEY\"\n")
        } else {
            base.to_string()
        };
        CapsuleManifest::from_toml(&toml).unwrap()
    }

    #[test]
    fn no_secret_capsule_keeps_the_v1_derivation_regardless_of_flags() {
        // Flags on or off, a no-secret capsule derives the v1 no-binding spec.
        for (sup, bin, vsock) in [(false, false, false), (true, true, true)] {
            let spec = derive_job_spec(&manifest(false), &probe_python(), sup, bin, vsock).unwrap();
            assert!(spec.supervisor.is_none());
            assert_eq!(spec.start_cmd, "python3 app.py");
        }
    }

    #[test]
    fn secret_capsule_fails_closed_unless_every_supervisor_prerequisite_holds() {
        let m = manifest(true);
        // Opt-in flag off → eligibility failure naming the flag (v1 behavior for
        // secret capsules stays "no", just with an actionable reason).
        let err = derive_job_spec(&m, &probe_python(), false, true, true).unwrap_err();
        assert_eq!(err.0, "eligibility");
        assert!(err.1.contains("ATO_BUILDER_SUPERVISOR"), "{}", err.1);
        // Missing guest-agent binary → names the env var.
        let err = derive_job_spec(&m, &probe_python(), true, false, true).unwrap_err();
        assert_eq!(err.0, "eligibility");
        assert!(err.1.contains("ATO_GUEST_AGENT_BIN"), "{}", err.1);
        // vsock off → names the flag.
        let err = derive_job_spec(&m, &probe_python(), true, true, false).unwrap_err();
        assert_eq!(err.0, "eligibility");
        assert!(err.1.contains("ATO_FC_VSOCK"), "{}", err.1);
    }

    #[test]
    fn secret_capsule_with_all_prerequisites_derives_a_supervisor_spec() {
        let spec = derive_job_spec(&manifest(true), &probe_python(), true, true, true).unwrap();
        let sup = spec.supervisor.as_ref().expect("supervisor spec");
        assert_eq!(sup.binding_names, vec!["openai_api_key"]);
        assert_eq!(sup.env_map.get("OPENAI_API_KEY").map(String::as_str), Some("openai_api_key"));
        // Runtime/port/probe detection identical to the v1 path.
        assert_eq!(spec.start_cmd, "python3 app.py");
        assert_eq!(spec.port, 8080);
        assert_eq!(spec.healthcheck, "/health");
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
    fn l4_canaries_are_long_enough_to_gate_binaries() {
        // #932 finding 4: a short literal over GiB-scale binaries false-positives at
        // random offsets (expected hits ≈ windows × 256^-len; 4 bytes ⇒ multiple hits
        // per GiB). 12+ bytes pushes the expectation to ~zero across the fleet. Never
        // re-add a bare provider prefix here — that detection belongs to the
        // policy-versioned seal scanner (snapshot::scanner).
        for c in L4_CANARIES {
            assert!(c.len() >= 12, "canary {:?} is too short to gate binary artifacts", String::from_utf8_lossy(c));
        }
    }

    #[test]
    fn l4_canaries_flag_pem_but_not_bare_provider_prefixes() {
        // A random-binary AKIA occurrence (finding 4's false-positive class) must pass…
        let binary_with_akia = [b"\x00\x9fAKIA\xffQ\x11 random bytes".as_slice(), &[0u8; 64]].concat();
        assert!(no_secret_scan::blob_is_clean(&binary_with_akia, L4_CANARIES));
        // …while PEM markers are detected (gating for manifest.json; advisory on CAS).
        let pem = b"-----BEGIN PRIVATE KEY-----\nMIIEvg==\n-----END PRIVATE KEY-----";
        assert!(!no_secret_scan::blob_is_clean(pem, L4_CANARIES));
        let openssh = b"-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaA==";
        assert!(!no_secret_scan::blob_is_clean(openssh, L4_CANARIES));
    }

    #[test]
    fn live_secret_canaries_gate_the_builder_token_but_skip_trivial_values() {
        let mk = |token: &str| Config {
            api_url: "https://api".into(),
            token: token.into(),
            agent_id: "a".into(),
            work: std::env::temp_dir(),
            rootfs_size_mib: 1024,
            once: true,
            poll_secs: 15,
        };
        // A real (long, random) token gates: an artifact containing it is dirty.
        let cfg = mk("0123456789abcdef0123456789abcdef");
        let canaries = live_secret_canaries(&cfg);
        assert_eq!(canaries.len(), 1);
        let leaked = [b"layer bytes ".as_slice(), cfg.token.as_bytes(), b" more"].concat();
        assert!(!no_secret_scan::blob_is_clean(&leaked, &canaries));
        assert!(no_secret_scan::blob_is_clean(b"layer bytes without the token", &canaries));
        // A trivially short token is excluded — it could only produce noise.
        assert!(live_secret_canaries(&mk("short")).is_empty());
    }

    #[test]
    fn planted_builder_token_fails_the_cas_scan_without_printing_it() {
        // compat fixture `planted-builder-token`, executable (local-only): an artifact
        // whose CAS content contains the builder's OWN token — exact value — must fail
        // the no_secret_scan, and nothing the scan reports may carry the token value.
        // Runs with a FAKE token so no real credential ever touches a fixture; this is
        // the runtime proof behind the contract row the fixture can only declare.
        let fake_token = "fake-builder-token-a1b2c3d4e5f6a7b8"; // long enough to gate
        let cfg = Config {
            api_url: "https://api".into(),
            token: fake_token.into(),
            agent_id: "a".into(),
            work: std::env::temp_dir(),
            rootfs_size_mib: 1024,
            once: true,
            poll_secs: 15,
        };
        let cas = std::env::temp_dir().join(format!("compat-planted-token-{}", std::process::id()));
        std::fs::create_dir_all(&cas).unwrap();
        std::fs::write(cas.join("layer.bin"), format!("prefix {fake_token} suffix")).unwrap();

        let targets = no_secret_scan::ScanTargets { cas: Some(cas.clone()), ..Default::default() };
        let canaries = live_secret_canaries(&cfg);
        let result = no_secret_scan::scan(&targets, &canaries);
        std::fs::remove_dir_all(&cas).ok();

        assert!(!result.clean, "a planted builder token must fail the CAS scan");
        assert_eq!(result.hits.len(), 1);
        // The report (what would reach logs / the failed-ack reason) must never
        // contain the token value — only the target label and file path.
        let report = format!("{result:?}");
        assert!(!report.contains(fake_token), "scan report must not print the token value");
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
            supervisor_build: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        // A NO-BINDING ack omits supervisor_build entirely (byte-identical vs the
        // pre-3e-2c schema, which the .strict() ato-api validator requires).
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

    #[test]
    fn supervisor_artifact_ack_carries_binding_names() {
        // v1.2 PR 3e-2c: a supervisor ack DOES include supervisor_build.binding_names
        // (names only) so ato-api derives no_binding_required=false + persists them.
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
            supervisor_build: Some(SupervisorAck { binding_names: vec!["openai_api_key".into()] }),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(
            v["supervisor_build"],
            serde_json::json!({ "binding_names": ["openai_api_key"] })
        );
        // Still no secret value anywhere in the ack.
        assert!(!serde_json::to_string(&v).unwrap().to_lowercase().contains("sk-"));
    }
}
