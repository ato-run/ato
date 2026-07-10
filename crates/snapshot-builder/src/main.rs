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
//! ato#1002: jobs carry a `kind`. `recipe` (the default, and the only pre-#1002 kind)
//! keeps the exact pipeline above. `dockerfile_import` clones the server-resolved
//! pinned commit WITHOUT a capsule.toml (an import candidate by definition has none),
//! validates the job's strict `params` fail-closed, runs the v1.7 Dockerfile import
//! (`snapshot::docker_import`, secret policy fixed to Reject), and feeds the packed
//! ext4 into the SAME seal → restore-verify → scan → ack tail. The daemon advertises
//! both kinds via `supported_kinds` on the claim.
//!
//! ```sh
//! ATO_API_URL=https://api… SNAPSHOT_BUILDER_AGENT_TOKEN=… ATO_FC_BIN=… ATO_FC_KERNEL=… \
//!   snapshot-builder --agent-id builder-1 [--once]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use capsule::engine::execution_graph::{
    ReadyStateDeclaredEnvelope, declared_dependencies_from_manifest_toml, store_source_identifier,
};
use capsule::foundation::types::manifest::CapsuleManifest;
use capsulefs::CasStore;
use serde::{Deserialize, Serialize};
use snapshot::docker_import::build::SystemImportCommandRunner;
use snapshot::docker_import::{
    DockerImportSpec, DockerfileImportRequest, SecretEnvPolicy, import_descriptor_blake3,
    import_execution_id, run_dockerfile_import, validate_dockerfile_path,
};
use snapshot::rootfs_builder::{
    RootfsBuildSpec, SourceProbe, build_rootfs, derive_build_spec, derive_supervisor_build_spec,
    materialize_source, reject_control_chars, valid_github_owner, valid_github_repo,
};
use snapshot::state_volume::DurableVolumeSpec;
use snapshot::{
    BuildLayers, BuildReadyStateInput, FirecrackerBackend, RestoreContract, RestoreReadyStateInput,
    SanitizerContract, SnapshotBackend, SupervisorBindings, no_secret_scan,
};

mod upload;

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

/// ato-api Hardware Binding Layer (RFC `docs/rfcs/draft/snapshot-artifact-format.md`,
/// `asf.*` namespace): the logical composition of everything this builder seals
/// (memory image + vmstate + rootfs + receipt) — the same for both the recipe
/// and dockerfile_import lanes, since both produce the identical Firecracker
/// snapshot shape. Only one format exists today; a new one is an RFC update in
/// ato-api, not a local choice made here.
const SNAPSHOT_FORMAT_ID: &str = "asf.fc-memsnap-v1";

/// ato-api Hardware Binding Layer (RFC `docs/rfcs/draft/snapshot-codec-registry.md`,
/// `asc.*` namespace): how the artifact bytes are encoded — today, byte-identical
/// to the raw layer bytes (no chunking/compression/checksum verified at restore).
/// Required on every sealed ack post-flag-day (ato-api#217); a missing value
/// fails the ack closed the same as a missing capsule_manifest_hash would.
const SNAPSHOT_CODEC_ID: &str = "asc.raw-v1.v1";

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
        // ato#1002: the artifact-store env (ATO_ARTIFACT_S3_*) is all-or-nothing;
        // a PARTIAL set is an operator error that must stop the daemon at
        // startup, never surface per-job (process_job re-reads the same env).
        upload::ArtifactStore::from_env().map_err(|e| anyhow!(e))?;
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
    /// capsule.toml is required, fail-closed exactly as before. Always null for a
    /// `dockerfile_import` job (there is no recipe manifest to carry).
    #[serde(default)]
    recipe_toml: Option<String>,
    /// ato#1002: which build lane this job takes — `"recipe"` (default; the only
    /// pre-#1002 kind, so an older ato-api parses as it) or `"dockerfile_import"`.
    /// The daemon advertises what it supports via `supported_kinds` on the claim,
    /// so any OTHER value here is a server/daemon contract skew — fail the job
    /// closed (`claim_kind`), never guess.
    #[serde(default = "default_job_kind")]
    kind: String,
    /// ato#1002: kind-specific parameters (`dockerfile_import` only). Strict-
    /// validated server-side at enqueue AND revalidated fail-closed here before
    /// any use ([`parse_import_params`]).
    #[serde(default)]
    params: Option<serde_json::Value>,
}

fn default_job_kind() -> String {
    "recipe".into()
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
    /// ato-api Hardware Binding Layer: required on every sealed ack post-flag-day
    /// (see SNAPSHOT_FORMAT_ID / SNAPSHOT_CODEC_ID doc comments above).
    snapshot_format_id: String,
    snapshot_codec_id: String,
    // ── #932 non-secret build provenance (diagnostics; never registry identity) ──
    /// Which manifest built this artifact: "recipe_toml" | "repo_capsule_toml" |
    /// "dockerfile_import" (ato#1002 — an import has no manifest; the value names
    /// the lane).
    manifest_source: String,
    /// True when the readiness probe was synthesized from the declared port.
    synthesized_probe: bool,
    /// The manifest-declared run command, verbatim.
    declared_command: String,
    /// The command actually embedded into the guest init (post normalization).
    normalized_guest_command: String,
    /// v1.2 PR 3e-2c: SUPERVISOR artifact facts — binding NAMES only, never a
    /// value. `Some` with NON-EMPTY names ⇒ ato-api registers the row with
    /// `no_binding_required=false` (+ persists the names) and the firewall CHECK
    /// keeps it permanently non-public. ato#1002 review (D4): a dockerfile-import
    /// ack ALWAYS carries this field — with an EMPTY name set for a zero-binding
    /// import (the rootfs still runs guest-agent + supervisor), which ato-api maps
    /// to `no_binding_required=true` + NULL `binding_names_json`, so the existing
    /// publish firewall passes unchanged. Omitted entirely for a no-binding
    /// RECIPE artifact (`skip_serializing_if`), so those acks stay byte-identical
    /// against the `.strict()` ack schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    supervisor_build: Option<SupervisorAck>,
    /// ato#1002: non-secret Dockerfile-import provenance — the full
    /// `DockerImportReceipt` (importer version, digest-pinned bases, options,
    /// warnings; already secret-screened at import time). Present ONLY on a
    /// `dockerfile_import` artifact; omitted entirely for recipe builds
    /// (`skip_serializing_if`), so those acks stay byte-identical against the
    /// `.strict()` ack schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    docker_import_receipt: Option<serde_json::Value>,
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
        // ato#1002: advertise both build lanes — the server hands a job ONLY if its
        // kind is listed here (an older ato-api ignores the field and keeps handing
        // recipe jobs exactly as before).
        .send_json(ureq::json!({ "agent_id": cfg.agent_id, "capacity": 1, "supported_kinds": ["recipe", "dockerfile_import"] }))
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

/// ato#1002: everything the SHARED job tail (Ready-State build → restore-verify →
/// no-secret scan → sealed ack) needs from a producer branch. Steps 1-3 differ by
/// job kind — `recipe`: materialize + manifest + rootfs build, exactly the pre-#1002
/// pipeline; `dockerfile_import`: clone + params + Dockerfile import — and everything
/// downstream consumes only this struct, so the tail stays one code path.
#[derive(Debug)]
struct ProducedBuild {
    /// The bootable ext4 rootfs bytes.
    rootfs: Vec<u8>,
    port: u16,
    healthcheck: String,
    /// Fed to `BuildReadyStateInput.execution_id` (recipe: the declared execution
    /// id from the graph envelope; import: `import_execution_id` over the import
    /// execution envelope — ato#1002 review D3). Always the Ato EXECUTION identity
    /// (what executes), never a rebuild-inputs / job identity.
    execution_id: String,
    capsule_manifest_hash: String,
    supervisor: Option<SupervisorBindings>,
    // ── sealed-ack facts (Artifact provenance) ──
    supervisor_ack: Option<SupervisorAck>,
    manifest_source: String,
    synthesized_probe: bool,
    declared_command: String,
    normalized_guest_command: String,
    docker_import_receipt: Option<serde_json::Value>,
}

/// ato#1002 producer dispatch: `kind` selects the steps 1-3 branch. An unknown kind
/// is a server/daemon contract skew (the claim advertised `supported_kinds`) — fail
/// the job closed at `claim_kind`, never guess a lane.
fn produce_build(cfg: &Config, job: &ClaimedJob, jobdir: &Path) -> std::result::Result<ProducedBuild, (String, String)> {
    match job.kind.as_str() {
        "recipe" => produce_recipe_build(cfg, job, jobdir),
        "dockerfile_import" => produce_import_build(cfg, job, jobdir),
        other => Err((
            "claim_kind".into(),
            format!("unsupported job kind {other:?} (this builder supports: recipe, dockerfile_import)"),
        )),
    }
}

/// The pre-#1002 pipeline, steps 1-3, byte-for-byte: materialize the server-resolved
/// source, parse + gate the manifest, derive the fail-closed build spec, compute the
/// declared execution identity, and build the bootable rootfs.
fn produce_recipe_build(cfg: &Config, job: &ClaimedJob, jobdir: &Path) -> std::result::Result<ProducedBuild, (String, String)> {
    let fail = |stage: &str, e: String| (stage.to_string(), e);

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

    // v1.2 PR 3e-2c: capture the supervisor binding names for the SEALED ACK. ato-api's
    // artifactSchema now accepts an optional `supervisor_build` (3e-2), so the ack must
    // carry the names — otherwise ato-api registers the row as no-binding + PUBLIC
    // (the E2E caught exactly this). A no-binding capsule keeps the field absent, so
    // those acks stay byte-identical against the .strict() schema.
    let supervisor_ack = spec
        .supervisor
        .as_ref()
        .map(|s| SupervisorAck { binding_names: s.binding_names.clone() });
    let supervisor = spec.supervisor.as_ref().map(|s| {
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
    });

    Ok(ProducedBuild {
        rootfs,
        port: spec.port,
        healthcheck: spec.healthcheck.clone(),
        execution_id: declared_execution_id,
        capsule_manifest_hash: format!("blake3:{}", blake3::hash(&toml_bytes).to_hex()),
        supervisor,
        supervisor_ack,
        manifest_source: manifest_source.to_string(),
        synthesized_probe: spec.probe_synthesized,
        declared_command: spec.declared_start_cmd,
        normalized_guest_command: spec.start_cmd,
        docker_import_receipt: None,
    })
}

/// ato#1002 `dockerfile_import` producer: validate the job params fail-closed, clone
/// the server-resolved pinned commit WITHOUT a capsule.toml (an import candidate by
/// definition has none — this deliberately does not go through `materialize_source`,
/// whose manifest gate stays intact for recipe jobs), then run the v1.7 Dockerfile
/// import (secret policy fixed to `Reject`: the Store job shape carries no secret
/// conversion opt-in) and hand the packed ext4 to the SAME steps 4-7 as a recipe job.
fn produce_import_build(cfg: &Config, job: &ClaimedJob, jobdir: &Path) -> std::result::Result<ProducedBuild, (String, String)> {
    let fail = |stage: &str, e: String| (stage.to_string(), e);

    // 1. Strict params validation BEFORE any network/build work (same bounds as the
    // ato-api enqueue validation; a violation here means the server-side gate was
    // bypassed or skewed — fail closed at eligibility).
    let params = parse_import_params(job.params.as_ref()).map_err(|e| fail("eligibility", e))?;

    // 2. Clone the SERVER-RESOLVED pinned commit (identity/subdir validated; no
    // capsule.toml requirement).
    let src = clone_pinned_source(&job.source, &jobdir.join("src")).map_err(|e| fail("source", e))?;

    // 3. Run the Dockerfile import: probe tool → digest-pinned build → service plan →
    // pack the imported image into a bootable supervisor ext4. DockerImportSpec::new
    // revalidates the Dockerfile path (containment discipline, defense in depth).
    let spec = DockerImportSpec::new(&params.dockerfile_path, BTreeMap::new()).map_err(|e| fail("eligibility", e))?;
    let ext4 = jobdir.join("rootfs.ext4");
    // The ephemeral image tag must be a valid container reference — job ids are
    // sanitized (the import's pack script removes the tag after export).
    let tag_suffix: String = job
        .id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .take(64)
        .collect();
    let req = DockerfileImportRequest {
        context_dir: &src,
        spec,
        policy: SecretEnvPolicy::Reject,
        port_override: params.port_override,
        readiness_http_path: params.readiness_http_path.clone(),
        volume_policy: params.volumes,
        host_bind_relay: params.host_bind_relay,
        image_tag: format!("ato-import-{tag_suffix}"),
        out_ext4: &ext4,
        size_mib: cfg.rootfs_size_mib,
    };
    let outcome = run_dockerfile_import(&SystemImportCommandRunner, &req).map_err(|e| fail("rootfs_build", e))?;
    let rootfs = std::fs::read(&ext4).map_err(|e| fail("rootfs_build", e.to_string()))?;

    // Import identity (ato#1002 review D3): execution_id = the import EXECUTION
    // identity — what executes (the derived service argv/cwd/env/port/readiness +
    // platform + final image digest), aligned in meaning with the recipe path's
    // declared_execution_id; host-independent, never a job id / timestamp. The
    // REBUILD-INPUTS identity (import_identity_digest) deliberately does NOT
    // become the execution id — it lives only inside the docker_import_receipt /
    // import descriptor lane. capsule_manifest_hash = the blake3 import
    // DESCRIPTOR hash over that input-only envelope (an import has no
    // capsule.toml — a descriptor hash, not a manifest hash).
    let execution_id = import_execution_id(&outcome.plan, &outcome.receipt);
    let capsule_manifest_hash = import_descriptor_blake3(&outcome.receipt);
    let docker_import_receipt = serde_json::to_value(&outcome.receipt)
        .map_err(|e| fail("artifact_metadata", format!("serialize docker import receipt: {e}")))?;

    // v0 imports emit exactly ONE public service; its argv (ENTRYPOINT+CMD, exec
    // form) lands in supervisor.json verbatim — no sh -lc normalization — so the
    // declared and guest commands are the same string (diagnostics only).
    let argv_display = outcome
        .plan
        .supervisor
        .services
        .as_ref()
        .and_then(|s| s.first())
        .map(|s| s.cmd.join(" "))
        .unwrap_or_default();

    // ato#1002 review (D4): a dockerfile import is a SUPERVISOR artifact end to
    // end — the packed rootfs runs guest-agent + supervisor even with ZERO
    // bindings (ato#1001 starts the vacuously bound-ready workload at boot, so
    // the backend's health wait passes with nothing delivered, and the backend
    // accepts the empty set: no placeholder protocol, sealed per the no-binding
    // contract). The sealed ack therefore ALWAYS carries supervisor_build for an
    // import (binding_names may be []) so ato-api records honest supervisor
    // provenance; server-side an EMPTY set still registers as
    // no_binding_required=true with NULL binding_names_json, keeping the publish
    // firewall unchanged. Under the fixed Reject policy the set is always empty
    // today; the mapping stays general for a future with-bindings job shape.
    let binding_names = outcome.plan.supervisor.binding_names.clone();
    let supervisor = Some(SupervisorBindings {
        binding_names: binding_names.clone(),
        state_volumes: vec![],
        state_owner_scope: None,
    });
    let supervisor_ack = Some(SupervisorAck { binding_names });

    Ok(ProducedBuild {
        rootfs,
        port: outcome.plan.port,
        healthcheck: outcome.plan.readiness_http_path.clone().unwrap_or_else(|| "/".to_string()),
        execution_id,
        capsule_manifest_hash,
        supervisor,
        supervisor_ack,
        manifest_source: "dockerfile_import".to_string(),
        synthesized_probe: outcome.plan.readiness_http_path.is_none(),
        declared_command: argv_display.clone(),
        normalized_guest_command: argv_display,
        docker_import_receipt: Some(docker_import_receipt),
    })
}

/// ato#1002: validated `dockerfile_import` job params (defaults applied).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DockerfileImportParams {
    dockerfile_path: String,
    port_override: Option<u16>,
    readiness_http_path: Option<String>,
    /// ato#1024: `"tmpfs"` opts in to mapping image-declared VOLUMEs to guest
    /// tmpfs (ephemeral by design); absent keeps the ato#983 fail-closed gate.
    volumes: snapshot::docker_import::VolumePolicy,
    /// ato#1026: `true` opts in to the localhost→guest-IP relay for apps that
    /// bind 127.0.0.1 inside the guest (default off).
    host_bind_relay: bool,
}

impl Default for DockerfileImportParams {
    fn default() -> Self {
        DockerfileImportParams {
            dockerfile_path: "Dockerfile".into(),
            port_override: None,
            readiness_http_path: None,
            volumes: snapshot::docker_import::VolumePolicy::Reject,
            host_bind_relay: false,
        }
    }
}

/// Strict, fail-closed parse of `dockerfile_import` params — the same bounds the
/// ato-api enqueue validation enforces (ato#1002): `dockerfile_path` relative, no
/// `..` component, ≤200 chars (default `"Dockerfile"`); `port_override` an integer
/// in 1..65535; `readiness_http_path` starting `/`, ≤200 chars, single-line.
/// Unknown keys and non-object params are rejected; absent/null params mean all
/// defaults.
///
/// The readiness bound is 200 — NOT the contract draft's 256 — because the value
/// is acked verbatim as `healthcheck_url_path`, which ato-api's strict artifact
/// schema caps at 200; a longer path would build an artifact whose sealed ack can
/// never validate (rebuilt forever on claim expiry). And it must be single-line
/// ([`reject_control_chars`]) because it is interpolated into the builder-host
/// pack script — a newline would break out of its `#` comment and execute as
/// root on the builder.
fn parse_import_params(params: Option<&serde_json::Value>) -> std::result::Result<DockerfileImportParams, String> {
    let mut out = DockerfileImportParams::default();
    let Some(v) = params.filter(|v| !v.is_null()) else { return Ok(out) };
    let obj = v.as_object().ok_or("dockerfile_import params must be a JSON object")?;
    for (key, val) in obj {
        match key.as_str() {
            "dockerfile_path" => {
                let p = val.as_str().ok_or("params.dockerfile_path must be a string")?;
                if p.chars().count() > 200 {
                    return Err("params.dockerfile_path exceeds 200 characters".into());
                }
                if p.starts_with('/') {
                    return Err("params.dockerfile_path must be relative (no leading '/')".into());
                }
                // Full containment discipline (empty, absolute, `..`, prefix) — the
                // same gate DockerImportSpec::new re-applies later.
                validate_dockerfile_path(p)?;
                out.dockerfile_path = p.to_string();
            }
            "port_override" => {
                let n = val
                    .as_u64()
                    .filter(|n| (1..=65535).contains(n))
                    .ok_or("params.port_override must be an integer in 1..65535")?;
                out.port_override = Some(n as u16);
            }
            "readiness_http_path" => {
                let p = val.as_str().ok_or("params.readiness_http_path must be a string")?;
                if !p.starts_with('/') {
                    return Err("params.readiness_http_path must start with '/'".into());
                }
                if p.chars().count() > 200 {
                    return Err("params.readiness_http_path exceeds 200 characters".into());
                }
                // Interpolated into the builder-host pack script (`{hc}` inside a
                // `#` comment) — the same NUL/newline gate rootfs_builder applies
                // to run/build commands, or a newline runs as root on the builder.
                reject_control_chars("params.readiness_http_path", p)?;
                out.readiness_http_path = Some(p.to_string());
            }
            "volumes" => {
                // ato#1024: only the literal "tmpfs" opts in; anything else is
                // rejected rather than ignored (a typo must not silently keep
                // the fail-closed VOLUME gate the caller thought they lifted —
                // or worse, lift a gate they didn't mean to).
                match val.as_str() {
                    Some("tmpfs") => out.volumes = snapshot::docker_import::VolumePolicy::Tmpfs,
                    _ => return Err("params.volumes must be the string \"tmpfs\" (the only supported mapping)".into()),
                }
            }
            "host_bind_relay" => {
                // ato#1026: strictly a bool — a non-bool must not be silently
                // treated as truthy/falsy.
                out.host_bind_relay = val
                    .as_bool()
                    .ok_or("params.host_bind_relay must be a boolean")?;
            }
            other => return Err(format!("unknown dockerfile_import param {other:?} (rejected fail-closed)")),
        }
    }
    Ok(out)
}

/// ato#1002: shallow-clone the SERVER-RESOLVED pinned commit for a
/// `dockerfile_import` job. Mirrors `materialize_source`'s identity validation +
/// subdir containment (lexical + canonical) but deliberately WITHOUT its
/// capsule.toml gate — an import candidate by definition carries none (the same
/// reasoning as `docker_import_kvm_smoke`'s `clone_pinned`). `materialize_source`
/// keeps its manifest gate untouched for recipe jobs.
fn clone_pinned_source(source: &ClaimedSource, dest: &Path) -> std::result::Result<PathBuf, String> {
    if !valid_github_owner(&source.github_owner) {
        return Err(format!("invalid github owner {:?}", source.github_owner));
    }
    if !valid_github_repo(&source.github_repo) {
        return Err(format!("invalid github repo {:?}", source.github_repo));
    }
    let commit = source.commit_sha.as_str();
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("refusing non-pinned commit {commit:?} (need a full 40-char sha)"));
    }
    let sub = source.subdirectory.as_deref().filter(|s| !s.is_empty());
    if let Some(s) = sub {
        // Lexical containment first (relative, no `..`, no prefix) — the same rule
        // materialize_source applies, via the docker_import path validator.
        validate_dockerfile_path(s).map_err(|e| format!("invalid subdirectory: {e}"))?;
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let run = |args: &[&str]| -> std::result::Result<(), String> {
        let out = Command::new("git").args(args).current_dir(dest).output().map_err(|e| format!("git {args:?}: {e}"))?;
        if !out.status.success() {
            return Err(format!("git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        Ok(())
    };
    run(&["init", "-q"])?;
    run(&["remote", "add", "origin", &format!("https://github.com/{}/{}.git", source.github_owner, source.github_repo)])?;
    run(&["fetch", "-q", "--depth", "1", "origin", commit])?;
    run(&["checkout", "-q", "FETCH_HEAD"])?;

    // Canonical containment after checkout (closes symlink traversal), exactly like
    // materialize_source's contained_source_root — minus the manifest requirement.
    let root = match sub {
        Some(s) => dest.join(s),
        None => dest.to_path_buf(),
    };
    let dest_canon = dest.canonicalize().map_err(|e| format!("canonicalize checkout: {e}"))?;
    let root_canon = root.canonicalize().map_err(|e| format!("resolved source root {} not found: {e}", root.display()))?;
    if !root_canon.starts_with(&dest_canon) {
        return Err(format!("subdirectory escapes the checkout: {} is outside {}", root_canon.display(), dest_canon.display()));
    }
    Ok(root_canon)
}

/// Build + seal + verify one claimed job. Returns the non-secret artifact metadata on
/// success, or `(failure_stage, failure_reason)` — never a panic, never a secret.
fn process_job(cfg: &Config, backend: &FirecrackerBackend, job: &ClaimedJob) -> std::result::Result<Artifact, (String, String)> {
    let fail = |stage: &str, e: String| (stage.to_string(), e);
    let jobdir = cfg.work.join(&job.id);
    let _ = std::fs::remove_dir_all(&jobdir);

    // Steps 1-3 branch by job kind (ato#1002): recipe = materialize + manifest +
    // rootfs build (pre-#1002, byte-for-byte); dockerfile_import = clone + params +
    // Dockerfile import. Steps 4-7 below are SHARED and unchanged.
    let produced = produce_build(cfg, job, &jobdir)?;

    // 4. Ready-State build: boot → verify healthcheck → snapshot → seal (no UFFD). For
    // a supervisor spec the backend drives the whole placeholder protocol itself
    // (deliver → health → StopWorkload → Revoke → seal, #962); the daemon only passes
    // the binding NAMES — no secret value exists anywhere in this process. A
    // ZERO-binding supervisor build (dockerfile import, ato#1002 D4) has no
    // placeholder protocol: the workload starts at boot (vacuously bound-ready,
    // ato#1001) and the artifact seals per the no-binding contract.
    let store = CasStore::open(jobdir.join("cas")).map_err(|e| fail("build_ready_state", e.to_string()))?;
    let receipt = backend
        .build_ready_state(BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: produced.capsule_manifest_hash.clone(),
            runner_class: None,
            layers: BuildLayers { rootfs: produced.rootfs, runtime: None, dependency: None, app: None, vmstate: Vec::new(), memory: Vec::new() },
            restore_contract: RestoreContract { ports: vec![produced.port], healthcheck: Some(produced.healthcheck.clone()), expected_ready_ms: Some(8000) },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: vec![],
            execution_id: Some(produced.execution_id),
            supervisor: produced.supervisor,
        })
        .map_err(|e| fail("build_ready_state", e.to_string()))?;
    let manifest_out = receipt.manifest.clone();

    // 5. Verify the sealed artifact RESTORES before we call it sealed (no traffic
    // exposed). A supervisor artifact WITH required bindings restore-verifies via
    // the backend's agent probe (reachable + NOT bound-ready, #962) — no health
    // wait, no binding needed; a ZERO-binding supervisor artifact (import) sealed
    // its workload RUNNING, so the backend health-waits like any no-binding
    // artifact (ato#1002 D4).
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
    let artifact_location = upload::cas_location(&job.id, &artifact_manifest_hash);

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

    // 8. ato#1002 Snapshot Serving v1: with the artifact store configured (all
    // four ATO_ARTIFACT_S3_* vars — validated all-or-nothing at startup),
    // package {manifest.json, cas/} into one artifact.tar.gz and upload it
    // BEFORE the sealed ack; the registered location then names the remote
    // store ("r2://<bucket>/<job_id>/<artifact_manifest_hash>"). Upload failure
    // ⇒ failed ack at artifact_upload — never sealed-without-bytes. Absent
    // config keeps v1 byte-identical: the same-host cas:// location above, no
    // packing, no upload.
    let artifact_location = match upload::ArtifactStore::from_env().map_err(|e| fail("artifact_upload", e))? {
        Some(store) => store
            .pack_and_upload(&upload::SystemImportCommandRunner, &jobdir, &job.id, &artifact_manifest_hash)
            .map_err(|e| fail("artifact_upload", e))?,
        None => artifact_location,
    };

    Ok(Artifact {
        capsule_manifest_hash: produced.capsule_manifest_hash,
        execution_id,
        artifact_manifest_hash,
        runner_class_id,
        snapshot_backend: manifest_out.snapshot_backend.kind.clone(),
        artifact_location,
        healthcheck_url_path: produced.healthcheck,
        no_secret_scan_clean: true,
        rootfs_bytes: manifest_out.layers.rootfs.as_ref().map(|m| m.total_len).unwrap_or(0),
        mem_bytes: manifest_out.layers.memory.as_ref().map(|m| m.total_len).unwrap_or(0),
        vmstate_bytes: manifest_out.layers.vmstate.as_ref().map(|m| m.total_len).unwrap_or(0),
        snapshot_format_id: SNAPSHOT_FORMAT_ID.to_string(),
        snapshot_codec_id: SNAPSHOT_CODEC_ID.to_string(),
        // #932 build provenance — lands in receipt_json via the sealed ack (diagnostics
        // only; the ato-api registry identity comparison never reads these).
        manifest_source: produced.manifest_source,
        synthesized_probe: produced.synthesized_probe,
        declared_command: produced.declared_command,
        normalized_guest_command: produced.normalized_guest_command,
        supervisor_build: produced.supervisor_ack,
        docker_import_receipt: produced.docker_import_receipt,
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
        // ato#1002: an OLDER ato-api without kind/params parses as the recipe lane.
        assert_eq!(resp.jobs[0].kind, "recipe");
        assert!(resp.jobs[0].params.is_none());
    }

    #[test]
    fn parses_a_dockerfile_import_claim() {
        // ato#1002: an import job carries kind + params (and a null recipe_toml).
        let body = serde_json::json!({
            "jobs": [{
                "id": "job_4", "capsule_id": "cap_4",
                "source": { "source_kind": "github", "github_owner": "acme", "github_repo": "app", "commit_sha": "d".repeat(40), "subdirectory": null },
                "recipe_toml": null,
                "kind": "dockerfile_import",
                "params": { "dockerfile_path": "docker/prod.Dockerfile", "port_override": 8080 },
                "target_label": "app", "profile": "default", "claim_expires_at": "2026-01-01T00:00:00.000Z"
            }]
        });
        let resp: ClaimResponse = serde_json::from_value(body).unwrap();
        assert_eq!(resp.jobs[0].kind, "dockerfile_import");
        assert!(resp.jobs[0].recipe_toml.is_none());
        let params = parse_import_params(resp.jobs[0].params.as_ref()).unwrap();
        assert_eq!(params.dockerfile_path, "docker/prod.Dockerfile");
        assert_eq!(params.port_override, Some(8080));
        assert!(params.readiness_http_path.is_none());
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

    // ── ato#1002: producer branch selection + import params ──────────────────

    fn test_cfg() -> Config {
        Config {
            api_url: "https://api".into(),
            token: "t".into(),
            agent_id: "a".into(),
            work: std::env::temp_dir(),
            rootfs_size_mib: 1024,
            once: true,
            poll_secs: 15,
        }
    }

    fn import_job(kind: &str, params: Option<serde_json::Value>) -> ClaimedJob {
        ClaimedJob {
            id: "job_x".into(),
            capsule_id: "cap_x".into(),
            target_label: "app".into(),
            profile: "default".into(),
            source: ClaimedSource {
                github_owner: "acme".into(),
                github_repo: "app".into(),
                commit_sha: "a".repeat(40),
                subdirectory: None,
            },
            recipe_toml: None,
            kind: kind.into(),
            params,
        }
    }

    #[test]
    fn unknown_job_kind_fails_closed_at_claim_kind() {
        // Server/daemon contract skew (the claim advertised supported_kinds): an
        // unknown kind never guesses a lane — ack failed at stage claim_kind.
        let err = produce_build(&test_cfg(), &import_job("oci_image", None), Path::new("/nonexistent")).unwrap_err();
        assert_eq!(err.0, "claim_kind");
        assert!(err.1.contains("oci_image"), "{}", err.1);
    }

    #[test]
    fn import_params_validation_failures_fail_at_eligibility() {
        // Params are validated BEFORE any clone/build work, so a bad-params job
        // acks failed at the eligibility stage without touching the network.
        for bad in [
            serde_json::json!({ "port_override": 0 }),
            serde_json::json!({ "dockerfile_path": "../evil" }),
            serde_json::json!({ "unknown_key": true }),
            serde_json::json!("not-an-object"),
        ] {
            let err = produce_build(&test_cfg(), &import_job("dockerfile_import", Some(bad.clone())), Path::new("/nonexistent")).unwrap_err();
            assert_eq!(err.0, "eligibility", "{bad}");
        }
    }

    #[test]
    fn import_params_parse_defaults_and_full_shape() {
        // Absent or null ⇒ all defaults (dockerfile_path = "Dockerfile").
        assert_eq!(parse_import_params(None).unwrap(), DockerfileImportParams::default());
        assert_eq!(parse_import_params(Some(&serde_json::Value::Null)).unwrap(), DockerfileImportParams::default());
        assert_eq!(parse_import_params(None).unwrap().dockerfile_path, "Dockerfile");
        // Full object parses with the bounds applied (65535 is a legal port).
        let v = serde_json::json!({
            "dockerfile_path": "docker/app.Dockerfile",
            "port_override": 65535,
            "readiness_http_path": "/healthz",
        });
        let p = parse_import_params(Some(&v)).unwrap();
        assert_eq!(p.dockerfile_path, "docker/app.Dockerfile");
        assert_eq!(p.port_override, Some(65535));
        assert_eq!(p.readiness_http_path.as_deref(), Some("/healthz"));
        // Boundary: exactly 200 chars is legal (the ack schema's healthcheck_url_path max).
        let max = format!("/{}", "x".repeat(199));
        let v = serde_json::json!({ "readiness_http_path": max.as_str() });
        assert_eq!(parse_import_params(Some(&v)).unwrap().readiness_http_path.as_deref(), Some(max.as_str()));
        // ato#1024: only the literal "tmpfs" engages the VOLUME mapping; any other
        // value is rejected, never silently ignored.
        let v = serde_json::json!({ "volumes": "tmpfs" });
        assert_eq!(parse_import_params(Some(&v)).unwrap().volumes, snapshot::docker_import::VolumePolicy::Tmpfs);
        assert_eq!(parse_import_params(None).unwrap().volumes, snapshot::docker_import::VolumePolicy::Reject);
        for bad in [serde_json::json!({"volumes": "rw"}), serde_json::json!({"volumes": true}), serde_json::json!({"volumes": null})] {
            assert!(parse_import_params(Some(&bad)).unwrap_err().contains("volumes"));
        }
        // ato#1026: host_bind_relay is a strict bool.
        assert!(parse_import_params(Some(&serde_json::json!({"host_bind_relay": true}))).unwrap().host_bind_relay);
        assert!(!parse_import_params(None).unwrap().host_bind_relay);
        for bad in [serde_json::json!({"host_bind_relay": "yes"}), serde_json::json!({"host_bind_relay": 1})] {
            assert!(parse_import_params(Some(&bad)).unwrap_err().contains("host_bind_relay"));
        }
    }

    #[test]
    fn import_params_reject_every_out_of_bounds_shape() {
        // The same strict bounds the ato-api enqueue validation enforces (ato#1002):
        // unknown keys, non-object params, path escape/length, port range/type,
        // readiness shape/length — each rejected with an actionable reason.
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (serde_json::json!({ "extra": 1 }), "unknown"),
            (serde_json::json!({ "dockerfile_path": "/abs/Dockerfile" }), "relative"),
            (serde_json::json!({ "dockerfile_path": "a/../../Dockerfile" }), ".."),
            (serde_json::json!({ "dockerfile_path": "x".repeat(201) }), "200"),
            (serde_json::json!({ "dockerfile_path": 7 }), "string"),
            (serde_json::json!({ "port_override": 0 }), "1..65535"),
            (serde_json::json!({ "port_override": 65536 }), "1..65535"),
            (serde_json::json!({ "port_override": "8080" }), "integer"),
            (serde_json::json!({ "port_override": 8080.5 }), "integer"),
            (serde_json::json!({ "readiness_http_path": "health" }), "start with '/'"),
            // 200, not the contract draft's 256: the ack's healthcheck_url_path
            // schema (ato-api, strict) caps at 200 — see parse_import_params.
            (serde_json::json!({ "readiness_http_path": format!("/{}", "x".repeat(200)) }), "200"),
            (serde_json::json!({ "readiness_http_path": 1 }), "string"),
            // Shell-injection gate: the value lands in the builder-host pack
            // script, so NUL/CR/LF are rejected fail-closed (reject_control_chars).
            (serde_json::json!({ "readiness_http_path": "/x\nid > /tmp/pwned\n#" }), "newline"),
            (serde_json::json!({ "readiness_http_path": "/x\rid" }), "newline"),
            (serde_json::json!({ "readiness_http_path": "/x\u{0}y" }), "NUL"),
            (serde_json::json!([1, 2]), "object"),
        ];
        for (v, needle) in cases {
            let err = parse_import_params(Some(&v)).unwrap_err();
            assert!(err.contains(needle), "{v}: {err}");
        }
    }

    #[test]
    fn import_clone_rejects_invalid_identities_before_any_git() {
        let src = |owner: &str, repo: &str, commit: &str, sub: Option<&str>| ClaimedSource {
            github_owner: owner.into(),
            github_repo: repo.into(),
            commit_sha: commit.into(),
            subdirectory: sub.map(String::from),
        };
        let dest = std::env::temp_dir().join(format!("never-created-clone-dest-{}", std::process::id()));
        let full = "a".repeat(40);
        // Bad owner / repo / non-pinned commit / escaping subdir — each fails BEFORE
        // any git command or directory creation (same gates as materialize_source;
        // only the capsule.toml requirement is deliberately absent).
        assert!(clone_pinned_source(&src("bad owner", "app", &full, None), &dest).unwrap_err().contains("owner"));
        assert!(clone_pinned_source(&src("acme", "bad repo", &full, None), &dest).unwrap_err().contains("repo"));
        assert!(clone_pinned_source(&src("acme", "app", "main", None), &dest).unwrap_err().contains("non-pinned"));
        assert!(clone_pinned_source(&src("acme", "app", &full[..12], None), &dest).unwrap_err().contains("non-pinned"));
        let err = clone_pinned_source(&src("acme", "app", &full, Some("../up")), &dest).unwrap_err();
        assert!(err.contains("subdirectory"), "{err}");
        assert!(!dest.exists(), "validation must reject before any clone IO");
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
            snapshot_format_id: SNAPSHOT_FORMAT_ID.to_string(),
            snapshot_codec_id: SNAPSHOT_CODEC_ID.to_string(),
            supervisor_build: None,
            docker_import_receipt: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        // A NO-BINDING recipe ack omits supervisor_build AND docker_import_receipt
        // entirely (byte-identical vs the pre-3e-2c / pre-#1002 schema, which the
        // .strict() ato-api validator requires).
        assert_eq!(
            keys,
            [
                "artifact_location", "artifact_manifest_hash", "capsule_manifest_hash", "declared_command", "execution_id",
                "healthcheck_url_path", "manifest_source", "mem_bytes", "no_secret_scan_clean", "normalized_guest_command",
                "rootfs_bytes", "runner_class_id", "snapshot_backend", "snapshot_codec_id", "snapshot_format_id",
                "synthesized_probe", "vmstate_bytes"
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
    fn dockerfile_import_ack_carries_the_receipt_and_the_new_manifest_source() {
        // ato#1002: an import ack adds docker_import_receipt (an arbitrary
        // non-secret JSON object) and manifest_source = "dockerfile_import";
        // both are OPTIONAL server-side, so old recipe acks keep validating.
        // Review D4: an import ack ALWAYS carries supervisor_build too — a
        // zero-binding import is still a supervisor artifact, acked with an
        // EMPTY name set (ato-api maps [] to no_binding_required=true + NULL
        // binding_names_json, so the publish firewall passes unchanged).
        let a = Artifact {
            capsule_manifest_hash: "blake3:d".into(),
            execution_id: "sha256:i".into(),
            artifact_manifest_hash: "blake3:a".into(),
            runner_class_id: "rc".into(),
            snapshot_backend: "firecracker".into(),
            artifact_location: "cas://job/blake3:a".into(),
            healthcheck_url_path: "/".into(),
            no_secret_scan_clean: true,
            rootfs_bytes: 1,
            mem_bytes: 2,
            vmstate_bytes: 3,
            manifest_source: "dockerfile_import".into(),
            synthesized_probe: true,
            declared_command: "docker-entrypoint.sh node server.js".into(),
            normalized_guest_command: "docker-entrypoint.sh node server.js".into(),
            snapshot_format_id: SNAPSHOT_FORMAT_ID.to_string(),
            snapshot_codec_id: SNAPSHOT_CODEC_ID.to_string(),
            supervisor_build: Some(SupervisorAck { binding_names: vec![] }),
            docker_import_receipt: Some(serde_json::json!({
                "importer_version": "ato-docker-import/0.1.0",
                "build_tool": "podman",
                "resolved_base_images": [{ "original_ref": "node:20", "resolved_digest": "docker.io/library/node@sha256:ab" }],
            })),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["manifest_source"], "dockerfile_import");
        assert!(v["docker_import_receipt"].is_object());
        assert_eq!(v["docker_import_receipt"]["build_tool"], "podman");
        // The zero-binding supervisor facet serializes as an EXPLICIT empty set —
        // present, never omitted, never null.
        assert_eq!(v["supervisor_build"], serde_json::json!({ "binding_names": [] }));
        // The keys are present ONLY when Some — a recipe ack (None) never carries
        // docker_import_receipt.
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        assert!(keys.contains(&"docker_import_receipt"));
        assert!(keys.contains(&"supervisor_build"));
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
            snapshot_format_id: SNAPSHOT_FORMAT_ID.to_string(),
            snapshot_codec_id: SNAPSHOT_CODEC_ID.to_string(),
            supervisor_build: Some(SupervisorAck { binding_names: vec!["openai_api_key".into()] }),
            docker_import_receipt: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(
            v["supervisor_build"],
            serde_json::json!({ "binding_names": ["openai_api_key"] })
        );
        // Still no secret value anywhere in the ack.
        assert!(!serde_json::to_string(&v).unwrap().to_lowercase().contains("sk-"));
    }

    #[test]
    fn sealed_ack_always_carries_snapshot_format_and_codec_ids_matching_ato_api_charsets() {
        // ato-api Hardware Binding Layer flag-day (ato-api#217): snapshot_format_id
        // / snapshot_codec_id are REQUIRED on every sealed ack — never optional,
        // never omitted (unlike supervisor_build/docker_import_receipt). A missing
        // value fails ato-api's ack closed the same as a missing
        // capsule_manifest_hash would. This is a regression test for exactly that:
        // both fields must always serialize, with the exact values ato-api's
        // hwc./asf./asc. anchored regexes expect.
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
            snapshot_format_id: SNAPSHOT_FORMAT_ID.to_string(),
            snapshot_codec_id: SNAPSHOT_CODEC_ID.to_string(),
            supervisor_build: None,
            docker_import_receipt: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["snapshot_format_id"], "asf.fc-memsnap-v1");
        assert_eq!(v["snapshot_codec_id"], "asc.raw-v1.v1");
        // ato-api anchors these to /^asf\.[a-z0-9_.-]{1,124}$/ and
        // /^asc\.[a-z0-9_.-]{1,124}$/ respectively (LABEL_RE charset minus the
        // namespace prefix) — check the same charset here without pulling in a
        // regex dependency for one test.
        fn matches_ato_api_label_charset(prefix: &str, value: &str) -> bool {
            let Some(rest) = value.strip_prefix(prefix) else {
                return false;
            };
            !rest.is_empty()
                && rest.len() <= 124
                && rest
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
        }
        assert!(matches_ato_api_label_charset("asf.", v["snapshot_format_id"].as_str().unwrap()));
        assert!(matches_ato_api_label_charset("asc.", v["snapshot_codec_id"].as_str().unwrap()));
    }
}
