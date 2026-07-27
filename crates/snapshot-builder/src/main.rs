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
//! AND `ATO_FC_VSOCK=1` are set; otherwise secret capsules keep failing closed at
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

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use capsule::engine::execution_graph::{
    ReadyStateDeclaredEnvelope, declared_dependencies_from_manifest_toml, store_source_identifier,
};
use capsule::execution_contract::{
    ContentDigest, DigestAlgorithm, ExecutionContractEnvelopeV1, ExecutionContractV1,
};
use capsule::execution_contract_finalize::{ExecutionObservationV1, FinalizationError};
use capsule::foundation::blob::{
    SourceMaterializeError, materialize_source_archive, materialized_source_tree_hash,
};
use capsule::foundation::types::manifest::{CapsuleManifest, SessionSurfaceRequirement};
use capsule::foundation::types::ready_state::{
    DEFAULT_STABLE_INTERVAL_MS, DEFAULT_STABLE_SUCCESSES,
};
use capsulefs::CasStore;
use protocol::session_surface::{
    EndpointContract, EndpointExposure, EndpointProtocol, EndpointReadiness, EndpointRole,
    PIXEL_STREAM_PROFILE, SessionSurfaceKind,
};
use serde::{Deserialize, Serialize};
use snapshot::docker_import::build::SystemImportCommandRunner;
use snapshot::docker_import::{
    DockerImportSpec, DockerfileImportRequest, EphemeralMountSeed, EphemeralMountSource,
    EphemeralMountSpec, EphemeralSeedFile, OciImageImportRequest, SecretEnvPolicy, VolumePolicy,
    import_descriptor_blake3, import_execution_id, oci_import_descriptor_blake3,
    oci_import_execution_id, run_dockerfile_import, run_oci_image_import, validate_dockerfile_path,
    validate_ephemeral_mounts, validate_image_ref,
};
use snapshot::rootfs_builder::{
    RootfsBuildSpec, SourceProbe, build_rootfs, checkout_source_tree, derive_build_spec,
    derive_supervisor_build_spec, materialize_source, reject_control_chars, valid_github_owner,
    valid_github_repo,
};
use snapshot::state_volume::DurableVolumeSpec;
use snapshot::{
    BuildLayers, BuildReadyStateInput, FirecrackerBackend, ReadyStateManifest, RestoreContract,
    RestoreReadyStateInput, SanitizerContract, SnapshotBackend, SupervisorBindings, WarmupRecipe,
    no_secret_scan,
};

/// Submission Wizard PR-2 (slice 3) — eligibility for a running capture, minted
/// from the contract the control plane pinned on the claim. Its module doc states
/// exactly which guarantee that is, and which it deliberately is not.
mod claim_eligibility;
/// Submission Wizard PR-2 (slice 3) — the production [`CaptureAction`]: pause,
/// snapshot, resume and seal a candidate from a live held guest, then persist and
/// upload it through the same path a built artifact takes.
mod guest_capture;
/// Submission Wizard PR-2 (slice 3) — the local TCP relay that fronts a held
/// guest so the operator-registered ingress origin has something to reach.
mod hold_ingress;
/// Submission Wizard PR-2 (slice 1) — the pure, KVM-free HOLD-phase orchestration
/// (hold → capture → #1088 accept). Driven by injected seams; unreachable in prod
/// (the interactive_capture kind is never advertised on the claim). See its doc.
mod hold_phase;
/// Getting the frozen source archive off this builder's disk: authorize one
/// upload with the API, PUT it, and report the object key the API derived. The
/// builder holds no storage credential and never names the object.
/// Bringing a pinned source archive down and PROVING it before anything builds
/// from it. The states are types, so an unverified path cannot reach the build.
mod source_archive_download;
mod source_archive_upload;
mod upload;
/// ADR-015 slice 7A — the gate a v1 `ato build` output passes before any later
/// phase may act on it: trusted-load the lock, re-derive the Execution
/// Identity, refuse every facet outside the §7 subset, and bind the contract to
/// the artifact actually on disk. Verification only; boots nothing.
///
/// Not yet called from the claim loop, and deliberately so: no ato-api
/// deployment hands this daemon a lock-plus-receipt today (the claim carries a
/// contract pinned by the control plane — see `ClaimedJob::execution_contract`).
/// Slice 7B introduces the caller, which takes a `VerifiedV1BuildInput` and
/// nothing else. Wiring it into a lane that cannot supply the inputs would make
/// the gate look exercised when it is not, so it is `dead_code` until the
/// producer side reaches this daemon — the same treatment `uffd_page_server`
/// carries in `snapshot`. Its own tests exercise every path below.
#[allow(dead_code)]
mod v1_intake;
/// Submission Wizard PR-2 (slice 2) — the builder-lane api client: the
/// FENCING-4 transport split, the lease-renew driver, and the production
/// control-poll [`ControlSource`](hold_phase::ControlSource). Transport only;
/// no live guest. See its doc.
mod wizard_api;
/// Submission Wizard PR-0 wire contract — serde/TOML types + tests only, nothing
/// wired into the claim/dispatch loop yet (see the module doc).
mod wizard_wire;

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
    /// The interactive HOLD's slot, when this daemon is configured to serve one.
    ///
    /// `None` ⇒ this builder does not advertise `interactive_capture` at all
    /// (see [`supported_job_kinds`]). The lane needs a local port to relay from
    /// AND the operator-registered `(builder_id, slot_id)` that names the https
    /// origin fronting it — a builder with only some of that could claim a hold
    /// it cannot make reachable, so all three arrive together or not at all.
    hold_slot: Option<HoldSlotConfig>,
}

/// One interactive-hold slot this daemon can serve.
///
/// Read by the job-loop arm that boots a hold (next commit); parsed and
/// validated now because it is what decides whether the lane is advertised at
/// all, and that decision must be right from startup.
#[derive(Clone)]
#[allow(dead_code)]
struct HoldSlotConfig {
    /// Matches the `builder_id` the operator registered with ato-api.
    builder_id: String,
    /// Matches the `slot_id` the operator registered with ato-api.
    slot_id: String,
    /// Where the relay listens — the local port the registered origin proxies to.
    proxy_listen: std::net::SocketAddr,
}

/// Parse the three hold-slot flags, all-or-nothing.
///
/// A partial set is an operator error, not a degraded mode: a builder that
/// advertised `interactive_capture` without a registered slot would claim holds
/// it could never make reachable, and every one of them would burn a full build
/// before failing at `hold-ready`. So all three or none, checked at startup.
fn hold_slot_from(flag: &dyn Fn(&str) -> Option<String>) -> Result<Option<HoldSlotConfig>> {
    let builder_id = flag("--builder-id");
    let slot_id = flag("--slot-id");
    let listen = flag("--hold-proxy-listen");
    match (builder_id, slot_id, listen) {
        (None, None, None) => Ok(None),
        (Some(builder_id), Some(slot_id), Some(listen)) => {
            let proxy_listen = listen.parse::<std::net::SocketAddr>().map_err(|e| {
                anyhow!("--hold-proxy-listen `{listen}` is not a host:port address: {e}")
            })?;
            Ok(Some(HoldSlotConfig {
                builder_id,
                slot_id,
                proxy_listen,
            }))
        }
        _ => Err(anyhow!(
            "--builder-id, --slot-id and --hold-proxy-listen must be given together \
             (they are one registration: the slot ato-api knows, and the local port \
             its public origin proxies to)"
        )),
    }
}

impl Config {
    fn from_env_args() -> Result<Self> {
        let args: Vec<String> = std::env::args().collect();
        let flag = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let has = |name: &str| args.iter().any(|a| a == name);
        // ato#1002: the artifact-store env (ATO_ARTIFACT_S3_*) is all-or-nothing;
        // a PARTIAL set is an operator error that must stop the daemon at
        // startup, never surface per-job (process_job re-reads the same env).
        upload::ArtifactStore::from_env().map_err(|e| anyhow!(e))?;
        Ok(Config {
            api_url: std::env::var("ATO_API_URL")
                .ok()
                .or_else(|| flag("--api-url"))
                .context("ATO_API_URL (or --api-url) required")?,
            token: std::env::var("SNAPSHOT_BUILDER_AGENT_TOKEN")
                .context("SNAPSHOT_BUILDER_AGENT_TOKEN required")?,
            agent_id: flag("--agent-id").context("--agent-id required")?,
            work: flag("--work")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("snapshot-builder")),
            rootfs_size_mib: flag("--rootfs-size-mib")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1024),
            once: has("--once"),
            poll_secs: flag("--poll-secs")
                .and_then(|s| s.parse().ok())
                .unwrap_or(15),
            hold_slot: hold_slot_from(&flag)?,
        })
    }
}

/// The server-resolved source identity on a claimed job (owner/repo/commit) — the only
/// authoritative source. A client-provided `source_ref` never appears here.
/// The pinned source a v1 build must use, exactly as ato-api resolved it.
///
/// Carries no repository coordinate — no owner, no repo, no ref — because a
/// pinned build has no use for one and a builder holding one could clone.
#[derive(Debug, Clone, Deserialize)]
struct ClaimedPinnedSource {
    source_revision_id: String,
    source_archive_digest: String,
    source_archive_object_key: String,
    source_tree_digest: String,
}

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
    /// The server-resolved approved source. Present for the source-backed kinds
    /// (recipe / dockerfile_import), and `None` for the self-contained kinds
    /// whose whole input is `params` (compose_import; oci_image_import never reads
    /// it either) — those pack from params, not a checkout. A recipe/dockerfile
    /// job with no source fails closed at the `source` stage.
    #[serde(default)]
    source: Option<ClaimedSource>,
    /// The pinned source for a v1 wizard submission build (ato-api#360).
    ///
    /// Present means: fetch THIS archive and build from it. Absent means this is
    /// a legacy job and `source` + `recipe_toml` are what it builds from. The two
    /// are NOT a preference order — a builder must never treat one as a fallback
    /// for the other, which is why they are separate fields rather than one
    /// nullable source that could be "topped up" from the other.
    #[serde(default)]
    pinned_source: Option<ClaimedPinnedSource>,
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
    /// Capsule v1 (`ato.execution-contract/v1`) expected contract, when the
    /// control plane pinned one for this job.
    ///
    /// ato-api DOES send this (with `execution_id` and
    /// `execution_identity_schema` below) whenever the job has a row in
    /// `capsule_snapshot_job_execution_contracts` — migration 0123. All three
    /// stay `#[serde(default)]` because a job enqueued without a pinned
    /// identity, or an installation that has not applied 0123, legitimately has
    /// none, and must parse exactly as before.
    #[serde(default)]
    execution_contract: Option<ExecutionContractV1>,
    /// The canonical hash of `execution_contract`, as the control plane stored
    /// it. Carried separately (rather than recomputed) so the builder can VERIFY
    /// the pair agrees instead of trusting either alone — see
    /// [`crate::claim_eligibility`]. Read once the hold lane is wired into the
    /// job loop; parsed now so the claim shape is already correct.
    #[serde(default)]
    #[allow(dead_code)]
    execution_id: Option<String>,
    /// The identity schema tag the contract was stored under.
    #[serde(default)]
    #[allow(dead_code)]
    execution_identity_schema: Option<String>,
    // ── Submission Wizard §3.1 claim extension (interactive_capture only) ──
    //
    // The api emits these five fields on (and only on) a claimed
    // `interactive_capture` job. All five are `#[serde(default)]` optionals, so
    // every OTHER kind parses byte-identically to before — a recipe/import/
    // materialize claim simply leaves them `None`. They are assembled
    // ALL-OR-NOTHING by `interactive_capture_claim`; the individual fields are
    // never read directly, because a half-present set is a contract skew, not a
    // half-usable fencing tuple.
    /// Required LITERAL when present — but the gate is JOB-SCOPED, not
    /// batch-scoped: a claim response is one document carrying several jobs of
    /// several kinds, so a deserializer that rejected a skewed value outright
    /// would fail the whole batch and drop the healthy recipe / import jobs
    /// beside it. [`wizard_wire::ClaimedWireContractVersion`] parses any string
    /// and fails closed in `interactive_capture_claim` below, so a version skew
    /// still runs NO wizard semantics — it just costs only its own job.
    #[serde(default)]
    wire_contract_version: Option<wizard_wire::ClaimedWireContractVersion>,
    #[serde(default)]
    submission_attempt_id: Option<String>,
    #[serde(default)]
    worker_claim_id: Option<String>,
    /// A [`wizard_wire::LeaseToken`], NOT a `String`: `ClaimedJob` derives
    /// `Debug`, and a bare secret here would print on any `{:?}` of a claimed
    /// job.
    #[serde(default)]
    lease_token: Option<wizard_wire::LeaseToken>,
    #[serde(default)]
    lease_expires_at: Option<String>,
}

fn default_job_kind() -> String {
    "recipe".into()
}

impl ClaimedJob {
    /// Submission Wizard §3.1: assemble the interactive-capture claim extension
    /// ALL-OR-NOTHING, mirroring [`upload::ArtifactStore::from_parts`]' gate — a
    /// PARTIAL set is a server/daemon contract skew and must fail closed, never
    /// produce a fencing tuple with a guessed member. The error names the
    /// MISSING keys only; it never echoes a value (one of them is the secret).
    fn interactive_capture_claim(
        &self,
    ) -> std::result::Result<wizard_wire::InteractiveCaptureClaimExt, String> {
        let missing: Vec<&str> = [
            (
                "wire_contract_version",
                self.wire_contract_version.is_none(),
            ),
            (
                "submission_attempt_id",
                self.submission_attempt_id.is_none(),
            ),
            ("worker_claim_id", self.worker_claim_id.is_none()),
            ("lease_token", self.lease_token.is_none()),
            ("lease_expires_at", self.lease_expires_at.is_none()),
        ]
        .iter()
        .filter(|(_, absent)| *absent)
        .map(|(key, _)| *key)
        .collect();
        if !missing.is_empty() {
            return Err(format!(
                "interactive_capture claim is missing its wire §3.1 extension (missing: {})",
                missing.join(", ")
            ));
        }
        // The §3.1 version gate, applied HERE rather than at batch parse (see the
        // field's doc): a skewed contract yields no claim extension, so no wizard
        // semantics run for this job — and its siblings in the batch are untouched.
        let wire_contract_version = self
            .wire_contract_version
            .as_ref()
            .expect("presence checked above")
            .supported()?;
        Ok(wizard_wire::InteractiveCaptureClaimExt {
            wire_contract_version,
            submission_attempt_id: self.submission_attempt_id.clone().unwrap(),
            worker_claim_id: self.worker_claim_id.clone().unwrap(),
            lease_token: self.lease_token.clone().unwrap(),
            lease_expires_at: self.lease_expires_at.clone().unwrap(),
        })
    }

    /// The FENCING-4 tuple this claim is fenced under (§1.1): `job_id` from the
    /// claim itself plus the three §3.1 extension members. Every builder request
    /// after claim carries it.
    fn fencing4(&self) -> std::result::Result<wizard_wire::Fencing4, String> {
        let ext = self.interactive_capture_claim()?;
        Ok(wizard_wire::Fencing4 {
            job_id: self.id.clone(),
            submission_attempt_id: ext.submission_attempt_id,
            worker_claim_id: ext.worker_claim_id,
            lease_token: ext.lease_token,
        })
    }
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
    /// Immutable capsule target requirement copied from the sealed manifest.
    /// Omitted for legacy Web/import artifacts; never contains access material.
    #[serde(skip_serializing_if = "Option::is_none")]
    surface_requirement: Option<SessionSurfaceRequirement>,
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
    /// ato#1028: non-secret registry-image-import provenance — the full
    /// `OciImageImportReceipt` (importer version, resolved image digest +
    /// original ref, normalized image config, options, warnings). Present ONLY
    /// on an `oci_image_import` artifact; omitted entirely for recipe /
    /// dockerfile_import builds (`skip_serializing_if`), so those acks stay
    /// byte-identical against the `.strict()` ack schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    oci_import_receipt: Option<serde_json::Value>,
    /// ato#1049: non-secret compose-import provenance — the full
    /// `ComposeImportReceipt` (importer version, per-service pinned digests +
    /// kinds, public entrypoint, self-contained-env note). Present ONLY on a
    /// `compose_import` artifact; omitted entirely for the other lanes
    /// (`skip_serializing_if`), so those acks stay byte-identical against the
    /// `.strict()` ack schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    compose_import_receipt: Option<serde_json::Value>,
    /// Store thumbnail automation: a best-effort build-time screenshot of the
    /// booted app's root page (base64-encoded PNG, ~500KB raw cap), captured
    /// during `build_ready_state`'s warmup window — see `snapshot::screenshot`.
    /// `None` whenever capture wasn't possible (no headless browser present,
    /// guest unreachable, timeout, oversized/garbled output) — omitted entirely
    /// in that case (`skip_serializing_if`), so an ack from a builder host with
    /// no browser installed stays byte-identical against the pre-screenshot
    /// `.strict()` ack schema. The ato-api ack handler treats this the same
    /// way: decode + upload + persist as the store thumbnail ONLY if the
    /// capsule has none yet, wrapped so it can never fail the ack.
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot_png_base64: Option<String>,
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

/// P0 Ready-State warmup for the RECIPE lane: the author's `[snapshot]` table.
/// Validated here so an authoring typo fails the build with a pointed error
/// instead of an opaque warmup-timeout later.
fn warmup_from_manifest(m: &CapsuleManifest) -> std::result::Result<WarmupRecipe, String> {
    let w = WarmupRecipe::from_snapshot_config(&m.snapshot_config());
    w.validate()?;
    Ok(w)
}

/// P0 Ready-State warmup for the IMPORT lanes (no capsule.toml, so the operator
/// opts in via per-builder env). Empty by default — an import capsule is
/// unchanged unless an operator opts in. An invalid path is a build error, not a
/// silent skip: a typo'd `ATO_SNAPSHOT_BUILDER_*` that quietly produced a
/// NON-warmed artifact is exactly the confusion this flight exists to remove.
fn warmup_from_env() -> std::result::Result<WarmupRecipe, String> {
    let paths: Vec<String> = std::env::var("ATO_SNAPSHOT_BUILDER_WARMUP_PATHS")
        .ok()
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let content_ready = std::env::var("ATO_SNAPSHOT_BUILDER_CONTENT_READY_PATH")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let num = |k: &str| -> std::result::Result<Option<u64>, String> {
        match std::env::var(k) {
            Ok(v) => v
                .trim()
                .parse::<u64>()
                .map(Some)
                .map_err(|e| format!("{k}: not a number ({e})")),
            Err(_) => Ok(None),
        }
    };
    let w = WarmupRecipe::new(
        paths,
        num("ATO_SNAPSHOT_BUILDER_STABLE_SUCCESSES")?
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(DEFAULT_STABLE_SUCCESSES),
        num("ATO_SNAPSHOT_BUILDER_STABLE_INTERVAL_MS")?.unwrap_or(DEFAULT_STABLE_INTERVAL_MS),
        content_ready,
    );
    w.validate()?;
    Ok(w)
}

/// The lanes this builder advertises on the claim — the server hands a job ONLY
/// if its kind is listed here (an older ato-api ignores the field and keeps
/// handing recipe jobs exactly as before). `source_materialize`
/// (SOURCE_MATERIALIZATION_SPEC) is a non-sealing lane on the SAME claim/ack
/// lease machinery: it emits a frozen source archive + A1v2 identity, not a
/// snapshot artifact (dispatched in `run_once`, not `produce_build`).
///
/// **`interactive_capture` is deliberately ABSENT from this list** — it is added
/// per-daemon by [`supported_job_kinds`] when a hold slot is configured, and a
/// builder with no slot therefore never receives one. Pinned by a unit test.
/// The kinds every builder can always take.
const SUPPORTED_JOB_KINDS: &[&str] = &[
    "recipe",
    "dockerfile_import",
    "oci_image_import",
    "compose_import",
    "source_materialize",
];

/// What THIS daemon advertises on the claim.
///
/// `interactive_capture` is added only when a hold slot is configured. That is
/// the switch: a builder with no slot cannot make a held guest reachable, so it
/// must not take holds — claiming one would burn a full build and then fail at
/// `hold-ready` with `builder_slot_not_registered`. Configuring the slot is an
/// operator act that pairs with registering its public origin in ato-api, so the
/// two sides turn on together.
fn supported_job_kinds(cfg: &Config) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = SUPPORTED_JOB_KINDS.to_vec();
    if cfg.hold_slot.is_some() {
        kinds.push(wizard_wire::JOB_KIND_INTERACTIVE_CAPTURE);
    }
    kinds
}

fn claim(cfg: &Config) -> Result<Vec<ClaimedJob>> {
    let resp: ClaimResponse = ureq::post(&format!("{}/v1/capsule-snapshots/jobs/claim", cfg.api_url))
        .set("authorization", &format!("Bearer {}", cfg.token))
        // ato#1002: advertise every lane this builder handles (see
        // SUPPORTED_JOB_KINDS for what is on the list and what is deliberately
        // off it).
        .send_json(ureq::json!({ "agent_id": cfg.agent_id, "capacity": 1, "supported_kinds": supported_job_kinds(cfg) }))
        .map_err(|e| anyhow!("claim request: {e}"))?
        .into_json()
        .context("parse claim response")?;
    Ok(resp.jobs)
}

fn ack_sealed(cfg: &Config, job_id: &str, artifact: &Artifact) -> Result<()> {
    let res = ureq::post(&format!(
        "{}/v1/capsule-snapshots/jobs/{job_id}/ack",
        cfg.api_url
    ))
    .set("authorization", &format!("Bearer {}", cfg.token))
    .send_json(ureq::json!({ "agent_id": cfg.agent_id, "status": "sealed", "artifact": artifact }));
    match res {
        Ok(_) => Ok(()),
        // Surface the server's rejection BODY (e.g. the zod validation issues), not
        // just "status code 400" — a bare status is undebuggable for a schema skew.
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(anyhow!("sealed ack: status {code}: {body}"))
        }
        Err(e) => Err(anyhow!("sealed ack: {e}")),
    }
}

fn ack_failed(cfg: &Config, job_id: &str, stage: &str, reason: &str) -> Result<()> {
    // Truncate the reason to a sane length; it is non-secret build output. The
    // budget is the daemon's single one (`wizard_api::FAILURE_REASON_BUDGET`),
    // not a per-ack literal.
    let reason: String = wizard_api::truncate_failure_reason(reason);
    ureq::post(&format!("{}/v1/capsule-snapshots/jobs/{job_id}/ack", cfg.api_url))
        .set("authorization", &format!("Bearer {}", cfg.token))
        .send_json(ureq::json!({ "agent_id": cfg.agent_id, "status": "failed", "failure_stage": stage, "failure_reason": reason }))
        .map_err(|e| anyhow!("failed ack: {e}"))?;
    Ok(())
}

/// SOURCE_MATERIALIZATION_SPEC §4.1: the success ack for a `source_materialize` job —
/// the frozen source's A1v2 identity, the archive byte identity, and the observed
/// sizes/count. All non-secret build provenance.
#[derive(Debug, Clone, Serialize)]
struct SourceMaterializeOk {
    materialized_source_tree_hash: String,
    source_archive_hash: String,
    file_count: u64,
    uncompressed_bytes: u64,
    compressed_bytes: u64,
    /// Where the archive is STORED — the content-addressed key the API derived
    /// from `source_archive_hash` and returned with the upload authorization.
    ///
    /// This replaces the local `archive_path` that used to be reported here. A
    /// builder-local path is meaningless to anyone else: the moment this process
    /// exits or another builder claims the follow-up build, it names nothing.
    /// The path stays inside the builder (see `source_archive_upload::LocalArchive`,
    /// whose path field is private and absent from its Debug output) and only
    /// facts about the BYTES cross the wire.
    object_key: String,
}

/// SOURCE_MATERIALIZATION_SPEC §4.2: the failure ack for a `source_materialize` job —
/// the terminal pipeline state plus a machine `error_code` and a human `error_detail`.
/// `pipeline_state` is `blocked_repo` (admissibility / cap violation — terminal) or
/// `failed_internal` (IO / checkout / contract skew — retryable, ato-api owns the
/// max-3-retries policy).
#[derive(Debug, Clone, Serialize)]
struct SourceMaterializeFail {
    pipeline_state: String,
    error_code: String,
    error_detail: String,
}

impl SourceMaterializeFail {
    /// A `failed_internal` failure raised by the builder before/around the archive
    /// step (missing server-resolved source, git checkout failure) — retryable.
    fn internal(code: &str, detail: String) -> Self {
        Self {
            pipeline_state: "failed_internal".to_string(),
            error_code: code.to_string(),
            error_detail: wizard_api::truncate_failure_reason(&detail),
        }
    }

    /// Map a [`SourceMaterializeError`] from the archive step onto the ack: the
    /// pipeline state and machine code come from the error itself (admissibility /
    /// cap → `blocked_repo`; archive IO → `failed_internal`), the detail from Display.
    fn from_materialize_error(err: &SourceMaterializeError) -> Self {
        Self {
            pipeline_state: err.pipeline_state().to_string(),
            error_code: err.error_code().to_string(),
            error_detail: wizard_api::truncate_failure_reason(&err.to_string()),
        }
    }
}

/// Ack a materialized source on the shared claim/ack lease lane.
///
/// TODO(ato-api follow-up): ato-api does not yet accept a `source_materialize` ack —
/// the claim/dispatch/DB wiring and the API-mediated R2 upload are a separate follow-up
/// (SOURCE_MATERIALIZATION_SPEC §3.1 puts this on the SAME lease/ack machinery as
/// snapshot builds). This posts `status = "materialized"` + the `source_materialized`
/// result to the existing ack endpoint; the follow-up adds the server-side handler that
/// persists the hashes/caps on the candidate, uploads the archive to R2, and advances
/// the candidate to `analyzing`.
fn ack_source_materialized(cfg: &Config, job_id: &str, ok: &SourceMaterializeOk) -> Result<()> {
    let res = ureq::post(&format!(
        "{}/v1/capsule-snapshots/jobs/{job_id}/ack",
        cfg.api_url
    ))
    .set("authorization", &format!("Bearer {}", cfg.token))
    .send_json(
        ureq::json!({ "agent_id": cfg.agent_id, "status": "materialized", "source_materialized": ok }),
    );
    match res {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(anyhow!("source_materialize ack: status {code}: {body}"))
        }
        Err(e) => Err(anyhow!("source_materialize ack: {e}")),
    }
}

/// Ack a `source_materialize` failure (blocked / internal) on the shared lane.
///
/// TODO(ato-api follow-up): same missing server-side handler as
/// [`ack_source_materialized`]. Posts `status = "materialize_failed"` +
/// `{pipeline_state, error_code, error_detail}`; ato-api owns mapping `blocked_repo`
/// (terminal) vs `failed_internal` (retry, max 3).
fn ack_source_materialize_failed(
    cfg: &Config,
    job_id: &str,
    fail: &SourceMaterializeFail,
) -> Result<()> {
    ureq::post(&format!(
        "{}/v1/capsule-snapshots/jobs/{job_id}/ack",
        cfg.api_url
    ))
    .set("authorization", &format!("Bearer {}", cfg.token))
    .send_json(
        ureq::json!({ "agent_id": cfg.agent_id, "status": "materialize_failed", "source_materialize_failure": fail }),
    )
    .map_err(|e| anyhow!("source_materialize failed-ack: {e}"))?;
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
/// Persist a sealed manifest beside its CAS and return the location the registry
/// records for it.
///
/// `cas://<job_id>/<hash>` names `<work>/<job_id>/{manifest.json, cas/}`: a runner
/// restores by loading `manifest.json`, verifying `manifest.id() == hash`
/// (fail-closed), then restoring from the co-located CAS. With the artifact store
/// configured (ato#1002 Snapshot Serving v1), the pair is packed into one
/// `artifact.tar.gz` and uploaded BEFORE anything is acked, and the returned
/// location names the remote store instead. Upload failure is a failure of the
/// whole step — never sealed-without-bytes. Absent config keeps the local
/// `cas://` location, no packing, no upload.
///
/// Shared by the auto-seal build and by the interactive HOLD's capture seam, so a
/// held candidate is persisted, scanned and located by exactly the same code as a
/// built artifact.
fn persist_and_locate_artifact(
    manifest: &ReadyStateManifest,
    jobdir: &Path,
    job_id: &str,
    artifact_manifest_hash: &str,
) -> std::result::Result<String, (String, String)> {
    let fail = |stage: &str, reason: String| (stage.to_string(), reason);
    let manifest_json = serde_json::to_vec_pretty(manifest).map_err(|e| {
        fail(
            "artifact_metadata",
            format!("serialize sealed manifest: {e}"),
        )
    })?;
    // The manifest carries no layer bytes — only hashes, contracts and sizes — so
    // a canary hit here means something leaked into metadata.
    if !no_secret_scan::blob_is_clean(&manifest_json, L4_CANARIES) {
        return Err(fail(
            "no_secret_scan",
            "sealed manifest json failed the no-secret scan".into(),
        ));
    }
    std::fs::write(jobdir.join("manifest.json"), &manifest_json)
        .map_err(|e| fail("artifact_metadata", format!("persist sealed manifest: {e}")))?;

    match upload::ArtifactStore::from_env().map_err(|e| fail("artifact_upload", e))? {
        Some(store) => store
            .pack_and_upload(
                &upload::SystemImportCommandRunner,
                jobdir,
                job_id,
                artifact_manifest_hash,
            )
            .map_err(|e| fail("artifact_upload", e)),
        None => Ok(upload::cas_location(job_id, artifact_manifest_hash)),
    }
}

fn sealed_identity(
    execution_id: Option<&str>,
    runner_class_id: Option<String>,
) -> std::result::Result<(String, String), (String, String)> {
    let exec = match execution_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => id.to_string(),
        None => {
            return Err((
                "artifact_metadata".into(),
                "missing execution_id in sealed Ready-State manifest".into(),
            ));
        }
    };
    let rc = match runner_class_id.filter(|s| !s.trim().is_empty()) {
        Some(rc) => rc,
        None => {
            return Err((
                "artifact_metadata".into(),
                "missing runner_class_id (build did not pin a runner class)".into(),
            ));
        }
    };
    Ok((exec, rc))
}

/// v1.2 PR 3d-2: whether this builder is opted into SUPERVISOR builds for
/// `[secrets.*]` capsules. Off by default — secret capsules then keep failing
/// closed at eligibility exactly as v1 did.
fn supervisor_builds_enabled() -> bool {
    matches!(
        std::env::var("ATO_BUILDER_SUPERVISOR").ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Mirror of the snapshot backend's `ATO_FC_VSOCK` gate (kept private there); the
/// backend re-checks and fails closed regardless — this early copy only exists to
/// fail a supervisor job at ELIGIBILITY with an actionable message instead of
/// after a rootfs build.
fn builder_vsock_enabled() -> bool {
    matches!(
        std::env::var("ATO_FC_VSOCK").ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
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
    // A supervisor build is warranted by EITHER an env-delivery secret OR a
    // Phase 7 generated internal binding — the SAME predicate
    // derive_supervisor_build_spec enforces. Dispatching generated-bindings-only
    // manifests down the v1 path built an artifact whose vsock binding channel
    // had no supervisor_build ack — the runner then (correctly) refused to
    // restore the inconsistent artifact (caught by KVM acceptance Test I).
    if manifest.secrets.is_empty() && manifest.generated_bindings.is_empty() {
        // v1 no-binding path, byte-for-byte unchanged (it also rejects any stray
        // bindings/external/GPU itself).
        return derive_build_spec(manifest, probe).map_err(fail);
    }
    if !supervisor_enabled {
        return Err(fail(
            "capsule declares [secrets.*] or [generated_bindings.*]: supervisor builds are \
             disabled on this builder (operator opt-in: ATO_BUILDER_SUPERVISOR=1)"
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
    surface_requirement: Option<SessionSurfaceRequirement>,
    /// Explicit restore endpoints sealed into the manifest's restore contract.
    /// Empty for Web artifacts (the legacy `ports` projection stays
    /// authoritative); a pixel import seals `app_http` + `pixel_rfb` here —
    /// the runner refuses a pixel descriptor without exactly one sealed
    /// `pixel_rfb` endpoint, so the pair is derived and validated fail-closed
    /// at produce time ([`pixel_import_contract`]).
    endpoints: Vec<EndpointContract>,
    supervisor: Option<SupervisorBindings>,
    // ── sealed-ack facts (Artifact provenance) ──
    supervisor_ack: Option<SupervisorAck>,
    /// First-screen warmup recipe (`[snapshot].warmup_paths`/`content_ready_path`
    /// in the recipe lane) or operator env (`ATO_SNAPSHOT_BUILDER_*` in the
    /// import lanes). Fed into the sealed artifact's `RestoreContract` so the
    /// runner restores it together with the bytes — the ack payload itself
    /// (`Artifact`) does NOT carry it, because the ato-api ack schema is
    /// `.strict()` and the sealed manifest is already the authoritative copy.
    warmup_paths: Vec<String>,
    stable_successes: Option<u32>,
    stable_interval_ms: Option<u64>,
    content_ready_path: Option<String>,
    manifest_source: String,
    synthesized_probe: bool,
    declared_command: String,
    normalized_guest_command: String,
    docker_import_receipt: Option<serde_json::Value>,
    oci_import_receipt: Option<serde_json::Value>,
    /// ato#1049: the sealed `compose_import` receipt (multi-service resolution +
    /// self-contained-env note), threaded into the ack like the other lanes.
    compose_import_receipt: Option<serde_json::Value>,
    /// Per-job readiness `boot_timeout` (seconds) applied to the shared
    /// `build_ready_state` boot. Only `compose_import` sets it (heavy multi-image
    /// stacks need a larger budget than the env default); other lanes leave it
    /// `None` and inherit the backend env/default. Clamped in the backend.
    boot_timeout_s: Option<u32>,
    /// The capsule's authored `[seal_at]` acceptance program (RFC §6.1/§6.3),
    /// validated at produce time.
    ///
    /// Read only by the interactive HOLD, which cannot accept a candidate
    /// without it: acceptance is defined as "this argv exited 0 against a
    /// disposable restore", so a capsule that authors none has nothing that
    /// could accept. The auto-seal lane ignores it (it seals on the legacy
    /// contract), and the import lanes have no capsule.toml to author it in, so
    /// they leave it `None`.
    seal_at: Option<capsule::types::SealAtConfig>,
}

/// ato#1002 producer dispatch: `kind` selects the steps 1-3 branch. An unknown kind
/// is a server/daemon contract skew (the claim advertised `supported_kinds`) — fail
/// the job closed at `claim_kind`, never guess a lane. `source_materialize` never
/// reaches here: it is a non-sealing lane intercepted in `run_once` (it produces a
/// source archive, not a `ProducedBuild`).
fn produce_build(
    cfg: &Config,
    job: &ClaimedJob,
    jobdir: &Path,
) -> std::result::Result<ProducedBuild, (String, String)> {
    // ── The pinned lane is chosen by what the CLAIM carries, not by kind ──
    //
    // A job that carries a pinned source builds from THAT archive and from
    // nothing else. This is checked before the kind dispatch because the kind
    // ("recipe") is the same for a pinned wizard build and a legacy one — only
    // the presence of the pinned source distinguishes them, and dispatching on
    // kind first would send a pinned job down the checkout path.
    //
    // There is no fallback arm. If the archive cannot be obtained and verified,
    // the job fails; it does not clone, and it does not read a recipe.
    if let Some(pinned) = job.pinned_source.as_ref() {
        let verified = obtain_pinned_source(cfg, job, pinned, &jobdir.join("pinned-source"))?;
        return produce_pinned_v1_build(cfg, job, jobdir, verified);
    }

    match job.kind.as_str() {
        "recipe" => produce_recipe_build(cfg, job, jobdir),
        "dockerfile_import" => produce_import_build(cfg, job, jobdir),
        "oci_image_import" => produce_oci_image_import(cfg, job, jobdir),
        "compose_import" => produce_compose_import(cfg, job, jobdir),
        other => Err((
            "claim_kind".into(),
            format!(
                "unsupported job kind {other:?} (this builder supports: recipe, dockerfile_import, oci_image_import, compose_import)"
            ),
        )),
    }
}

/// The pre-#1002 pipeline, steps 1-3, byte-for-byte: materialize the server-resolved
/// source, parse + gate the manifest, derive the fail-closed build spec, compute the
/// declared execution identity, and build the bootable rootfs.
fn produce_recipe_build(
    cfg: &Config,
    job: &ClaimedJob,
    jobdir: &Path,
) -> std::result::Result<ProducedBuild, (String, String)> {
    let fail = |stage: &str, e: String| (stage.to_string(), e);

    // 1. Materialize the SERVER-RESOLVED source (pinned commit; identity/subdir validated).
    // #932: a Store-recipe job carries the APPROVED recipe manifest on the claim — it is
    // materialized as capsule.toml at the source root (authoritative over any repo file,
    // because the Store-apply publish model stores the manifest server-side and upstream
    // repos carry none). A raw-GitHub job (no recipe_toml) requires the repo's own
    // capsule.toml, fail-closed exactly as before.
    let manifest_source = if job.recipe_toml.is_some() {
        "recipe_toml"
    } else {
        "repo_capsule_toml"
    };
    let source = job.source.as_ref().ok_or_else(|| {
        fail(
            "source",
            "recipe job carries no server-resolved source".into(),
        )
    })?;
    let src = materialize_source(
        &source.github_owner,
        &source.github_repo,
        &source.commit_sha,
        source.subdirectory.as_deref(),
        job.recipe_toml.as_deref(),
        &jobdir.join("src"),
    )
    .map_err(|e| fail("source", e))?;

    // 2. Parse the capsule.toml + derive a fail-closed build spec (rejects bindings/etc.).
    let toml_bytes =
        std::fs::read(src.join("capsule.toml")).map_err(|e| fail("manifest", e.to_string()))?;
    let toml_text = String::from_utf8_lossy(&toml_bytes).into_owned();
    let manifest =
        CapsuleManifest::from_toml(&toml_text).map_err(|e| fail("manifest", e.to_string()))?;
    // v1 target/profile gate: only the manifest default target with profile "default"
    // may seal (never silently substitute the default for a different requested target).
    v1_target_profile_gate(
        &job.target_label,
        &job.profile,
        manifest.default_target.trim(),
    )?;
    // P0: the author's `[snapshot]` first-screen warmup recipe. Validated before
    // any rootfs work so a bad path fails fast with a pointed error.
    let warmup = warmup_from_manifest(&manifest).map_err(|e| fail("warmup", e))?;
    // The author's `[seal_at]` acceptance program, validated by the SAME
    // function the manifest layer uses so both reject the same argv. Validated
    // here (not where it is consumed) for the same reason as the warmup: an
    // authoring typo should fail before a rootfs is built, not after a guest is
    // held and the author has been operating it.
    let seal_at = manifest.seal_at.clone();
    if let Some(seal_at) = seal_at.as_ref() {
        capsule::types::validate_seal_at(seal_at).map_err(|e| fail("manifest", e))?;
    }
    // v1.2 PR 3d-2: secret capsules dispatch to the supervisor derivation when this
    // builder is opted in (each prerequisite fail-closed with an actionable reason);
    // no-secret capsules keep the v1 derivation untouched.
    let spec = derive_job_spec(
        &manifest,
        &SourceProbe::scan(&src),
        supervisor_builds_enabled(),
        std::env::var("ATO_GUEST_AGENT_BIN")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false),
        builder_vsock_enabled(),
    )?;

    // 2b. The declared Ato Execution Identity for this build — computed from DECLARED,
    // host-independent facts only (the server-resolved pinned source + the manifest's
    // default target/runtime/working-dir/dependencies), via the same graph
    // canonicalization the launch path uses. Never from the job id / artifact hash /
    // builder-host state. Stamped into the sealed manifest by build_ready_state.
    let target = manifest
        .resolve_default_target()
        .map_err(|e| fail("manifest", e.to_string()))?;
    if let Some(surface) = &target.surface {
        surface
            .validate()
            .map_err(|error| fail("manifest", format!("invalid surface requirement: {error}")))?;
        // The recipe lane has no way to seal the explicit `pixel_rfb` restore
        // endpoint a pixel artifact requires (the runner refuses a pixel
        // descriptor without exactly one) — sealing the requirement alone would
        // register an artifact that can NEVER restore. Fail closed here instead;
        // pixel builds go through the dockerfile_import lane's `pixel_rfb_port`.
        if surface.kind == SessionSurfaceKind::PixelStream {
            return Err(fail(
                "manifest",
                "pixel_stream surface requirements are not supported by the recipe lane \
                 (use the dockerfile_import lane with params.pixel_rfb_port)"
                    .to_string(),
            ));
        }
    }
    let envelope = ReadyStateDeclaredEnvelope {
        source_identifier: store_source_identifier(
            &source.github_owner,
            &source.github_repo,
            &source.commit_sha,
            source.subdirectory.as_deref(),
        ),
        // The REQUESTED target (gate-validated == manifest.default_target): the identity
        // is computed for the target actually being built, never a substituted one.
        target_label: job.target_label.clone(),
        runtime: target.runtime.clone(),
        working_directory: target.working_dir.clone(),
        dependencies: declared_dependencies_from_manifest_toml(&toml_text)
            .map_err(|e| fail("artifact_metadata", e))?,
        network_policy_hash: None,
        capability_policy_hash: None,
    };
    let declared_execution_id = envelope.declared_execution_id();

    // 3. Build the bootable rootfs (Docker→ext4; commands run only in Docker/guest).
    let ext4 = jobdir.join("rootfs.ext4");
    build_rootfs(&src, &spec, &ext4, cfg.rootfs_size_mib).map_err(|e| fail("rootfs_build", e))?;
    let rootfs = std::fs::read(&ext4).map_err(|e| fail("rootfs_build", e.to_string()))?;

    // Capsule v1 execution identity (`ato.execution-contract/v1`, RFC §4.6
    // strict finalization gate) — confirm-only, and ONLY attempted when the
    // claimed job itself carries an expected contract (`job.execution_contract`,
    // pinned by the control plane). No ato-api deployment sends this field yet
    // (a repo-wide search of the claim wire protocol confirms it), so this is
    // unreachable in production today — it exists so the wiring is already
    // correct (and tested) for the day it is. See
    // `attempt_v1_execution_identity`'s doc for exactly which facets are
    // measured for real vs. why the rest legitimately cannot be. Only the
    // recipe lane resolves a `CapsuleManifest`/target/source triple in the
    // shape this needs — the import lanes (`dockerfile_import`/
    // `oci_image_import`/`compose_import`) are not wired here, matching the
    // discarded old PR's own scoping (it finalized v1 contracts only in this
    // lane too).
    if let Some(expected) = job.execution_contract.as_ref() {
        attempt_v1_execution_identity(expected, &src, &rootfs)?;
    }

    // v1.2 PR 3e-2c: capture the supervisor binding names for the SEALED ACK. ato-api's
    // artifactSchema now accepts an optional `supervisor_build` (3e-2), so the ack must
    // carry the names — otherwise ato-api registers the row as no-binding + PUBLIC
    // (the E2E caught exactly this). A no-binding capsule keeps the field absent, so
    // those acks stay byte-identical against the .strict() schema.
    let supervisor_ack = spec.supervisor.as_ref().map(|s| SupervisorAck {
        binding_names: s.binding_names.clone(),
    });
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
            .map(|v| DurableVolumeSpec {
                state_name: v.state_name.clone(),
                size_mb: v.size_mb,
            })
            .collect();
        let state_owner_scope = if volumes.is_empty() {
            None
        } else {
            manifest.persistent_state_owner_scope()
        };
        SupervisorBindings {
            binding_names: s.binding_names.clone(),
            state_volumes: volumes,
            state_owner_scope,
        }
    });

    Ok(ProducedBuild {
        rootfs,
        port: spec.port,
        healthcheck: spec.healthcheck.clone(),
        execution_id: declared_execution_id,
        capsule_manifest_hash: format!("blake3:{}", blake3::hash(&toml_bytes).to_hex()),
        surface_requirement: target.surface.clone(),
        endpoints: Vec::new(),
        supervisor,
        supervisor_ack,
        manifest_source: manifest_source.to_string(),
        synthesized_probe: spec.probe_synthesized,
        declared_command: spec.declared_start_cmd,
        normalized_guest_command: spec.start_cmd,
        docker_import_receipt: None,
        oci_import_receipt: None,
        compose_import_receipt: None,
        boot_timeout_s: None,
        // The authored acceptance program, validated above. Only the interactive
        // HOLD reads it.
        seal_at,
        // P0 Ready-State warmup — the author's `[snapshot]` recipe rides the
        // sealed artifact. An empty `warmup_paths` (the default when no
        // `[snapshot]` table is present) leaves `stable_*` as `None`, so the
        // snapshot backend applies its v1 fallback (1 success / 250ms).
        warmup_paths: warmup.warmup_paths,
        stable_successes: warmup.stable_successes,
        stable_interval_ms: warmup.stable_interval_ms,
        content_ready_path: warmup.content_ready_path,
    })
}

/// Attempt to confirm this build's Capsule v1 Execution Identity
/// (`ato.execution-contract/v1`) against `expected` — the contract the
/// control plane pinned on the claimed job (`job.execution_contract`). This
/// function only ever reads `expected`; it never derives, invents, or
/// self-attests one (there is no other source of "expected" available to
/// this daemon — see the call site's doc comment).
///
/// Returns:
/// * `Ok(None)` — the RFC §4.6 strict gate legitimately refused because some
///   required facet has no measurement producer anywhere in this codebase
///   yet ([`FinalizationError::UnmeasuredFacet`] — see "Honest scope" below
///   for exactly which facet that is in practice today). Not an error: the
///   legacy (non-v1) seal proceeds unchanged.
/// * `Ok(Some(envelope))` — every facet this function measured agreed with
///   `expected` AND every other required facet was already satisfied. Not
///   reachable with any producer coverage that exists in this codebase today
///   (see "Honest scope"), but this is the success path a future producer PR
///   unlocks without any further change here.
/// * `Err((stage, reason))` — a genuine, caught problem: one of the facets
///   this function actually measures for real
///   ([`FinalizationError::FacetMismatch`] on `source.digest`,
///   `dependencies`, or `filesystem.readonly_layers`) disagreed with what the
///   control plane pinned. That is real drift this gate exists to catch, so
///   it fails the job via this file's normal `(stage, reason)` convention
///   rather than being silently downgraded.
///
/// ## Honest scope: which facets are measured for real, and why the rest are not
///
/// Only the three G0-2-recognized producers are wired here, each reusing a
/// value this build already materializes:
///
/// * `source.digest` — [`materialized_source_tree_hash`] (RFC A1v2) over the
///   checked-out `src` tree verbatim. Unlike the CLI's analogous
///   `measure_workspace_source_digest` in `crates/cli/src/cli/commands/build.rs`,
///   this does NOT need to exclude anything: `expected` here comes from the
///   claimed JOB (the control plane), never from a file living inside `src`
///   itself, so there is no self-referential hash-quine risk the way there
///   would be if `expected` were derived by hashing a tree that also
///   contained the very digest being computed. If the checked-out repo
///   happens to commit its own `ato.lock.json`, that file's bytes are simply
///   ordinary source content from this daemon's point of view. The one thing
///   that DOES have to be excluded — the checkout's own `.git`, which A1v2
///   hashes as an ordinary directory at the root and whose index is not
///   reproducible — is already gone: `materialize_source` removes it, so this
///   digest is stable across checkouts of the same commit.
/// * `dependencies[]` — measured only in the (common) trivial case where
///   `expected` itself declares zero dependencies: zero declared and zero
///   observed is a real, honest measurement, not a placeholder. When
///   `expected` declares one or more dependencies, this is left UNMEASURED:
///   no per-dependency derivation/output digest producer exists anywhere in
///   this codebase today (confirmed by a repo-wide search — the only
///   occurrences of `derivation_digest`/`output_digest` outside the
///   contract's own types are in `crates/snapshot/src/contract_fixtures.rs`,
///   an explicit test fixture).
/// * `filesystem.readonly_layers` — the content digest of the actual sealed
///   `rootfs` bytes, the one layer this daemon's `process_job` ever populates
///   (`runtime`/`dependency`/`app` are always `None` — see `process_job`'s
///   `BuildLayers` construction). Measured only when `expected` also declares
///   exactly one readonly layer.
///
/// Every OTHER required facet — `source.projection_digest` foremost, since it
/// is the second facet [`ExecutionObservationV1::finalize`] checks, right
/// after `source.digest` — has no measurement producer anywhere in this
/// codebase yet. Because `finalize`'s facet checks run in a fixed order and
/// stop at the first missing one, this means **in the (currently
/// unreachable) case where a job carries `execution_contract`, this function
/// will return `Ok(None)` citing `source.projection_digest`**, regardless of
/// the three real measurements above. This mirrors
/// `crates/cli/src/cli/commands/build.rs`'s `attempt_v1_execution_identity`
/// precisely, and `execution_contract_finalize`'s own module doc.
fn attempt_v1_execution_identity(
    expected: &ExecutionContractV1,
    src: &Path,
    rootfs: &[u8],
) -> std::result::Result<Option<ExecutionContractEnvelopeV1>, (String, String)> {
    let fail = |reason: String| ("execution_identity".to_string(), reason);

    let mut observation = ExecutionObservationV1::new();

    // source.digest — real: hash the checked-out source tree verbatim (see
    // the doc comment above for why no exclusion is needed here, unlike the
    // CLI's equivalent).
    let source_hash = materialized_source_tree_hash(src).map_err(|error| {
        fail(format!(
            "hash checked-out source tree {}: {error}",
            src.display()
        ))
    })?;
    let source_digest = ContentDigest::try_from(source_hash).map_err(|error| {
        fail(format!(
            "materialized_source_tree_hash output did not parse as a ContentDigest: {error}"
        ))
    })?;
    observation = observation.measured_source_digest(source_digest);

    // dependencies[] — real only in the trivial zero-dependency case (see doc).
    if expected.dependencies.is_empty() {
        observation = observation.measured_dependencies(Vec::new());
    }

    // filesystem.readonly_layers — real: content digest of the actual sealed
    // rootfs bytes, only when `expected` declares exactly one such layer.
    if expected.filesystem.readonly_layers.len() == 1 {
        let algorithm = expected.filesystem.readonly_layers[0].algorithm();
        observation =
            observation.measured_readonly_layers(vec![content_digest_of(rootfs, algorithm)]);
    }

    match observation.finalize(expected) {
        Ok(finalized) => Ok(Some(finalized.into_envelope())),
        Err(FinalizationError::UnmeasuredFacet(_)) => Ok(None),
        Err(other) => Err(fail(format!(
            "Capsule v1 execution identity check failed against the control plane's expected \
             contract: {other}"
        ))),
    }
}

/// A real (not placeholder) content digest of `bytes`, using whichever
/// algorithm the corresponding expected-contract field declares — a
/// measurement must match the expected value's own algorithm choice to have
/// any chance of agreeing with it (`ContentDigest` does not fix one
/// algorithm the way the opaque `*_digest` facets do). Mirrors
/// `crates/cli/src/cli/commands/build.rs`'s helper of the same name.
fn content_digest_of(bytes: &[u8], algorithm: DigestAlgorithm) -> ContentDigest {
    match algorithm {
        DigestAlgorithm::Blake3 => {
            ContentDigest::new(DigestAlgorithm::Blake3, *blake3::hash(bytes).as_bytes())
        }
        DigestAlgorithm::Sha256 => {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            let digest = hasher.finalize();
            let mut buffer = [0u8; 32];
            buffer.copy_from_slice(&digest);
            ContentDigest::new(DigestAlgorithm::Sha256, buffer)
        }
    }
}

/// ato#1002 `dockerfile_import` producer: validate the job params fail-closed, clone
/// the server-resolved pinned commit WITHOUT a capsule.toml (an import candidate by
/// definition has none — this deliberately does not go through `materialize_source`,
/// whose manifest gate stays intact for recipe jobs), then run the v1.7 Dockerfile
/// import (secret policy fixed to `Reject`: the Store job shape carries no secret
/// conversion opt-in) and hand the packed ext4 to the SAME steps 4-7 as a recipe job.
/// Pixel Stream v1: derive the (surface requirement, sealed endpoints) pair a
/// `dockerfile_import` job opts into via `params.pixel_rfb_port`. `None` keeps
/// the Web-only import contract byte-identical (no requirement, no endpoints).
/// Every derived endpoint is re-validated through the protocol contract rules
/// (`EndpointContract::validate`) so an invalid pair can never reach seal.
fn pixel_import_contract(
    pixel_rfb_port: Option<u16>,
    app_port: u16,
    healthcheck_path: &str,
) -> std::result::Result<(Option<SessionSurfaceRequirement>, Vec<EndpointContract>), String> {
    let Some(rfb_port) = pixel_rfb_port else {
        return Ok((None, Vec::new()));
    };
    if rfb_port == app_port {
        // Defense in depth: run_dockerfile_import already refuses the collision
        // at plan derivation; keep the produce-time gate self-contained too.
        return Err(format!(
            "pixel_rfb_port {rfb_port} collides with the public app port {app_port}"
        ));
    }
    let requirement = SessionSurfaceRequirement {
        kind: SessionSurfaceKind::PixelStream,
        profiles: Some(vec![PIXEL_STREAM_PROFILE.to_string()]),
    };
    requirement
        .validate()
        .map_err(|error| format!("derived pixel surface requirement is invalid: {error}"))?;
    let endpoints = vec![
        EndpointContract {
            role: EndpointRole::AppHttp,
            protocol: EndpointProtocol::Http,
            exposure: EndpointExposure::HostInternal,
            port: u32::from(app_port),
            readiness: EndpointReadiness::HttpGet {
                path: healthcheck_path.to_string(),
            },
        },
        EndpointContract {
            role: EndpointRole::PixelRfb,
            protocol: EndpointProtocol::Tcp,
            exposure: EndpointExposure::GuestPrivate,
            port: u32::from(rfb_port),
            readiness: EndpointReadiness::FirstFrame,
        },
    ];
    for endpoint in &endpoints {
        endpoint.validate().map_err(|error| {
            format!(
                "derived pixel endpoint contract for {:?} is invalid: {error}",
                endpoint.role
            )
        })?;
    }
    Ok((Some(requirement), endpoints))
}

fn produce_import_build(
    cfg: &Config,
    job: &ClaimedJob,
    jobdir: &Path,
) -> std::result::Result<ProducedBuild, (String, String)> {
    let fail = |stage: &str, e: String| (stage.to_string(), e);

    // 1. Strict params validation BEFORE any network/build work (same bounds as the
    // ato-api enqueue validation; a violation here means the server-side gate was
    // bypassed or skewed — fail closed at eligibility).
    let params = parse_import_params(job.params.as_ref()).map_err(|e| fail("eligibility", e))?;
    // Phase 1: enforce the builder-config size caps fail-closed before any work.
    let (per_mount_cap, total_cap) = ephemeral_mount_caps();
    enforce_ephemeral_mount_caps(&params, per_mount_cap, total_cap)
        .map_err(|e| fail("eligibility", e))?;

    // 2. Clone the SERVER-RESOLVED pinned commit (identity/subdir validated; no
    // capsule.toml requirement).
    let source = job.source.as_ref().ok_or_else(|| {
        fail(
            "source",
            "dockerfile_import job carries no server-resolved source".into(),
        )
    })?;
    let src = clone_pinned_source(source, &jobdir.join("src")).map_err(|e| fail("source", e))?;

    // 3. Run the Dockerfile import: probe tool → digest-pinned build → service plan →
    // pack the imported image into a bootable supervisor ext4. DockerImportSpec::new
    // revalidates the Dockerfile path (containment discipline, defense in depth).
    let spec = DockerImportSpec::new(&params.dockerfile_path, BTreeMap::new())
        .map_err(|e| fail("eligibility", e))?;
    let ext4 = jobdir.join("rootfs.ext4");
    // The ephemeral image tag must be a valid container reference — job ids are
    // sanitized (the import's pack script removes the tag after export).
    let tag_suffix: String = job
        .id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    let req = DockerfileImportRequest {
        context_dir: &src,
        spec,
        policy: SecretEnvPolicy::Reject,
        port_override: params.port_override,
        readiness_http_path: params.readiness_http_path.clone(),
        volume_policy: params.volumes,
        ephemeral_mounts: params.ephemeral_mounts.clone(),
        host_bind_relay: params.host_bind_relay,
        pixel_rfb_port: params.pixel_rfb_port,
        image_tag: format!("ato-import-{tag_suffix}"),
        out_ext4: &ext4,
        // An explicit per-job override wins (capped in the parser); otherwise the
        // builder config default. Single-image lane — no compose floor.
        size_mib: params
            .rootfs_size_mib
            .map_or(cfg.rootfs_size_mib, u64::from),
    };
    let outcome = run_dockerfile_import(&SystemImportCommandRunner, &req)
        .map_err(|e| fail("rootfs_build", e))?;
    let rootfs = std::fs::read(&ext4).map_err(|e| fail("rootfs_build", e.to_string()))?;
    // Operator opt-in for first-screen warmup at seal time — see
    // `warmup_from_env` (import lanes have no capsule.toml).
    let warmup = warmup_from_env().map_err(|e| fail("warmup", e))?;

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
    let docker_import_receipt = serde_json::to_value(&outcome.receipt).map_err(|e| {
        fail(
            "artifact_metadata",
            format!("serialize docker import receipt: {e}"),
        )
    })?;

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

    let healthcheck = outcome
        .plan
        .readiness_http_path
        .clone()
        .unwrap_or_else(|| "/".to_string());
    // Pixel Stream v1 (params.pixel_rfb_port): derive + validate the surface
    // requirement and the explicit endpoint pair BEFORE seal — a pixel artifact
    // without exactly one sealed pixel_rfb endpoint can never restore.
    let (surface_requirement, endpoints) =
        pixel_import_contract(params.pixel_rfb_port, outcome.plan.port, &healthcheck)
            .map_err(|e| fail("eligibility", e))?;

    Ok(ProducedBuild {
        rootfs,
        port: outcome.plan.port,
        healthcheck,
        execution_id,
        capsule_manifest_hash,
        surface_requirement,
        endpoints,
        supervisor,
        supervisor_ack,
        manifest_source: "dockerfile_import".to_string(),
        synthesized_probe: outcome.plan.readiness_http_path.is_none(),
        declared_command: argv_display.clone(),
        normalized_guest_command: argv_display,
        docker_import_receipt: Some(docker_import_receipt),
        oci_import_receipt: None,
        compose_import_receipt: None,
        boot_timeout_s: None,
        // No capsule.toml, so no authored `[seal_at]` to read.
        seal_at: None,
        // Import lane has no capsule.toml: operator opts into first-screen
        // warmup via env (empty by default ⇒ the v1 healthcheck-only seal).
        warmup_paths: warmup.warmup_paths,
        stable_successes: warmup.stable_successes,
        stable_interval_ms: warmup.stable_interval_ms,
        content_ready_path: warmup.content_ready_path,
    })
}

/// ato#1028 `oci_image_import` producer: validate the job params fail-closed, then
/// PULL a public registry image and pack it into a Ready-State rootfs, reusing the
/// whole Dockerfile-import backend after the image is materialized
/// ([`run_oci_image_import`]). Unlike the recipe / dockerfile_import lanes there is
/// NO git clone — the artifact comes from the registry image named in `params.image`,
/// not from a checkout (the job's `source` is provenance for the Store recipe only,
/// unused here). Secret policy is fixed to `Reject` (the Store job shape carries no
/// secret-conversion opt-in), same as the dockerfile_import lane.
fn produce_oci_image_import(
    cfg: &Config,
    job: &ClaimedJob,
    jobdir: &Path,
) -> std::result::Result<ProducedBuild, (String, String)> {
    let fail = |stage: &str, e: String| (stage.to_string(), e);

    // 1. Strict params validation BEFORE any network/pull work (same bounds as the
    // ato-api enqueue validation; a violation here means the server-side gate was
    // bypassed or skewed — fail closed at eligibility).
    let params =
        parse_oci_import_params(job.params.as_ref()).map_err(|e| fail("eligibility", e))?;

    // 2. Run the registry-image import: probe tool → pull + digest-pin + inspect →
    // service plan → pack the pinned image into a bootable supervisor ext4. No
    // capsule.toml, no source checkout. The out_ext4 parent (jobdir) must exist.
    std::fs::create_dir_all(jobdir).map_err(|e| fail("rootfs_build", e.to_string()))?;
    let ext4 = jobdir.join("rootfs.ext4");
    let req = OciImageImportRequest {
        image_ref: params.image,
        policy: SecretEnvPolicy::Reject,
        port_override: params.port_override,
        readiness_http_path: params.readiness_http_path.clone(),
        volume_policy: params.volumes,
        host_bind_relay: params.host_bind_relay,
        out_ext4: &ext4,
        // An explicit per-job override wins (capped in the parser); otherwise the
        // builder config default. Single-image lane — no compose floor.
        size_mib: params
            .rootfs_size_mib
            .map_or(cfg.rootfs_size_mib, u64::from),
    };
    let outcome = run_oci_image_import(&SystemImportCommandRunner, &req)
        .map_err(|e| fail("rootfs_build", e))?;
    let rootfs = std::fs::read(&ext4).map_err(|e| fail("rootfs_build", e.to_string()))?;
    // Operator opt-in for first-screen warmup at seal time — see
    // `warmup_from_env` (import lanes have no capsule.toml).
    let warmup = warmup_from_env().map_err(|e| fail("warmup", e))?;

    // Import identity (ato#1002 review D3, ato#1028): execution_id = the import
    // EXECUTION identity (derived service + platform + final image digest),
    // host-independent, never a job id. capsule_manifest_hash = the blake3 import
    // DESCRIPTOR hash over the input-only envelope keyed on the RESOLVED image
    // digest (a registry import has no capsule.toml — a descriptor hash, not a
    // manifest hash; two tags of the same image share it).
    let execution_id = oci_import_execution_id(&outcome.plan, &outcome.receipt);
    let capsule_manifest_hash = oci_import_descriptor_blake3(&outcome.receipt);
    let oci_import_receipt = serde_json::to_value(&outcome.receipt).map_err(|e| {
        fail(
            "artifact_metadata",
            format!("serialize oci import receipt: {e}"),
        )
    })?;

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

    // ato#1002 review (D4): a registry import is a SUPERVISOR artifact end to end —
    // the packed rootfs runs guest-agent + supervisor even with ZERO bindings — so
    // the sealed ack ALWAYS carries supervisor_build (binding_names may be []).
    // Under the fixed Reject policy the set is always empty today; the mapping
    // stays general for a future with-bindings job shape.
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
        healthcheck: outcome
            .plan
            .readiness_http_path
            .clone()
            .unwrap_or_else(|| "/".to_string()),
        execution_id,
        capsule_manifest_hash,
        surface_requirement: None,
        endpoints: Vec::new(),
        supervisor,
        supervisor_ack,
        manifest_source: "oci_image_import".to_string(),
        synthesized_probe: outcome.plan.readiness_http_path.is_none(),
        declared_command: argv_display.clone(),
        normalized_guest_command: argv_display,
        docker_import_receipt: None,
        oci_import_receipt: Some(oci_import_receipt),
        compose_import_receipt: None,
        boot_timeout_s: None,
        // No capsule.toml, so no authored `[seal_at]` to read.
        seal_at: None,
        warmup_paths: warmup.warmup_paths,
        stable_successes: warmup.stable_successes,
        stable_interval_ms: warmup.stable_interval_ms,
        content_ready_path: warmup.content_ready_path,
    })
}

/// ato#1049: produce a `compose_import` build — a self-contained Docker Compose
/// file (image-only services) packed into ONE bootable supervisor rootfs. The
/// guest runs every compose service under the `depends_on` DAG; the single
/// public service is proxied. No capsule.toml, no source checkout — the compose
/// file IS the plan (parsed into the canonical graph, each image pulled +
/// digest-pinned, joined + packed via the v1.5 multi-image backend).
fn produce_compose_import(
    cfg: &Config,
    job: &ClaimedJob,
    jobdir: &Path,
) -> std::result::Result<ProducedBuild, (String, String)> {
    let fail = |stage: &str, e: String| (stage.to_string(), e);

    // 1. Strict params validation BEFORE any network/pull work.
    let params =
        parse_compose_import_params(job.params.as_ref()).map_err(|e| fail("eligibility", e))?;
    // Operator opt-in for first-screen warmup at seal time — see
    // `warmup_from_env` (import lanes have no capsule.toml).
    let warmup = warmup_from_env().map_err(|e| fail("warmup", e))?;

    // 2. Run the compose import: parse → per-service pull+pin → join → pack.
    std::fs::create_dir_all(jobdir).map_err(|e| fail("rootfs_build", e.to_string()))?;
    let ext4 = jobdir.join("rootfs.ext4");
    let req = snapshot::docker_import::ComposeImportRequest {
        compose_yaml: &params.compose_yaml,
        public_readiness_http_path: params.readiness_http_path.clone(),
        out_ext4: &ext4,
        // An explicit per-job override wins (capped in the parser); otherwise the
        // larger of the builder config default and the multi-service floor.
        size_mib: params
            .rootfs_size_mib
            .map(u64::from)
            .unwrap_or_else(|| cfg.rootfs_size_mib.max(COMPOSE_ROOTFS_FLOOR_MIB)),
    };
    let outcome = snapshot::docker_import::run_compose_import(&SystemImportCommandRunner, &req)
        .map_err(|e| fail("rootfs_build", e))?;
    let rootfs = std::fs::read(&ext4).map_err(|e| fail("rootfs_build", e.to_string()))?;

    // Import identity: execution_id = WHAT EXECUTES (the joined pinned service
    // images + public entrypoint); capsule_manifest_hash = the blake3 descriptor
    // over the SAME input envelope (a compose import has no capsule.toml).
    let execution_id = snapshot::docker_import::compose_import_execution_id(&outcome.receipt);
    let capsule_manifest_hash =
        snapshot::docker_import::compose_import_descriptor_blake3(&outcome.receipt);
    let compose_import_receipt = serde_json::to_value(&outcome.receipt).map_err(|e| {
        fail(
            "artifact_metadata",
            format!("serialize compose import receipt: {e}"),
        )
    })?;

    // A compose import is a SUPERVISOR artifact end to end (guest-agent +
    // supervisor drive every service). Self-contained ⇒ no external bindings.
    let binding_names = outcome.binding_names.clone();
    let supervisor = Some(SupervisorBindings {
        binding_names: binding_names.clone(),
        state_volumes: vec![],
        state_owner_scope: None,
    });
    let supervisor_ack = Some(SupervisorAck { binding_names });

    let public_svc_cmd = outcome
        .receipt
        .services
        .iter()
        .find(|s| s.name == outcome.receipt.public_service)
        .map(|s| format!("{} ({})", s.name, s.resolved_digest))
        .unwrap_or_default();

    Ok(ProducedBuild {
        rootfs,
        port: outcome.public_port,
        healthcheck: outcome
            .public_readiness_http_path
            .clone()
            .unwrap_or_else(|| "/".to_string()),
        execution_id,
        capsule_manifest_hash,
        surface_requirement: None,
        // A compose import is a Web artifact — no sealed restore endpoints; the
        // legacy `ports` projection stays authoritative (see ProducedBuild docs).
        endpoints: Vec::new(),
        supervisor,
        supervisor_ack,
        manifest_source: "compose_import".to_string(),
        synthesized_probe: outcome.synthesized_probe,
        declared_command: public_svc_cmd.clone(),
        normalized_guest_command: public_svc_cmd,
        docker_import_receipt: None,
        oci_import_receipt: None,
        compose_import_receipt: Some(compose_import_receipt),
        boot_timeout_s: params.boot_timeout_s,
        // No capsule.toml, so no authored `[seal_at]` to read.
        seal_at: None,
        // ato#1049 compose lane (added on nightly after this flight was cut):
        // an import lane like the others — operator env opt-in, empty default.
        warmup_paths: warmup.warmup_paths,
        stable_successes: warmup.stable_successes,
        stable_interval_ms: warmup.stable_interval_ms,
        content_ready_path: warmup.content_ready_path,
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
    volumes: VolumePolicy,
    /// Explicit, image-independent ephemeral tmpfs mounts (copy-up seeding,
    /// per-mount size cap, and optional recipe-owned static seed `files` — the
    /// SINGLE public mount param; the resolved job shape a Store recipe
    /// translates to). Structural validation is fail-closed here
    /// (`validate_ephemeral_mounts`); source containment + the secret content
    /// scan land at build time in `seed_files` staging.
    ephemeral_mounts: Vec<EphemeralMountSpec>,
    /// ato#1026: `true` opts in to the localhost→guest-IP relay for apps that
    /// bind 127.0.0.1 inside the guest (default off).
    host_bind_relay: bool,
    /// Pixel Stream v1: the guest-private RFB port. Presence opts the import
    /// into `SessionSurface(kind=pixel_stream, ato.pixel-stream.v1)` — the ack
    /// carries the surface requirement and the sealed manifest carries the
    /// explicit `app_http` + `pixel_rfb` restore endpoints.
    pixel_rfb_port: Option<u16>,
    /// Optional per-job ext4 rootfs size override (MiB, capped by
    /// [`MAX_ROOTFS_SIZE_MIB`]). `None` = the builder config default
    /// (`--rootfs-size-mib`). A large built image (Stirling-PDF extracts to
    /// ~2–3 GiB) needs more than the 1024 default or the pack fails ENOSPC.
    /// No compose-style floor applies — this lane packs a single image.
    rootfs_size_mib: Option<u32>,
}

impl Default for DockerfileImportParams {
    fn default() -> Self {
        DockerfileImportParams {
            dockerfile_path: "Dockerfile".into(),
            port_override: None,
            readiness_http_path: None,
            volumes: VolumePolicy::Reject,
            ephemeral_mounts: Vec::new(),
            host_bind_relay: false,
            pixel_rfb_port: None,
            rootfs_size_mib: None,
        }
    }
}

/// Strict, fail-closed parse of `dockerfile_import` params — the same bounds the
/// ato-api enqueue validation enforces (ato#1002): `dockerfile_path` relative, no
/// `..` component, ≤200 chars (default `"Dockerfile"`); `port_override` an integer
/// in 1..65535; `readiness_http_path` starting `/`, ≤200 chars, single-line;
/// `rootfs_size_mib` shares the import lanes' 1..=[`MAX_ROOTFS_SIZE_MIB`] cap.
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
fn parse_import_params(
    params: Option<&serde_json::Value>,
) -> std::result::Result<DockerfileImportParams, String> {
    let mut out = DockerfileImportParams::default();
    let Some(v) = params.filter(|v| !v.is_null()) else {
        return Ok(out);
    };
    let obj = v
        .as_object()
        .ok_or("dockerfile_import params must be a JSON object")?;
    for (key, val) in obj {
        match key.as_str() {
            "dockerfile_path" => {
                let p = val
                    .as_str()
                    .ok_or("params.dockerfile_path must be a string")?;
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
            "port_override" => out.port_override = Some(parse_port_override_value(val)?),
            "readiness_http_path" => {
                out.readiness_http_path = Some(parse_readiness_http_path_value(val)?)
            }
            "volumes" => {
                // ato#1024 + Phase 1: the legacy string form "tmpfs" (map all
                // image VOLUMEs to empty tmpfs) OR the structured object form
                // { "mode": "tmpfs", "size_mib": N }. Anything else is rejected
                // rather than ignored (a typo must not silently keep — or lift —
                // the fail-closed VOLUME gate).
                out.volumes = parse_volumes_param(val)?;
            }
            "ephemeral_mounts" => {
                // Phase 1: explicit, image-independent mounts. Fail-closed on
                // shape; per-path/size structural validity is enforced here and
                // re-validated at plan derivation.
                out.ephemeral_mounts = parse_ephemeral_mounts_param(val)?;
            }
            "host_bind_relay" => {
                // ato#1026: strictly a bool — a non-bool must not be silently
                // treated as truthy/falsy.
                out.host_bind_relay = val
                    .as_bool()
                    .ok_or("params.host_bind_relay must be a boolean")?;
            }
            "pixel_rfb_port" => {
                // Pixel Stream v1: same strict u16 bounds as port_override. The
                // app-port collision check needs the DERIVED app port, so it
                // lands after plan derivation (`run_dockerfile_import`).
                out.pixel_rfb_port = Some(
                    val.as_u64()
                        .and_then(|n| u16::try_from(n).ok())
                        .filter(|n| *n > 0)
                        .ok_or("params.pixel_rfb_port must be an integer in 1..=65535")?,
                );
            }
            "rootfs_size_mib" => out.rootfs_size_mib = Some(parse_rootfs_size_mib(val)?),
            other => {
                return Err(format!(
                    "unknown dockerfile_import param {other:?} (rejected fail-closed)"
                ));
            }
        }
    }
    Ok(out)
}

/// Parse a JSON integer `size_mib` (`1..=u32::MAX`). Fail-closed on 0, negative,
/// fractional, or non-numeric. Cap enforcement is separate ([`enforce_ephemeral_mount_caps`]).
fn parse_size_mib(ctx: &str, v: &serde_json::Value) -> std::result::Result<u32, String> {
    let n = v
        .as_u64()
        .filter(|n| (1..=u32::MAX as u64).contains(n))
        .ok_or_else(|| format!("{ctx} must be an integer >= 1"))?;
    Ok(n as u32)
}

/// Phase 1: parse the `volumes` param — the legacy string `"tmpfs"` (map all
/// image VOLUMEs to empty tmpfs, uncapped) OR the structured object
/// `{ "mode": "tmpfs", "size_mib"?: N }`. Fail-closed on any other value.
fn parse_volumes_param(val: &serde_json::Value) -> std::result::Result<VolumePolicy, String> {
    if let Some(s) = val.as_str() {
        return match s {
            "tmpfs" => Ok(VolumePolicy::Tmpfs { size_mib: None }),
            _ => Err("params.volumes string must be \"tmpfs\" (the only supported mapping)".into()),
        };
    }
    if let Some(obj) = val.as_object() {
        if !obj.contains_key("mode") {
            return Err("params.volumes object requires \"mode\": \"tmpfs\"".into());
        }
        let mut size_mib = None;
        for (k, v) in obj {
            match k.as_str() {
                "mode" => {
                    if v.as_str() != Some("tmpfs") {
                        return Err("params.volumes.mode must be \"tmpfs\"".into());
                    }
                }
                "size_mib" => size_mib = Some(parse_size_mib("params.volumes.size_mib", v)?),
                other => {
                    return Err(format!(
                        "unknown params.volumes field {other:?} (rejected fail-closed)"
                    ));
                }
            }
        }
        return Ok(VolumePolicy::Tmpfs { size_mib });
    }
    Err("params.volumes must be the string \"tmpfs\" or an object { \"mode\": \"tmpfs\", \"size_mib\"?: N }".into())
}

/// Parse the `ephemeral_mounts` param — an array of explicit, image-independent
/// tmpfs mounts `{ "path": "/config", "seed": "empty"|"copy-up", "size_mib"?: N,
/// "files"?: [{ "path", "source", "if_missing"? }] }` (THE single public mount
/// param — seed files belong to their mount, there is no separate seed param).
/// `path` and `seed` are required; unknown keys, non-object items, and a
/// non-array value are rejected. A file's `source_digest` is NEVER accepted from
/// params (an unknown key) — it is computed by build-time staging, so an enqueue
/// cannot forge an identity input. The full set is structurally validated
/// (paths shell-safe/non-forbidden, `size_mib >= 1`, no duplicate or nested
/// mountpoint, per-file dest/source lexical containment + duplicate dest)
/// fail-closed here and re-validated at plan derivation.
fn parse_ephemeral_mounts_param(
    val: &serde_json::Value,
) -> std::result::Result<Vec<EphemeralMountSpec>, String> {
    let arr = val
        .as_array()
        .ok_or("params.ephemeral_mounts must be an array")?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| format!("params.ephemeral_mounts[{i}] must be an object"))?;
        let mut path: Option<String> = None;
        let mut seed: Option<EphemeralMountSeed> = None;
        let mut size_mib = None;
        let mut files: Vec<EphemeralSeedFile> = Vec::new();
        for (k, v) in obj {
            match k.as_str() {
                "path" => {
                    let p = v.as_str().ok_or_else(|| {
                        format!("params.ephemeral_mounts[{i}].path must be a string")
                    })?;
                    path = Some(p.to_string());
                }
                "seed" => {
                    seed = Some(match v.as_str() {
                        Some("empty") => EphemeralMountSeed::Empty,
                        Some("copy-up") => EphemeralMountSeed::CopyUp,
                        _ => {
                            return Err(format!(
                                "params.ephemeral_mounts[{i}].seed must be \"empty\" or \"copy-up\""
                            ));
                        }
                    });
                }
                "size_mib" => {
                    size_mib = Some(parse_size_mib(
                        &format!("params.ephemeral_mounts[{i}].size_mib"),
                        v,
                    )?)
                }
                "files" => files = parse_seed_files(i, v)?,
                other => {
                    return Err(format!(
                        "unknown params.ephemeral_mounts[{i}] field {other:?} (rejected fail-closed)"
                    ));
                }
            }
        }
        let path = path.ok_or_else(|| format!("params.ephemeral_mounts[{i}] requires \"path\""))?;
        let seed = seed.ok_or_else(|| {
            format!("params.ephemeral_mounts[{i}] requires \"seed\" (\"empty\" or \"copy-up\")")
        })?;
        out.push(EphemeralMountSpec {
            path,
            seed,
            size_mib,
            source: EphemeralMountSource::Explicit,
            files,
        });
    }
    validate_ephemeral_mounts(&out)?;
    Ok(out)
}

/// Strict parse of one mount's `files` array (`{ path, source, if_missing? }`).
/// `path` is the mount-relative DESTINATION; `source` is recipe-root-relative.
/// `source_digest` is filled at build-time staging, never parsed.
fn parse_seed_files(
    mi: usize,
    val: &serde_json::Value,
) -> std::result::Result<Vec<EphemeralSeedFile>, String> {
    let arr = val
        .as_array()
        .ok_or_else(|| format!("params.ephemeral_mounts[{mi}].files must be an array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for (j, f) in arr.iter().enumerate() {
        let obj = f
            .as_object()
            .ok_or_else(|| format!("params.ephemeral_mounts[{mi}].files[{j}] must be an object"))?;
        let mut dest: Option<String> = None;
        let mut source: Option<String> = None;
        let mut if_missing = false;
        for (k, v) in obj {
            match k.as_str() {
                "path" => {
                    dest = Some(
                        v.as_str()
                            .ok_or_else(|| {
                                format!(
                                    "params.ephemeral_mounts[{mi}].files[{j}].path must be a string"
                                )
                            })?
                            .to_string(),
                    )
                }
                "source" => source = Some(
                    v.as_str()
                        .ok_or_else(|| {
                            format!(
                                "params.ephemeral_mounts[{mi}].files[{j}].source must be a string"
                            )
                        })?
                        .to_string(),
                ),
                "if_missing" => {
                    if_missing = v.as_bool().ok_or_else(|| {
                        format!(
                            "params.ephemeral_mounts[{mi}].files[{j}].if_missing must be a boolean"
                        )
                    })?
                }
                other => {
                    return Err(format!(
                        "unknown params.ephemeral_mounts[{mi}].files[{j}] key {other:?} (rejected fail-closed)"
                    ));
                }
            }
        }
        let dest = dest.ok_or_else(|| {
            format!("params.ephemeral_mounts[{mi}].files[{j}] missing required key \"path\"")
        })?;
        let source = source.ok_or_else(|| {
            format!("params.ephemeral_mounts[{mi}].files[{j}] missing required key \"source\"")
        })?;
        out.push(EphemeralSeedFile {
            path: dest,
            source_path: source,
            source_digest: String::new(),
            if_missing,
        });
    }
    Ok(out)
}

/// Phase 1: the builder-config ephemeral-mount size caps, read fail-closed from
/// the environment. `ATO_MAX_EPHEMERAL_MOUNT_MIB` bounds each mount (default
/// 2048), `ATO_MAX_TOTAL_EPHEMERAL_MOUNT_MIB` bounds the sum of declared sizes
/// (default 8192). A malformed / zero value falls back to the default.
fn ephemeral_mount_caps() -> (u32, u32) {
    let read = |name: &str, default: u32| {
        std::env::var(name)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(default)
    };
    (
        read("ATO_MAX_EPHEMERAL_MOUNT_MIB", 2048),
        read("ATO_MAX_TOTAL_EPHEMERAL_MOUNT_MIB", 8192),
    )
}

/// Enforce the per-mount + total caps over every DECLARED size (each explicit
/// mount's `size_mib` and the image-VOLUME policy's `size_mib`). Uncapped
/// (`None`) mounts contribute nothing to the MiB total (legacy uncapped shape).
/// Fail-closed.
fn enforce_ephemeral_mount_caps(
    params: &DockerfileImportParams,
    per_mount: u32,
    total: u32,
) -> std::result::Result<(), String> {
    let mut sum: u64 = 0;
    for (i, m) in params.ephemeral_mounts.iter().enumerate() {
        if let Some(s) = m.size_mib {
            if s > per_mount {
                return Err(format!(
                    "params.ephemeral_mounts[{i}] ({}) size {s} MiB exceeds the per-mount cap {per_mount} MiB (ATO_MAX_EPHEMERAL_MOUNT_MIB)",
                    m.path
                ));
            }
            sum += s as u64;
        }
    }
    if let VolumePolicy::Tmpfs { size_mib: Some(s) } = params.volumes {
        if s > per_mount {
            return Err(format!(
                "params.volumes.size_mib {s} MiB exceeds the per-mount cap {per_mount} MiB (ATO_MAX_EPHEMERAL_MOUNT_MIB)"
            ));
        }
        sum += s as u64;
    }
    if sum > total as u64 {
        return Err(format!(
            "total ephemeral mount size {sum} MiB exceeds the cap {total} MiB (ATO_MAX_TOTAL_EPHEMERAL_MOUNT_MIB)"
        ));
    }
    Ok(())
}

/// Shared strict parse of a `port_override` JSON value (integer in 1..=65535).
/// Used by both the `dockerfile_import` and `oci_image_import` param parsers so
/// the bound cannot drift between lanes.
fn parse_port_override_value(val: &serde_json::Value) -> std::result::Result<u16, String> {
    let n = val
        .as_u64()
        .filter(|n| (1..=65535).contains(n))
        .ok_or("params.port_override must be an integer in 1..65535")?;
    Ok(n as u16)
}

/// Shared strict parse of a `readiness_http_path` JSON value: starts `/`, ≤200
/// chars, single-line. 200 (not 256) because the value is acked verbatim as
/// `healthcheck_url_path`, which ato-api's strict artifact schema caps at 200;
/// single-line because it is interpolated into the builder-host pack script (a
/// newline would break out of its `#` comment and run as root on the builder —
/// [`reject_control_chars`]).
fn parse_readiness_http_path_value(val: &serde_json::Value) -> std::result::Result<String, String> {
    let p = val
        .as_str()
        .ok_or("params.readiness_http_path must be a string")?;
    if !p.starts_with('/') {
        return Err("params.readiness_http_path must start with '/'".into());
    }
    if p.chars().count() > 200 {
        return Err("params.readiness_http_path exceeds 200 characters".into());
    }
    reject_control_chars("params.readiness_http_path", p)?;
    Ok(p.to_string())
}

/// ato#1028: validated `oci_image_import` job params. `image` is REQUIRED (there is
/// no default registry image); `platform` defaults to (and in v1 must equal)
/// `linux/amd64`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OciImageImportParams {
    image: String,
    platform: String,
    port_override: Option<u16>,
    readiness_http_path: Option<String>,
    /// ato#1024: `Tmpfs` when `ephemeral_mounts` is a non-empty well-formed list
    /// (image-declared VOLUMEs → guest tmpfs); `Reject` (the ato#983 default)
    /// otherwise.
    volumes: snapshot::docker_import::VolumePolicy,
    host_bind_relay: bool,
    /// Optional per-job ext4 rootfs size override (MiB, capped by
    /// [`MAX_ROOTFS_SIZE_MIB`]). `None` = the builder config default
    /// (`--rootfs-size-mib`). A large registry image needs more than the 1024
    /// default or the pack fails ENOSPC. No compose-style floor applies — this
    /// lane packs a single image.
    rootfs_size_mib: Option<u32>,
}

/// ato#1028: `ephemeral_mounts` opts image-declared VOLUMEs into guest tmpfs
/// (ephemeral by design, ato#1024). v1 reuses the Dockerfile-import backend's
/// [`snapshot::docker_import::VolumePolicy`], which maps ALL image-declared
/// VOLUMEs when engaged — so a NON-EMPTY well-formed list engages `Tmpfs`, an
/// empty/absent list keeps the fail-closed VOLUME gate. Each entry is shape-
/// validated (absolute non-root, ≤200 chars, single-line, no duplicate) fail-
/// closed; v1 does NOT enforce a per-path subset (the backend maps every
/// declared VOLUME), so the list is an explicit ephemerality acknowledgement —
/// a per-path allow-list is a documented follow-up.
fn parse_ephemeral_mounts(
    val: &serde_json::Value,
) -> std::result::Result<snapshot::docker_import::VolumePolicy, String> {
    let arr = val
        .as_array()
        .ok_or("params.ephemeral_mounts must be an array of absolute path strings")?;
    if arr.len() > 64 {
        return Err("params.ephemeral_mounts has more than 64 entries".into());
    }
    let mut seen: Vec<&str> = Vec::new();
    for entry in arr {
        let p = entry
            .as_str()
            .ok_or("params.ephemeral_mounts entries must be strings")?;
        if !p.starts_with('/') || p == "/" {
            return Err(format!(
                "params.ephemeral_mounts entry {p:?} must be an absolute non-root path"
            ));
        }
        if p.chars().count() > 200 {
            return Err(format!(
                "params.ephemeral_mounts entry {p:?} exceeds 200 characters"
            ));
        }
        reject_control_chars("params.ephemeral_mounts entry", p)?;
        if seen.contains(&p) {
            return Err(format!("params.ephemeral_mounts entry {p:?} is duplicated"));
        }
        seen.push(p);
    }
    Ok(if arr.is_empty() {
        snapshot::docker_import::VolumePolicy::Reject
    } else {
        snapshot::docker_import::VolumePolicy::Tmpfs { size_mib: None }
    })
}

/// Strict, fail-closed parse of `oci_image_import` params (ato#1028) — the same
/// bounds the ato-api enqueue validation enforces. `image` is required + shape-
/// validated ([`validate_image_ref`]); `platform` must be `linux/amd64` (v1);
/// `port_override` / `readiness_http_path` share the dockerfile_import validators;
/// `ephemeral_mounts` maps to the VOLUME policy; `host_bind_relay` is a strict
/// bool; `rootfs_size_mib` shares the import lanes' 1..=[`MAX_ROOTFS_SIZE_MIB`]
/// cap. Unknown keys, non-object params, and absent/null params (no `image`)
/// are rejected.
fn parse_oci_import_params(
    params: Option<&serde_json::Value>,
) -> std::result::Result<OciImageImportParams, String> {
    let v = params
        .filter(|v| !v.is_null())
        .ok_or("oci_image_import params are required (must carry an \"image\")")?;
    let obj = v
        .as_object()
        .ok_or("oci_image_import params must be a JSON object")?;
    let mut image: Option<String> = None;
    let mut platform = snapshot::docker_import::DOCKER_IMPORT_PLATFORM.to_string();
    let mut port_override = None;
    let mut readiness_http_path = None;
    let mut volumes = snapshot::docker_import::VolumePolicy::Reject;
    let mut host_bind_relay = false;
    let mut rootfs_size_mib = None;
    for (key, val) in obj {
        match key.as_str() {
            "image" => {
                let s = val.as_str().ok_or("params.image must be a string")?;
                validate_image_ref(s)?;
                image = Some(s.to_string());
            }
            "platform" => {
                let p = val.as_str().ok_or("params.platform must be a string")?;
                if p != snapshot::docker_import::DOCKER_IMPORT_PLATFORM {
                    return Err(format!(
                        "params.platform must be {:?} (v1 imports linux/amd64 only)",
                        snapshot::docker_import::DOCKER_IMPORT_PLATFORM
                    ));
                }
                platform = p.to_string();
            }
            "port_override" => port_override = Some(parse_port_override_value(val)?),
            "readiness_http_path" => {
                readiness_http_path = Some(parse_readiness_http_path_value(val)?)
            }
            "ephemeral_mounts" => volumes = parse_ephemeral_mounts(val)?,
            "host_bind_relay" => {
                host_bind_relay = val
                    .as_bool()
                    .ok_or("params.host_bind_relay must be a boolean")?;
            }
            "rootfs_size_mib" => rootfs_size_mib = Some(parse_rootfs_size_mib(val)?),
            other => {
                return Err(format!(
                    "unknown oci_image_import param {other:?} (rejected fail-closed)"
                ));
            }
        }
    }
    let image = image.ok_or("params.image is required for an oci_image_import job")?;
    Ok(OciImageImportParams {
        image,
        platform,
        port_override,
        readiness_http_path,
        volumes,
        host_bind_relay,
        rootfs_size_mib,
    })
}

/// ato#1049: validated `compose_import` job params. The compose file itself is
/// the plan; the only tunables are the public service's readiness path and the
/// per-job rootfs size (a multi-service stack needs more than the 1024 default).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposeImportParams {
    /// The raw Docker Compose YAML (image-only services). Bounded here — the
    /// enqueue gate bounds it too; a skew fails closed.
    compose_yaml: String,
    /// Optional HTTP readiness path for the PUBLIC service (`None` = TCP-accept).
    readiness_http_path: Option<String>,
    /// Optional per-job ext4 rootfs size override (MiB, capped). `None` = the
    /// COMPOSE_ROOTFS_FLOOR default. A large image (e.g. Stirling-PDF ~2–3 GiB
    /// extracted) needs more than the 4096 floor or the pack fails ENOSPC.
    rootfs_size_mib: Option<u32>,
    /// Optional per-job readiness `boot_timeout` (seconds, capped). `None` = the
    /// backend env/default. A heavy stack (Java/Spring — Stirling-PDF, or a big
    /// JVM warmup) can take longer than the default to answer HTTP; a plain
    /// 2-service app should not pay that budget. Clamped in the backend.
    boot_timeout_s: Option<u32>,
}

/// The compose file's hard size ceiling (bytes). A compose file is small;
/// anything past this is hostile/mistaken input, rejected before any pull.
const MAX_COMPOSE_YAML_BYTES: usize = 128 * 1024;

/// A compose stack packs MULTIPLE service images into one rootfs, so it needs
/// more than the single-image 1024 default. Floor the ext4 at this (unless the
/// builder config or an explicit `rootfs_size_mib` asks for more) so a 2–3
/// service stack (e.g. Blinko: blinko + postgres) does not fail ENOSPC.
const COMPOSE_ROOTFS_FLOOR_MIB: u64 = 4096;

/// The maximum per-job rootfs override (MiB), shared by every import lane
/// (`dockerfile_import`, `oci_image_import`, `compose_import`) — one bound, one
/// parser. A big image (Stirling-PDF, etc.) needs a larger ext4 than the lane
/// default, but an unbounded value would let one job exhaust the builder disk +
/// blow up restore memory — so the override is capped fail-closed.
/// `None`/absent keeps the lane default (the compose floor for `compose_import`,
/// the builder config `--rootfs-size-mib` for the single-image lanes).
const MAX_ROOTFS_SIZE_MIB: u32 = 8192;

/// The maximum per-job readiness `boot_timeout` override (seconds). A heavy
/// image legitimately needs longer than the env default, but an unbounded value
/// would let one job pin the builder on a hung guest — so the override is capped
/// fail-closed (the backend clamps to the same ceiling). Kept in lockstep with
/// `firecracker::MAX_JOB_BOOT_TIMEOUT_S`. `None`/absent keeps the env/default.
const MAX_BOOT_TIMEOUT_S: u32 = 600;

/// Parse the optional `rootfs_size_mib` param: an integer in `1..=MAX`. Fail
/// closed on 0, negative, fractional, non-numeric, or over the cap.
fn parse_rootfs_size_mib(v: &serde_json::Value) -> std::result::Result<u32, String> {
    let n = v
        .as_u64()
        .filter(|n| (1..=MAX_ROOTFS_SIZE_MIB as u64).contains(n))
        .ok_or_else(|| {
            format!("params.rootfs_size_mib must be an integer in 1..={MAX_ROOTFS_SIZE_MIB}")
        })?;
    Ok(n as u32)
}

/// Parse the optional `boot_timeout_s` param: an integer in `1..=MAX`. Fail
/// closed on 0, negative, fractional, non-numeric, or over the cap.
fn parse_boot_timeout_s(v: &serde_json::Value) -> std::result::Result<u32, String> {
    let n = v
        .as_u64()
        .filter(|n| (1..=MAX_BOOT_TIMEOUT_S as u64).contains(n))
        .ok_or_else(|| {
            format!("params.boot_timeout_s must be an integer in 1..={MAX_BOOT_TIMEOUT_S}")
        })?;
    Ok(n as u32)
}

/// Strict, fail-closed parse of `compose_import` params: `compose_yaml` required
/// (non-empty, ≤128 KiB); `readiness_http_path` shares the import validator;
/// `rootfs_size_mib` shares the 1..=8192 cap. Unknown keys / non-object / absent
/// params reject.
fn parse_compose_import_params(
    params: Option<&serde_json::Value>,
) -> std::result::Result<ComposeImportParams, String> {
    let v = params
        .filter(|v| !v.is_null())
        .ok_or("compose_import params are required (must carry \"compose_yaml\")")?;
    let obj = v
        .as_object()
        .ok_or("compose_import params must be a JSON object")?;
    let mut compose_yaml: Option<String> = None;
    let mut readiness_http_path = None;
    let mut rootfs_size_mib = None;
    let mut boot_timeout_s = None;
    for (key, val) in obj {
        match key.as_str() {
            "compose_yaml" => {
                let s = val.as_str().ok_or("params.compose_yaml must be a string")?;
                if s.trim().is_empty() {
                    return Err("params.compose_yaml must not be empty".into());
                }
                if s.len() > MAX_COMPOSE_YAML_BYTES {
                    return Err(format!(
                        "params.compose_yaml exceeds {MAX_COMPOSE_YAML_BYTES} bytes (fail-closed)"
                    ));
                }
                compose_yaml = Some(s.to_string());
            }
            "readiness_http_path" => {
                readiness_http_path = Some(parse_readiness_http_path_value(val)?)
            }
            "rootfs_size_mib" => rootfs_size_mib = Some(parse_rootfs_size_mib(val)?),
            "boot_timeout_s" => boot_timeout_s = Some(parse_boot_timeout_s(val)?),
            other => {
                return Err(format!(
                    "unknown compose_import param {other:?} (rejected fail-closed)"
                ));
            }
        }
    }
    Ok(ComposeImportParams {
        compose_yaml: compose_yaml
            .ok_or("params.compose_yaml is required for a compose_import job")?,
        readiness_http_path,
        rootfs_size_mib,
        boot_timeout_s,
    })
}

/// ato#1002: shallow-clone the SERVER-RESOLVED pinned commit for a
/// `dockerfile_import` job. Mirrors `materialize_source`'s identity validation +
/// subdir containment (lexical + canonical) but deliberately WITHOUT its
/// capsule.toml gate — an import candidate by definition carries none (the same
/// reasoning as `docker_import_kvm_smoke`'s `clone_pinned`). `materialize_source`
/// keeps its manifest gate untouched for recipe jobs.
fn clone_pinned_source(
    source: &ClaimedSource,
    dest: &Path,
) -> std::result::Result<PathBuf, String> {
    if !valid_github_owner(&source.github_owner) {
        return Err(format!("invalid github owner {:?}", source.github_owner));
    }
    if !valid_github_repo(&source.github_repo) {
        return Err(format!("invalid github repo {:?}", source.github_repo));
    }
    let commit = source.commit_sha.as_str();
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "refusing non-pinned commit {commit:?} (need a full 40-char sha)"
        ));
    }
    let sub = source.subdirectory.as_deref().filter(|s| !s.is_empty());
    if let Some(s) = sub {
        // Lexical containment first (relative, no `..`, no prefix) — the same rule
        // materialize_source applies, via the docker_import path validator.
        validate_dockerfile_path(s).map_err(|e| format!("invalid subdirectory: {e}"))?;
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let run = |args: &[&str]| -> std::result::Result<(), String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(dest)
            .output()
            .map_err(|e| format!("git {args:?}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    };
    run(&["init", "-q"])?;
    run(&[
        "remote",
        "add",
        "origin",
        &format!(
            "https://github.com/{}/{}.git",
            source.github_owner, source.github_repo
        ),
    ])?;
    run(&["fetch", "-q", "--depth", "1", "origin", commit])?;
    run(&["checkout", "-q", "FETCH_HEAD"])?;

    // Canonical containment after checkout (closes symlink traversal), exactly like
    // materialize_source's contained_source_root — minus the manifest requirement.
    let root = match sub {
        Some(s) => dest.join(s),
        None => dest.to_path_buf(),
    };
    let dest_canon = dest
        .canonicalize()
        .map_err(|e| format!("canonicalize checkout: {e}"))?;
    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("resolved source root {} not found: {e}", root.display()))?;
    if !root_canon.starts_with(&dest_canon) {
        return Err(format!(
            "subdirectory escapes the checkout: {} is outside {}",
            root_canon.display(),
            dest_canon.display()
        ));
    }
    Ok(root_canon)
}

/// Build + seal + verify one claimed job. Returns the non-secret artifact metadata on
/// success, or `(failure_stage, failure_reason)` — never a panic, never a secret.
/// SOURCE_MATERIALIZATION_SPEC: process a `source_materialize` job. Unlike the build
/// lanes this seals NO snapshot artifact and needs no KVM/backend — it checks out the
/// pinned public source, freezes it into a deterministic content-addressed `.tar.zst`
/// with its A1v2 identity, and reports the hashes on the same claim/ack lease lane.
///
/// Steps: (1) check out the SERVER-RESOLVED pinned commit, reusing
/// [`checkout_source_tree`] (identity-validated, full-SHA pin, contained — but NO
/// capsule.toml requirement, since a source freeze applies no recipe); (2) run
/// [`materialize_source_archive`], which enforces A1v2 admissibility + the archive caps
/// and produces the frozen `.tar.zst` + its identity.
fn process_source_materialize_job(
    cfg: &Config,
    job: &ClaimedJob,
) -> std::result::Result<SourceMaterializeOk, SourceMaterializeFail> {
    let jobdir = cfg.work.join(&job.id);
    let _ = std::fs::remove_dir_all(&jobdir);

    // 1. Materialize the SERVER-RESOLVED pinned source (owner/repo/commit/subdir). A
    //    source_materialize job carries no recipe manifest and requires no capsule.toml —
    //    it freezes the repo tree as-is. A missing source is a server/daemon contract
    //    skew (the kind was claimed but no identity provided): failed_internal.
    let source = job.source.as_ref().ok_or_else(|| {
        SourceMaterializeFail::internal(
            "source_missing",
            "source_materialize job carries no server-resolved source".to_string(),
        )
    })?;
    let checkout = checkout_source_tree(
        &source.github_owner,
        &source.github_repo,
        &source.commit_sha,
        source.subdirectory.as_deref(),
        &jobdir.join("src"),
    )
    .map_err(|e| SourceMaterializeFail::internal("checkout", e))?;

    // 2. Freeze the checkout into a deterministic content-addressed archive. An
    //    inadmissible tree / cap violation maps to blocked_repo; an archive IO error to
    //    failed_internal — both via SourceMaterializeError::pipeline_state.
    let out = jobdir.join("source.tar.zst");
    let ms = materialize_source_archive(&checkout, &out)
        .map_err(|e| SourceMaterializeFail::from_materialize_error(&e))?;

    // 3. Get it OFF this disk. Until this step existed the job ended here and
    //    reported a local path, which names nothing once the process exits or
    //    another builder claims the follow-up build — so a submission could never
    //    reach a third party.
    //
    //    The builder holds no storage credential: it asks the API to authorize
    //    one upload and receives a short-lived URL for one object whose key the
    //    API derives from the archive digest. The local file is kept until the
    //    upload succeeds, because deleting it on the first error would make the
    //    bounded retry impossible.
    let archive = source_archive_upload::LocalArchive::new(
        out,
        ms.source_archive_hash.clone(),
        ms.compressed_bytes,
    );
    let transport = source_archive_upload::HttpArchiveUploadTransport {
        api_url: &cfg.api_url,
        token: &cfg.token,
        agent_id: &cfg.agent_id,
    };
    let object_key = source_archive_upload::upload_source_archive(&transport, &job.id, &archive)
        .map_err(|e| SourceMaterializeFail::internal(e.code(), e.to_string()))?;

    // The bytes are in the store now, so the local copy serves nothing. It is
    // discarded only HERE — after a successful upload — because keeping it
    // through the transfer is what makes the bounded retry possible, and a
    // failed report re-materializes from scratch anyway (the job directory is
    // wiped at the top of this function).
    archive.discard();

    Ok(SourceMaterializeOk {
        materialized_source_tree_hash: ms.materialized_source_tree_hash,
        source_archive_hash: ms.source_archive_hash,
        file_count: ms.file_count,
        uncompressed_bytes: ms.uncompressed_bytes,
        compressed_bytes: ms.compressed_bytes,
        // The API's key, not one this builder invented. The local path stays in
        // `archive`, which is dropped with the job directory.
        object_key,
    })
}

/// Obtain the pinned source for a v1 build, or fail the job.
///
/// The ONLY way a pinned build gets source. Every failure here is terminal:
/// there is no arm that clones the repository, reuses a local materialization
/// directory from an earlier job, or reads a recipe. Each of those would
/// substitute source that was never verified for source that was, at exactly
/// the moment the verified path is unavailable.
///
/// Returns a `TreeVerifiedArchive`, which is the only type the archive-only
/// build path accepts — not because a caller would remember to verify first,
/// but because no value of that type exists which has not been through both the
/// byte and the tree check.
fn obtain_pinned_source(
    cfg: &Config,
    job: &ClaimedJob,
    pinned: &ClaimedPinnedSource,
    workdir: &std::path::Path,
) -> std::result::Result<source_archive_download::TreeVerifiedArchive, (String, String)> {
    let input = snapshot::archive_only_build::ArchiveOnlyBuildInput::new(
        &pinned.source_revision_id,
        &pinned.source_archive_digest,
        &pinned.source_archive_object_key,
        &pinned.source_tree_digest,
    )
    .map_err(|e| ("source".to_string(), e.to_string()))?;

    let transport = source_archive_download::HttpArchiveDownloadTransport {
        api_url: &cfg.api_url,
        token: &cfg.token,
        agent_id: &cfg.agent_id,
    };
    source_archive_download::download_pinned_source(&transport, &job.id, &input, workdir)
        // The failure's own code, so the ack says which step refused rather than
        // flattening every terminal source failure into one stage.
        .map_err(|e| (e.code().to_string(), e.to_string()))
}

/// Build from a verified pinned archive.
///
/// Takes a `TreeVerifiedArchive` and nothing else that could name source: no
/// path, no URL, no repository coordinate. The type is the argument, so this
/// function cannot be called with source that has not been through both the
/// byte and the tree check.
///
/// Not yet complete: the projection and `v1_intake` handoff land with the
/// execution-contract wiring. What IS established here is the shape — the only
/// way in is a verified value.
fn produce_pinned_v1_build(
    _cfg: &Config,
    _job: &ClaimedJob,
    _jobdir: &Path,
    verified: source_archive_download::TreeVerifiedArchive,
) -> std::result::Result<ProducedBuild, (String, String)> {
    Err((
        "build".to_string(),
        format!(
            "the pinned v1 build path is not wired to v1_intake yet; the archive at \
             {} is verified and ready for it",
            verified.path().display()
        ),
    ))
}

fn process_job(
    cfg: &Config,
    backend: &FirecrackerBackend,
    job: &ClaimedJob,
) -> std::result::Result<Artifact, (String, String)> {
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
    let store =
        CasStore::open(jobdir.join("cas")).map_err(|e| fail("build_ready_state", e.to_string()))?;
    // A per-job readiness boot_timeout (compose_import heavy stacks — Java/Spring)
    // overrides the backend env/default for THIS build only; other lanes pass
    // None and inherit unchanged. Clamped fail-closed inside `with_boot_timeout`.
    let job_backend = backend.with_boot_timeout(produced.boot_timeout_s.map(u64::from));
    let receipt = job_backend
        .build_ready_state(BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: produced.capsule_manifest_hash.clone(),
            runner_class: None,
            surface_requirement: produced.surface_requirement.clone(),
            layers: BuildLayers {
                rootfs: produced.rootfs,
                runtime: None,
                dependency: None,
                app: None,
                vmstate: Vec::new(),
                memory: Vec::new(),
            },
            restore_contract: RestoreContract {
                ports: vec![produced.port],
                healthcheck: Some(produced.healthcheck.clone()),
                expected_ready_ms: Some(8000),
                // P0: copy the author's first-screen warmup recipe into the
                // sealed manifest so the runner restores it together with the
                // artifact (`None` for stable_* ⇒ the snapshot crate's v1
                // fallback applies — this stays byte-identical for a manifest
                // sealed before warmup-flight, since `warmup_paths` is empty).
                warmup_paths: produced.warmup_paths.clone(),
                stable_successes: produced.stable_successes,
                stable_interval_ms: produced.stable_interval_ms,
                content_ready_path: produced.content_ready_path.clone(),
                // Pixel Stream v1: the explicit app_http + pixel_rfb endpoint
                // pair (empty for Web artifacts — the legacy `ports` projection
                // stays authoritative there, and absent endpoints keep every
                // pre-existing manifest byte-identical).
                endpoints: produced.endpoints.clone(),
            },
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
        .restore(RestoreReadyStateInput {
            store: &store,
            manifest: manifest_out.clone(),
            overlay_root: jobdir.join("verify-ov"),
            host_runner_class: None,
            uffd_preview: false,
        })
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
            format!(
                "seal-side no-secret proof is not clean ({} finding(s), verdict {:?})",
                receipt.no_secret_proof.findings.len(),
                receipt.no_secret_proof.verdict
            ),
        ));
    }
    let cas_targets = no_secret_scan::ScanTargets {
        cas: Some(jobdir.join("cas")),
        ..Default::default()
    };
    let live: Vec<&[u8]> = live_secret_canaries(cfg);
    let leak = no_secret_scan::scan(&cas_targets, &live);
    if !leak.clean {
        let first = leak
            .hits
            .first()
            .map(|h| format!("{}:{}", h.target, h.path))
            .unwrap_or_default();
        return Err(fail(
            "no_secret_scan",
            format!(
                "builder credential found in the sealed artifact: {} file(s) across {} scanned; first: {first}",
                leak.hits.len(),
                leak.files_scanned
            ),
        ));
    }
    let pem = no_secret_scan::scan(&cas_targets, L4_CANARIES);
    if !pem.clean {
        let first = pem
            .hits
            .first()
            .map(|h| format!("{}:{}", h.target, h.path))
            .unwrap_or_default();
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

    // 7-8. Persist the sealed manifest beside the CAS and (when the artifact
    // store is configured) pack + upload it, yielding the location the registry
    // records. Shared with the interactive HOLD's capture seam so a held
    // candidate is persisted and located by exactly the same code as a built
    // artifact — see `persist_and_locate_artifact`.
    let artifact_location =
        persist_and_locate_artifact(&manifest_out, &jobdir, &job.id, &artifact_manifest_hash)?;

    Ok(Artifact {
        capsule_manifest_hash: produced.capsule_manifest_hash,
        execution_id,
        artifact_manifest_hash,
        runner_class_id,
        snapshot_backend: manifest_out.snapshot_backend.kind.clone(),
        artifact_location,
        healthcheck_url_path: produced.healthcheck,
        surface_requirement: manifest_out.surface_requirement.clone(),
        no_secret_scan_clean: true,
        rootfs_bytes: manifest_out
            .layers
            .rootfs
            .as_ref()
            .map(|m| m.total_len)
            .unwrap_or(0),
        mem_bytes: manifest_out
            .layers
            .memory
            .as_ref()
            .map(|m| m.total_len)
            .unwrap_or(0),
        vmstate_bytes: manifest_out
            .layers
            .vmstate
            .as_ref()
            .map(|m| m.total_len)
            .unwrap_or(0),
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
        oci_import_receipt: produced.oci_import_receipt,
        compose_import_receipt: produced.compose_import_receipt,
        screenshot_png_base64: receipt.screenshot_png_base64.clone(),
    })
}

/// The stage this slice's fail-closed `interactive_capture` refusal is reported
/// at. It names the §2 WIRE stage (`holding`), never a builder-local word: the
/// §3.8 ack's optional `failure_stage` refinement is parsed straight out of this
/// string ([`wizard_api::wizard_failure_stage`]), so a name outside the enum
/// silently drops the diagnostic and leaves admins a bare `build_failed`.
const INTERACTIVE_HOLD_REFUSAL_STAGE: &str = "holding";

/// How often the hold polls the control channel for the author's directive.
///
/// This is the latency the author feels between pressing Capture and the builder
/// acting on it, so it is deliberately far shorter than the claim loop's own
/// `--poll-secs` (which paces an unattended daemon looking for work). It is also
/// what has to fit inside ato-api's fail-closed drain deadline for a quiesce
/// (30 s): the epoch is only actionable once the proxy has acked, so the hold
/// needs several polls inside that window, not one.
const HOLD_CONTROL_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// §3.4 — coarse progress, best-effort by contract.
///
/// A failure here is logged and never fails the job: the wire makes this
/// message advisory (it does not enforce monotonic advance, and no state
/// transition hangs off it), and the very next call on this lane is a fenced one
/// that will discover a dead claim properly. Failing a build because a progress
/// ping was lost would be strictly worse for the author than a stale stage
/// label.
fn report_hold_progress(
    api: &dyn wizard_api::WizardApi,
    fencing: &wizard_wire::Fencing4,
    stage: wizard_wire::WizardStage,
) {
    if let Err(error) = api.report_progress(fencing, stage) {
        eprintln!("[builder] wizard progress {stage:?} not recorded: {error}");
    }
}

/// Submission Wizard PR-2 — the `interactive_capture` lane, end to end.
///
/// Shares the RECIPE build with the seal lane (materialize + manifest + rootfs +
/// declared execution identity) and then, instead of the auto-seal tail, keeps
/// the guest ALIVE: it fronts the live workload with a local relay the operator's
/// registered ingress origin proxies to, tells ato-api the hold is ready (§3.5),
/// and hands the whole thing to [`hold_phase::HoldPhase`] — which polls for the
/// author's directive, captures on their command, verifies each candidate by
/// disposable restore (#1088), and reports both (§3.6/§3.7).
///
/// The order below is the whole contract of this function, and each step exists
/// because the next one cannot be honest without it:
///
/// ```text
/// produce_recipe_build   the same build a sealed artifact gets
///   -> boot_and_hold     boot to the seal point, DO NOT pause (RFC §8.3 running)
///   -> HoldIngress       carry bytes from the registered slot port to the guest
///   -> hold-ready §3.5   ato-api derives the preview upstream from its OWN registry
///   -> HoldPhase::run    hold -> capture -> accept, with §3.6/§3.7 inside
///   -> terminal ack §3.8 exactly one of the legal reasons, or none at all
/// ```
///
/// Everything after the build runs with the guest up, so every early return from
/// here on tears it down (the relay first, then the guest — a relay outliving its
/// guest would accept connections it can only fail).
fn process_interactive_capture_job(
    cfg: &Config,
    backend: &FirecrackerBackend,
    job: &ClaimedJob,
    fencing: &wizard_wire::Fencing4,
) -> std::result::Result<(), (String, String)> {
    let fail = |stage: &str, reason: String| (stage.to_string(), reason);
    // The FENCING-4 tuple is parsed by the CALLER, before any build work: a job
    // with no §3.1 extension has no ack that could be sent (every call on this
    // lane would 409), so it must never reach a build in the first place — and
    // a `claim_kind` failure raised here could never leave the process anyway.
    eprintln!(
        "[builder] interactive_capture {} claimed under attempt {} / claim {}",
        job.id, fencing.submission_attempt_id, fencing.worker_claim_id
    );

    // The configured slot is what makes this kind advertised at all
    // ([`supported_job_kinds`]), so a claimed hold without one is a contract
    // skew — the api handed out a kind this daemon never offered — not a
    // degraded mode to muddle through. Refuse before spending a build.
    let slot = cfg.hold_slot.as_ref().ok_or_else(|| {
        fail(
            INTERACTIVE_HOLD_REFUSAL_STAGE,
            "this builder has no hold slot configured, so it never advertised \
             `interactive_capture` and cannot make a held guest reachable"
                .to_string(),
        )
    })?;

    let api = wizard_api_client(cfg);
    report_hold_progress(&api, fencing, wizard_wire::WizardStage::Build);

    let jobdir = cfg.work.join(&job.id);
    let _ = std::fs::remove_dir_all(&jobdir);

    // The RECIPE producer directly, NOT `produce_build`: that dispatches on
    // `job.kind`, which is `interactive_capture` here and would fail the job
    // closed at `claim_kind`. The api enqueues this lane with no `params` and a
    // server-resolved pinned source + approved recipe manifest (the same inputs
    // a recipe job gets), so the recipe branch is not a default — it is the one
    // producer whose inputs this kind actually carries.
    let produced = produce_recipe_build(cfg, job, &jobdir)?;
    eprintln!(
        "[builder] interactive_capture {} produced build (execution {})",
        job.id, produced.execution_id
    );

    // Acceptance is DEFINED as "the authored `seal_at.command` exited 0 against
    // a disposable restore" (RFC §6.3/§8.1). A capsule that authors none has
    // nothing that could accept a candidate, so the hold could only ever end at
    // its TTL with the author's work discarded. Refuse up front, where the
    // reason can name the fix.
    let seal_at = produced.seal_at.clone().ok_or_else(|| {
        fail(
            "manifest",
            "an interactive capture needs the capsule to declare a `[seal_at]` \
             command: it is what decides whether a candidate is accepted, and \
             Ato never accepts one on anything but that command's exit 0"
                .to_string(),
        )
    })?;
    let mut acceptance_config = snapshot::acceptance::acceptance_config_for_seal_at(&seal_at);

    // The Capsule v1 identity the control plane pinned on this claim. It is what
    // the eligibility proof is minted from and what `accept` binds every
    // candidate to, so the v1 sidecar for a captured candidate must carry THIS
    // id — not the legacy declared identity in the sealed manifest, which is a
    // different identity in a different space. A mismatch would fail acceptance
    // closed (`ExecutionIdentityMismatch`); passing it explicitly is what makes
    // the two agree by construction.
    let execution_id = job
        .execution_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            fail(
                INTERACTIVE_HOLD_REFUSAL_STAGE,
                "the claim carries no execution_id, so no candidate could be \
                 bound to a verified identity"
                    .to_string(),
            )
        })
        .and_then(|id| {
            capsule::execution_contract::ExecutionId::new(id.to_string()).map_err(|error| {
                fail(
                    INTERACTIVE_HOLD_REFUSAL_STAGE,
                    format!("the claim's execution_id is not a canonical id: {error}"),
                )
            })
        })?;

    let store =
        CasStore::open(jobdir.join("cas")).map_err(|e| fail("build_ready_state", e.to_string()))?;
    // Same per-job boot budget the seal lane applies, for the same boot.
    let job_backend = backend.with_boot_timeout(produced.boot_timeout_s.map(u64::from));

    // The acceptance restore on THIS lane is cold by construction, and
    // `acceptance_config_for_seal_at` does not know that.
    //
    // It allots a fixed `total_deadline - verification_timeout` — 30s — for
    // create + restore + teardown. That is sized for a warm restore. Here the
    // hold sealed into this job's own CAS and its teardown deleted the rootfs
    // cache entry it had written, so acceptance must rehydrate mem + vmstate +
    // rootfs from CapsuleFS and boot the guest to health before the author's
    // `seal_at` command may run at all — and `restore()`'s health wait alone is
    // bounded by `boot_timeout`, which is 30s by default and up to 600s per job.
    //
    // `accept()` checks the budget immediately after `restore_candidate`
    // returns, and `maximum_attempts` is 1, so an over-budget restore rejects
    // the candidate with `DeadlineExceeded` WITHOUT ever running the command it
    // was supposed to be judging. Nothing has ever spent this budget: every
    // acceptance on this lane died on the slot lock first, which is why the
    // shortfall has been invisible.
    //
    // `verification_timeout` and `maximum_attempts` are deliberately untouched,
    // so `[seal_at] timeout_seconds` still means exactly what the author wrote.
    const INTERACTIVE_REHYDRATE_SLACK: Duration = Duration::from_secs(120);
    acceptance_config.total_deadline += job_backend.boot_timeout() + INTERACTIVE_REHYDRATE_SLACK;

    report_hold_progress(&api, fencing, wizard_wire::WizardStage::Launch);
    // The boot half of `build_ready_state`, stopping at the seal point: the
    // workload is up and healthy and NOTHING has paused it. A capsule with a
    // supervisor (bindings and/or durable state volumes) is refused in here —
    // §8.3 puts it on the `workload_idle` side, and that is a separate lifecycle.
    let guest = job_backend
        .boot_and_hold(BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: produced.capsule_manifest_hash.clone(),
            runner_class: None,
            surface_requirement: produced.surface_requirement.clone(),
            layers: BuildLayers {
                rootfs: produced.rootfs,
                runtime: None,
                dependency: None,
                app: None,
                vmstate: Vec::new(),
                memory: Vec::new(),
            },
            restore_contract: RestoreContract {
                ports: vec![produced.port],
                healthcheck: Some(produced.healthcheck.clone()),
                expected_ready_ms: Some(8000),
                warmup_paths: produced.warmup_paths.clone(),
                stable_successes: produced.stable_successes,
                stable_interval_ms: produced.stable_interval_ms,
                content_ready_path: produced.content_ready_path.clone(),
                endpoints: produced.endpoints.clone(),
            },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: vec![],
            execution_id: Some(produced.execution_id.clone()),
            supervisor: produced.supervisor.clone(),
        })
        .map_err(|e| fail("launch", e.to_string()))?;

    let outcome = run_hold_session(
        cfg,
        &api,
        job,
        fencing,
        slot,
        HoldSession {
            guest,
            backend: &job_backend,
            store: &store,
            jobdir: &jobdir,
            guest_port: produced.port,
            execution_id,
            acceptance_config,
        },
    );
    // The job directory is left on disk exactly as `process_job` leaves its
    // own: the sealed bytes are already in the artifact store, and what remains
    // is diagnostics for the hold that just ended. It is wiped at the START of
    // the next job under this id, so the policy is one place, not two.
    outcome
}

/// Everything a hold owns while it runs. Grouped so the session and its teardown
/// are one scope: the guest is live for all of it, and every exit path here has
/// to bring it down.
struct HoldSession<'a> {
    guest: snapshot::HeldGuest<'a>,
    backend: &'a FirecrackerBackend,
    store: &'a CasStore,
    jobdir: &'a Path,
    guest_port: u16,
    execution_id: capsule::execution_contract::ExecutionId,
    acceptance_config: snapshot::acceptance::AcceptanceConfig,
}

/// Front the held guest, announce it, and drive [`hold_phase::HoldPhase`] to a
/// terminal outcome — then tear the session down whatever happened.
///
/// Split from the build half so the teardown has exactly one home: everything in
/// here runs with a live VM and a bound port, and a `?` that skipped the release
/// would strand both until the daemon restarts.
fn run_hold_session(
    cfg: &Config,
    api: &dyn wizard_api::WizardApi,
    job: &ClaimedJob,
    fencing: &wizard_wire::Fencing4,
    slot: &HoldSlotConfig,
    session: HoldSession<'_>,
) -> std::result::Result<(), (String, String)> {
    let fail = |stage: &str, reason: String| (stage.to_string(), reason);
    let HoldSession {
        guest,
        backend,
        store,
        jobdir,
        guest_port,
        execution_id,
        acceptance_config,
    } = session;

    // The relay is started BEFORE `hold-ready`: the api mints a preview binding
    // for the registered origin the moment it is told, and an origin whose local
    // port is not yet listening would 502 for the author at exactly the moment
    // the wizard first shows them their app. `HoldIngress::start` probes the
    // guest once for the same reason.
    let workload_addr = guest.workload_addr();
    let ingress = match hold_ingress::HoldIngress::start(slot.proxy_listen, &workload_addr) {
        Ok(ingress) => ingress,
        Err(error) => {
            guest.release();
            return Err(fail(
                INTERACTIVE_HOLD_REFUSAL_STAGE,
                format!("front the held guest at {} : {error}", slot.proxy_listen),
            ));
        }
    };
    eprintln!(
        "[builder] interactive_capture {} holding: {} -> {workload_addr}",
        job.id,
        ingress.listen_addr()
    );

    // The relay moves INTO the hold. It has to be gated at a point only the
    // hold knows — the instant before the guest is released for verification —
    // and gating it from out here could only happen after the whole hold
    // returned, which is far too late. `HoldIngress::Drop` still covers every
    // early return in there, so this is about WHERE it is closed, not whether.
    drive_hold(
        cfg,
        api,
        job,
        fencing,
        slot,
        guest,
        backend,
        store,
        jobdir,
        guest_port,
        execution_id,
        acceptance_config,
        ingress,
    )
}

/// Assemble the hold's seams and run it. The guest is consumed by the capture
/// action, which releases it on the way out.
#[allow(clippy::too_many_arguments)]
fn drive_hold(
    cfg: &Config,
    api: &dyn wizard_api::WizardApi,
    job: &ClaimedJob,
    fencing: &wizard_wire::Fencing4,
    slot: &HoldSlotConfig,
    guest: snapshot::HeldGuest<'_>,
    backend: &FirecrackerBackend,
    store: &CasStore,
    jobdir: &Path,
    guest_port: u16,
    execution_id: capsule::execution_contract::ExecutionId,
    acceptance_config: snapshot::acceptance::AcceptanceConfig,
    ingress: hold_ingress::HoldIngress,
) -> std::result::Result<(), (String, String)> {
    let fail = |stage: &str, reason: String| (stage.to_string(), reason);

    // §3.5 — the app is up. ADR-004: this carries NO upstream address; the api
    // looks `(builder_id, slot_id)` up in its OWN registry of ingress slots and
    // derives the preview host from that, so a builder can never point the proxy
    // anywhere. An unregistered pair fails closed here with no binding minted.
    let hold_ready = wizard_wire::HoldReadyRequest {
        submission_attempt_id: fencing.submission_attempt_id.clone(),
        worker_claim_id: fencing.worker_claim_id.clone(),
        builder_id: slot.builder_id.clone(),
        slot_id: slot.slot_id.clone(),
        // The job id IS this builder's session identity for the held app: it is
        // already unique per claim and it is what every local artifact of this
        // hold is filed under, so an audit trail that names it points at
        // something real.
        session_id: job.id.clone(),
        guest_port,
    };
    hold_ready
        .validate()
        .map_err(|e| fail(INTERACTIVE_HOLD_REFUSAL_STAGE, e))?;
    if let Err(error) = api.report_hold_ready(fencing, &hold_ready) {
        guest.release();
        return Err(fail(
            INTERACTIVE_HOLD_REFUSAL_STAGE,
            format!("hold-ready: {error}"),
        ));
    }
    report_hold_progress(api, fencing, wizard_wire::WizardStage::Holding);

    // The cell where each capture publishes what it sealed, and the verifier
    // reads it back. One writer, one reader, one thread — see its type doc for
    // why this is the seam rather than a second lifecycle implementation.
    let captured: guest_capture::CapturedCandidateCell = Rc::new(RefCell::new(None));
    let mut capture = guest_capture::GuestCaptureAction::new(
        guest,
        guest_capture::CaptureContext {
            job_id: job.id.clone(),
            jobdir: jobdir.to_path_buf(),
        },
        Rc::clone(&captured),
    );
    let mut eligibility = claim_eligibility::ClaimContractEligibility::from_claim(
        job.execution_contract.as_ref(),
        job.execution_id.as_deref(),
    );
    // USER DECISION (SSOT §5): the hold ends at its TTL rather than extending
    // itself. An extend is an explicit act, and nothing in this loop is entitled
    // to perform one on the author's behalf.
    let mut extend = hold_phase::NoExtend;
    let mut lifecycle = snapshot::disposable_lifecycle::BackendDisposableLifecycle {
        backend,
        store,
        candidate: guest_capture::HeldCandidateSource::new(
            Rc::clone(&captured),
            backend,
            execution_id,
        ),
        // Beside the CAS the candidate was sealed into, so the disposable
        // overlay is removed with the job directory even if a teardown is lost.
        overlay_root: jobdir.join("acceptance-overlay"),
        session: None,
        last_candidate: None,
    };

    let wall_clock = wizard_api::SystemWallClock;
    let lease = match wizard_api::LeaseRenewDriver::new(
        api,
        &wall_clock,
        job.lease_expires_at.as_deref().unwrap_or_default(),
    ) {
        Ok(lease) => lease,
        Err(lost) => {
            // Nothing was captured, so there is nothing to verify and the token
            // has no consumer.
            let _released = capture.release();
            // A lease that is already unusable at hold entry sends NO ack — the
            // same rule every lease loss on this lane follows (§3.8 has no
            // reason for it; expiry is server-owned).
            eprintln!(
                "[builder] interactive_capture {} lease unusable at hold entry, sending no ack: {lost}",
                job.id
            );
            return Ok(());
        }
    };
    let mut control =
        wizard_api::ApiControlSource::new(api, fencing, lease, HOLD_CONTROL_POLL_INTERVAL);
    let cancellation = snapshot::acceptance::AcceptanceCancellation::default();
    let clock = snapshot::acceptance::SystemClock;

    let outcome = {
        let mut phase = hold_phase::HoldPhase::new(
            &mut control,
            &mut capture,
            &mut eligibility,
            &mut extend,
            &clock,
            fencing.clone(),
            hold_phase::DEFAULT_HOLD_TTL,
        );
        phase.run()
    };

    // Close the author's relay to the guest BEFORE the guest goes away. Its
    // upstream is a fixed guest address, so anything still relayed from here on
    // would reach whatever occupies that address next — during verification,
    // the disposable guest under test. The gate answers 503 rather than
    // dropping the connection, so the wizard can say what is happening instead
    // of showing a dead frame.
    ingress.gate_for_verification();

    // The guest goes down before the ack: the attempt is over either way, and
    // holding a VM (and this builder's only build slot) open across a network
    // call to say so would be pure cost. The token this returns is what opens
    // `verify_captured_candidate` — acceptance restores a second guest, and the
    // backend admits one VMM per network identity.
    let released = capture.release();

    let outcome = match outcome {
        Ok(outcome) => outcome,
        // A receipt-less internal fault. It is NOT a rejection, so it must not
        // be acked as one; report it as the build failure it is.
        Err(fatal) => return Err(fail("acceptance", fatal.to_string())),
    };

    let termination = match outcome {
        hold_phase::HoldOutcome::Terminal(termination) => termination,
        hold_phase::HoldOutcome::CapturedPendingVerification(pending) => {
            // The author's preview is down for the length of one cold restore
            // plus `seal_at`. Reporting the stage is what makes that legible as
            // progress rather than as a failure.
            report_hold_progress(api, fencing, wizard_wire::WizardStage::Accepting);
            hold_phase::verify_captured_candidate(
                &mut control,
                &mut lifecycle,
                &mut eligibility,
                &acceptance_config,
                &cancellation,
                &clock,
                fencing,
                pending,
                &released,
            )
            .map_err(|fatal| fail("acceptance", fatal.to_string()))?
        }
    };
    eprintln!(
        "[builder] interactive_capture {} hold ended: {termination:?}",
        job.id
    );
    // §3.8 — exactly one legal terminal reason, or (for a torn-down hold) none
    // at all. The projection lives on `HoldTermination`, so this cannot pick a
    // reason the wire does not have.
    //
    // A FAILED ack is logged and left there, deliberately. Returning an error
    // here would hand the caller a job it acks as `build_failed` — a second,
    // CONTRADICTORY terminal claim about a hold that already ended (discarded,
    // or attempt_ended after an accepted candidate). One unsent ack that the
    // server sweep resolves is strictly better than two acks that disagree.
    if let Err(error) = wizard_api::ack_hold_termination(api, &cfg.agent_id, fencing, &termination)
    {
        eprintln!(
            "[builder] interactive_capture {} terminal ack was not accepted, \
             leaving the attempt to the server sweep: {error}",
            job.id
        );
    }
    Ok(())
}

/// The §3.8 terminal ack for an `interactive_capture` job, over the production
/// transport. Built per call (it is a failure path, not a hot loop).
fn wizard_api_client(cfg: &Config) -> wizard_api::HttpWizardApi<wizard_api::UreqTransport> {
    wizard_api::HttpWizardApi::new(
        cfg.api_url.clone(),
        cfg.token.clone(),
        wizard_api::UreqTransport::new(),
    )
}

/// Run one claimed `interactive_capture` job and dispose of its outcome on the
/// §3.8 terminal-ack rules.
///
/// Split out of [`run_once`] deliberately: reaching that loop needs a live claim
/// AND a real build, but the part that must never regress is the ROUTING — which
/// ack body is sent, when NO ack may be sent at all, and that a job with no
/// FENCING-4 tuple never reaches a build. Taking the api and the build step as
/// arguments makes exactly that decision assertable with no sockets and no VM,
/// the same seam shape `crate::wizard_api` uses for the transport.
fn dispatch_interactive_capture_job(
    api: &dyn wizard_api::WizardApi,
    agent_id: &str,
    job: &ClaimedJob,
    build: impl FnOnce(&wizard_wire::Fencing4) -> std::result::Result<(), (String, String)>,
) -> Result<()> {
    // §3.1 FIRST, before any build work: without the claim extension there is no
    // FENCING-4 tuple, so NO ack is sendable at all (the api would 409 it) and
    // the whole job is unreportable. Leave the attempt to the server-owned lease
    // sweep rather than spend a build on it or fall back to a body this kind
    // rejects.
    let fencing = match job.fencing4() {
        Ok(fencing) => fencing,
        Err(why) => {
            eprintln!(
                "[builder] interactive_capture {} has no fencing tuple, sending no ack: {why}",
                job.id
            );
            return Ok(());
        }
    };
    match build(&fencing) {
        Ok(()) => {
            eprintln!("[builder] interactive_capture {} -> held", job.id);
            Ok(())
        }
        Err((stage, reason)) => {
            eprintln!(
                "[builder] interactive_capture {} -> {stage}: {reason}",
                job.id
            );
            // §3.8: this kind's terminal ack is the RESTRICTED wizard payload,
            // NOT the legacy `ack_failed` body. The legacy body carries `status`
            // (a strict-mode reject for an interactive_capture job) and none of
            // FENCING-4 (a 409 even if the schema passed), so the legacy call
            // could never have landed. Failing before `holding` is exactly
            // `build_failed`.
            wizard_api::ack_interactive_build_failure(api, agent_id, &fencing, &stage, &reason)
                .map_err(|e| anyhow!("interactive_capture terminal ack: {e}"))
        }
    }
}

fn run_once(cfg: &Config, backend: &FirecrackerBackend) -> Result<usize> {
    let jobs = claim(cfg)?;
    for job in &jobs {
        eprintln!("[builder] claimed {} (capsule {})", job.id, job.capsule_id);
        // SOURCE_MATERIALIZATION_SPEC: source_materialize is a non-sealing lane on the
        // same claim/ack machinery — it emits a frozen source archive + A1v2 identity,
        // not a snapshot artifact, so it never enters produce_build/seal and has its own
        // ack shape. Unknown kinds still fall through to process_job → produce_build,
        // which fails them closed at `claim_kind`.
        if job.kind == "source_materialize" {
            match process_source_materialize_job(cfg, job) {
                Ok(ok) => {
                    eprintln!(
                        "[builder] materialized source {} (archive {})",
                        job.id, ok.source_archive_hash
                    );
                    ack_source_materialized(cfg, &job.id, &ok)?;
                }
                Err(fail) => {
                    eprintln!(
                        "[builder] source_materialize {} -> {}/{}: {}",
                        job.id, fail.pipeline_state, fail.error_code, fail.error_detail
                    );
                    ack_source_materialize_failed(cfg, &job.id, &fail)?;
                }
            }
            continue;
        }
        // Submission Wizard PR-2: the `interactive_capture` lane is a sibling of
        // `source_materialize` — it shares the RECIPE build (materialize +
        // rootfs + execution_id) but replaces the auto-seal tail with the
        // builder-resident HOLD phase (`crate::hold_phase`), so the author
        // operates their live app and picks the moment to capture.
        //
        // Reached only on a builder the operator configured with a hold slot:
        // `claim()` advertises this kind only then ([`supported_job_kinds`]), so
        // an unconfigured daemon is never handed one.
        if job.kind == wizard_wire::JOB_KIND_INTERACTIVE_CAPTURE {
            dispatch_interactive_capture_job(
                &wizard_api_client(cfg),
                &cfg.agent_id,
                job,
                |fencing| process_interactive_capture_job(cfg, backend, job, fencing),
            )?;
            continue;
        }
        match process_job(cfg, backend, job) {
            Ok(artifact) => {
                eprintln!(
                    "[builder] sealed {} (artifact {})",
                    job.id, artifact.artifact_manifest_hash
                );
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

    /// KVM acceptance Test I regression: a generated-bindings-ONLY manifest is a
    /// SUPERVISOR build (the guest generates + injects at run — vsock channel,
    /// supervisor_build ack). Dispatching it down the v1 path built an artifact
    /// whose vsock channel had no supervisor_build receipt; the runner refused
    /// to restore it (fail-closed). The dispatch predicate must mirror
    /// derive_supervisor_build_spec: secrets OR generated_bindings.
    #[test]
    fn generated_bindings_only_manifest_dispatches_to_the_supervisor_path() {
        let toml = r#"
schema_version = "0.3"
name = "genbind"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python3 -m http.server 8080"
port = 8080
readiness_probe = { http_get = "/" }

[services.web]
entrypoint = "exec python3 -m http.server 8080"

[services.web.network]
publish = true

[generated_bindings.gen_token]
generator = "random_base64"
bytes = 32
scope = "run"
targets = ["web"]
"#;
        let manifest = CapsuleManifest::from_toml(toml).expect("manifest parses");
        let probe = SourceProbe {
            has_package_json: false,
            has_requirements_txt: false,
            has_pyproject: false,
            has_index_html: false,
            has_py_files: true,
        };
        // Supervisor disabled ⇒ the supervisor-prerequisite gate must fire (NOT the
        // silent v1 fallback that produced the inconsistent artifact).
        let err = derive_job_spec(&manifest, &probe, false, true, true).unwrap_err();
        assert!(err.1.contains("generated_bindings"), "{err:?}");
        // Fully enabled ⇒ a supervisor spec with the generated binding and an
        // EMPTY external binding set (ack carries supervisor_build with []).
        let spec = derive_job_spec(&manifest, &probe, true, true, true).expect("supervisor spec");
        let sup = spec.supervisor.as_ref().expect("supervisor present");
        assert!(sup.binding_names.is_empty());
        assert_eq!(sup.generated_bindings.len(), 1);
        assert_eq!(sup.generated_bindings[0].name, "gen_token");
    }

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
        let src0 = resp.jobs[0]
            .source
            .as_ref()
            .expect("recipe claim carries a source");
        assert_eq!(src0.github_owner, "acme");
        assert_eq!(src0.commit_sha.len(), 40);
        assert!(src0.subdirectory.is_none());
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
    fn a_non_wizard_claim_carries_no_wire_extension() {
        // §3.1 is additive: every EXISTING kind parses exactly as before and
        // simply has no fencing tuple.
        let body = serde_json::json!({
            "jobs": [{
                "id": "job_2", "capsule_id": "cap_2",
                "source": { "github_owner": "acme", "github_repo": "app", "commit_sha": "b".repeat(40), "subdirectory": null },
                "target_label": "web", "profile": "default"
            }]
        });
        let resp: ClaimResponse = serde_json::from_value(body).unwrap();
        let job = &resp.jobs[0];
        assert_eq!(job.kind, "recipe");
        let why = job
            .fencing4()
            .expect_err("no wizard extension, no fencing tuple");
        for key in [
            "wire_contract_version",
            "submission_attempt_id",
            "worker_claim_id",
            "lease_token",
            "lease_expires_at",
        ] {
            assert!(why.contains(key), "{why}");
        }
    }

    /// The §3.1 claim object the api emits for an `interactive_capture` job.
    fn interactive_claim_job() -> serde_json::Value {
        serde_json::json!({
            "id": "job_w", "capsule_id": "cap_w",
            "kind": "interactive_capture",
            "target_label": "web", "profile": "default",
            "wire_contract_version": "ato.submission-wizard-wire/v1",
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "lease_token": "b64u-opaque-token",
            "lease_expires_at": "2026-07-22T09:15:00.000Z"
        })
    }

    /// A hold claimed by a daemon with NO slot is refused before any build.
    ///
    /// This is the other half of the lane switch. The kind is only advertised
    /// with a slot, so receiving one without a slot means the api handed out a
    /// kind this daemon never offered — a contract skew. Building anyway would
    /// spend the whole materialize + rootfs + boot pipeline on a hold that could
    /// never be made reachable, and the author would watch it fail at
    /// `hold-ready`; the refusal is reported at `holding` so the §3.8 ack's
    /// `failure_stage` refinement survives.
    #[test]
    fn a_hold_claimed_without_a_configured_slot_is_refused_before_any_build() {
        let mut cfg = test_cfg();
        cfg.hold_slot = None;
        // A work dir that does not exist: if this ever reached the build it
        // would fail there instead, and the assertion below would name a
        // different stage.
        cfg.work = std::env::temp_dir().join("interactive-no-slot-must-not-build");
        let resp: ClaimResponse =
            serde_json::from_value(serde_json::json!({ "jobs": [interactive_claim_job()] }))
                .unwrap();
        let job = &resp.jobs[0];
        let fencing = job.fencing4().expect("fencing tuple");
        let backend = FirecrackerBackend::new();

        let (stage, reason) =
            process_interactive_capture_job(&cfg, &backend, job, &fencing).expect_err("refused");

        assert_eq!(stage, INTERACTIVE_HOLD_REFUSAL_STAGE);
        assert!(reason.contains("no hold slot"), "{reason}");
        assert!(
            !cfg.work.exists(),
            "the refusal must come before the job directory is prepared"
        );
    }

    #[test]
    fn parses_an_interactive_capture_claim_into_its_fencing_tuple() {
        let resp: ClaimResponse =
            serde_json::from_value(serde_json::json!({ "jobs": [interactive_claim_job()] }))
                .unwrap();
        let job = &resp.jobs[0];
        assert_eq!(job.kind, wizard_wire::JOB_KIND_INTERACTIVE_CAPTURE);
        // An interactive job has no server-resolved source in the claim shape
        // above; what it DOES carry is the §3.1 extension.
        let ext = job.interactive_capture_claim().expect("§3.1 extension");
        assert_eq!(ext.lease_expires_at, "2026-07-22T09:15:00.000Z");
        let fencing = job.fencing4().expect("fencing tuple");
        assert_eq!(
            fencing.job_id, "job_w",
            "job_id comes from the claim itself"
        );
        assert_eq!(fencing.submission_attempt_id, "subatt_01J1XY");
        assert_eq!(fencing.worker_claim_id, "claim_01J1XZ");
        assert_eq!(fencing.lease_token.expose(), "b64u-opaque-token");
        // The token cannot reach a log through a `{:?}` of the claimed job.
        assert!(!format!("{job:?}").contains("b64u-opaque-token"));
    }

    #[test]
    fn a_partial_wizard_claim_extension_fails_closed() {
        // A half-present set is a contract skew, never a half-usable fencing
        // tuple — the request would 409 and the failure would be undebuggable.
        for dropped in ["submission_attempt_id", "worker_claim_id", "lease_token"] {
            let mut job = interactive_claim_job();
            job.as_object_mut().unwrap().remove(dropped);
            let resp: ClaimResponse =
                serde_json::from_value(serde_json::json!({ "jobs": [job] })).unwrap();
            let why = resp.jobs[0].fencing4().expect_err("partial extension");
            assert!(why.contains(dropped), "{why}");
        }
    }

    fn claimed(job: serde_json::Value) -> ClaimedJob {
        let resp: ClaimResponse =
            serde_json::from_value(serde_json::json!({ "jobs": [job] })).expect("claim parses");
        resp.jobs.into_iter().next().expect("one job")
    }

    /// The §3.8 routing is asserted against the REAL api client over the
    /// recording byte seam (`wizard_api::testing`), not a second hand-rolled
    /// fake: what must not regress is the BODY that leaves for an
    /// `interactive_capture` outcome, and a fake ack sink would only prove it
    /// agrees with itself.
    fn wizard_test_client(
        transport: wizard_api::testing::RecordingTransport,
    ) -> wizard_api::HttpWizardApi<wizard_api::testing::RecordingTransport> {
        wizard_api::HttpWizardApi::new(
            "https://api.example".to_string(),
            "agent-bearer-secret".to_string(),
            transport,
        )
    }

    fn sent_body(request: &wizard_api::HttpRequest) -> serde_json::Value {
        serde_json::from_str(request.body.as_ref().expect("POST carries a body"))
            .expect("body is JSON")
    }

    #[test]
    fn an_interactive_failure_acks_the_wizard_payload_not_the_legacy_failed_ack() {
        // The one production path this slice turns on. The legacy `ack_failed`
        // body carries `status` (a strict-mode reject for this kind) and none of
        // FENCING-4, so it could never have landed — the routing that picks the
        // §3.8 payload instead is what has to stay pinned.
        let job = claimed(interactive_claim_job());
        let api = wizard_test_client(wizard_api::testing::RecordingTransport::always_ok(
            serde_json::json!({}),
            1,
        ));
        dispatch_interactive_capture_job(&api, "builder-sugamo-1", &job, |fencing| {
            // The build step is handed the tuple the CALLER parsed — it never
            // re-derives (or re-fails on) the claim extension.
            assert_eq!(fencing.job_id, "job_w");
            assert_eq!(fencing.submission_attempt_id, "subatt_01J1XY");
            Err((
                INTERACTIVE_HOLD_REFUSAL_STAGE.to_string(),
                "no live guest in this slice".to_string(),
            ))
        })
        .expect("the terminal ack is sent");

        let requests = api.transport().requests();
        assert_eq!(requests.len(), 1, "exactly one terminal ack");
        assert!(
            requests[0]
                .url
                .ends_with("/v1/capsule-snapshots/jobs/job_w/ack"),
            "{}",
            requests[0].url
        );
        let body = sent_body(&requests[0]);
        assert_eq!(body["reason"], serde_json::json!("build_failed"));
        // The stage refinement survives the trip: a builder-local stage name
        // outside the §2 enum would silently drop it and leave an admin a bare
        // failure.
        assert_eq!(body["failure_stage"], serde_json::json!("holding"));
        assert_eq!(
            body["failure_reason"],
            serde_json::json!("no live guest in this slice")
        );
        // FENCING-4 rides the body; the legacy `status` member does not exist.
        assert_eq!(
            body["submission_attempt_id"],
            serde_json::json!("subatt_01J1XY")
        );
        assert_eq!(body["worker_claim_id"], serde_json::json!("claim_01J1XZ"));
        assert!(
            body.get("status").is_none(),
            "never the legacy failed-ack body: {body}"
        );
    }

    #[test]
    fn an_interactive_job_with_no_fencing_tuple_never_builds_and_acks_nothing() {
        // No §3.1 extension ⇒ no FENCING-4 ⇒ every call on this lane 409s,
        // including the ack. The job is unreportable, so it must not be built
        // either — the fail-closed check belongs BEFORE the build, and the
        // server-owned lease sweep owns the outcome.
        let mut skewed = interactive_claim_job();
        skewed
            .as_object_mut()
            .expect("object")
            .remove("lease_token");
        let job = claimed(skewed);
        // Any request at all panics this transport (nothing is scripted).
        let api = wizard_test_client(wizard_api::testing::RecordingTransport::new(vec![]));
        let mut built = false;
        dispatch_interactive_capture_job(&api, "builder-sugamo-1", &job, |_| {
            built = true;
            Ok(())
        })
        .expect("an unreportable job is not a daemon error");
        assert!(
            !built,
            "a job that can never be acked must never spend a build"
        );
        assert!(api.transport().requests().is_empty(), "no ack is sendable");
    }

    #[test]
    fn a_held_interactive_job_acks_nothing() {
        // Reaching `holding` is not a terminal state — §3.8 is for terminal
        // outcomes only, and the hold's own termination has its own projection
        // (`wizard_api::ack_hold_termination`).
        let job = claimed(interactive_claim_job());
        let api = wizard_test_client(wizard_api::testing::RecordingTransport::new(vec![]));
        dispatch_interactive_capture_job(&api, "builder-sugamo-1", &job, |_| Ok(())).expect("held");
        assert!(api.transport().requests().is_empty());
    }

    #[test]
    fn the_lanes_refusal_stage_names_a_wire_failure_stage() {
        // The §3.8 `failure_stage` refinement is PARSED out of this string, so a
        // builder-local word (`hold`) silently drops the diagnostic and the ack
        // goes out as a bare `build_failed`. Pin the producer, not just the
        // parser.
        assert_eq!(
            wizard_api::wizard_failure_stage(INTERACTIVE_HOLD_REFUSAL_STAGE),
            Some(wizard_wire::WizardFailureStage::Holding),
        );
    }

    #[test]
    fn a_skewed_wire_contract_version_fails_closed_for_its_own_job_only() {
        // The literal is the fail-closed version gate: a skewed contract yields
        // NO fencing tuple, so no wizard semantics ever run against it. What it
        // must NOT do is take the batch down with it — a claim response is one
        // document carrying several jobs of several kinds, and a wizard version
        // skew has nothing to do with the recipe job claimed beside it.
        let mut skewed = interactive_claim_job();
        skewed["wire_contract_version"] = serde_json::json!("ato.submission-wizard-wire/v2");
        let healthy = serde_json::json!({
            "id": "job_recipe", "capsule_id": "cap_r",
            "source": { "github_owner": "acme", "github_repo": "app", "commit_sha": "c".repeat(40), "subdirectory": null },
            "target_label": "web", "profile": "default"
        });
        let resp: ClaimResponse =
            serde_json::from_value(serde_json::json!({ "jobs": [skewed, healthy] }))
                .expect("one skewed wizard job never fails the whole batch");

        let why = resp.jobs[0]
            .fencing4()
            .expect_err("a skewed contract version has no fencing tuple");
        assert!(why.contains("wire_contract_version mismatch"), "{why}");
        assert!(
            why.contains("ato.submission-wizard-wire/v2"),
            "the diagnostic names the skewed value: {why}"
        );

        // The healthy sibling survived and is still claimable on its own lane.
        assert_eq!(resp.jobs.len(), 2);
        assert_eq!(resp.jobs[1].id, "job_recipe");
        assert_eq!(resp.jobs[1].kind, "recipe");
    }

    #[test]
    fn interactive_capture_is_advertised_only_with_a_configured_hold_slot() {
        // The master switch for the whole lane, and it is CONFIGURATION, not a
        // constant: a builder with no hold slot cannot make a held guest
        // reachable, so it must not take holds. Claiming one would burn a full
        // build and then fail at `hold-ready` with `builder_slot_not_registered`.
        // Configuring the slot is the operator act that pairs with registering
        // its public origin in ato-api, so the two sides turn on together.
        let mut cfg = test_cfg();
        cfg.hold_slot = None;
        assert!(
            !supported_job_kinds(&cfg).contains(&wizard_wire::JOB_KIND_INTERACTIVE_CAPTURE),
            "a builder with no hold slot must not advertise the interactive lane"
        );
        // The five always-on lanes stay exactly as they were.
        assert_eq!(
            supported_job_kinds(&cfg),
            [
                "recipe",
                "dockerfile_import",
                "oci_image_import",
                "compose_import",
                "source_materialize"
            ]
        );

        cfg.hold_slot = Some(HoldSlotConfig {
            builder_id: "builder-1".into(),
            slot_id: "slot-3".into(),
            proxy_listen: "127.0.0.1:8500".parse().expect("addr"),
        });
        assert!(
            supported_job_kinds(&cfg).contains(&wizard_wire::JOB_KIND_INTERACTIVE_CAPTURE),
            "a configured hold slot must advertise the interactive lane"
        );
    }

    #[test]
    fn a_partial_hold_slot_configuration_is_refused_at_startup() {
        // Half a registration is an operator error, not a degraded mode.
        let only_builder = |name: &str| match name {
            "--builder-id" => Some("builder-1".to_string()),
            _ => None,
        };
        assert!(hold_slot_from(&only_builder).is_err());

        let missing_listen = |name: &str| match name {
            "--builder-id" => Some("builder-1".to_string()),
            "--slot-id" => Some("slot-3".to_string()),
            _ => None,
        };
        assert!(hold_slot_from(&missing_listen).is_err());

        let bad_listen = |name: &str| match name {
            "--builder-id" => Some("builder-1".to_string()),
            "--slot-id" => Some("slot-3".to_string()),
            "--hold-proxy-listen" => Some("not-an-address".to_string()),
            _ => None,
        };
        assert!(hold_slot_from(&bad_listen).is_err());

        let none = |_: &str| None;
        assert!(matches!(hold_slot_from(&none), Ok(None)));
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
    fn parse_compose_import_params_requires_compose_yaml_and_rejects_unknown() {
        // compose_yaml required + non-empty.
        assert!(parse_compose_import_params(None).is_err());
        assert!(parse_compose_import_params(Some(&serde_json::json!({}))).is_err());
        assert!(
            parse_compose_import_params(Some(&serde_json::json!({ "compose_yaml": "  " })))
                .is_err()
        );
        // Happy path: compose_yaml + optional readiness path.
        let ok = parse_compose_import_params(Some(&serde_json::json!({
            "compose_yaml": "services:\n  web:\n    image: x:1\n    ports: ['80:80']\n",
            "readiness_http_path": "/health",
        })))
        .unwrap();
        assert!(ok.compose_yaml.contains("image: x:1"));
        assert_eq!(ok.readiness_http_path.as_deref(), Some("/health"));
        assert!(ok.rootfs_size_mib.is_none()); // absent ⇒ the COMPOSE_ROOTFS_FLOOR default
        assert!(ok.boot_timeout_s.is_none()); // absent ⇒ the backend env/default
        // rootfs_size_mib accepted within 1..=8192; over-cap / zero rejected.
        let sized = parse_compose_import_params(Some(&serde_json::json!({
            "compose_yaml": "services:\n  web:\n    image: x:1\n    ports: ['80:80']\n",
            "rootfs_size_mib": 8192,
        })))
        .unwrap();
        assert_eq!(sized.rootfs_size_mib, Some(8192));
        for bad in [8193, 0] {
            assert!(
                parse_compose_import_params(Some(&serde_json::json!({
                    "compose_yaml": "services:\n  web:\n    image: x:1\n    ports: ['80:80']\n",
                    "rootfs_size_mib": bad,
                })))
                .is_err(),
                "rootfs_size_mib={bad} must reject"
            );
        }
        // boot_timeout_s accepted within 1..=600; over-cap / zero rejected.
        let timed = parse_compose_import_params(Some(&serde_json::json!({
            "compose_yaml": "services:\n  web:\n    image: x:1\n    ports: ['80:80']\n",
            "boot_timeout_s": 300,
        })))
        .unwrap();
        assert_eq!(timed.boot_timeout_s, Some(300));
        for bad in [601, 0] {
            assert!(
                parse_compose_import_params(Some(&serde_json::json!({
                    "compose_yaml": "services:\n  web:\n    image: x:1\n    ports: ['80:80']\n",
                    "boot_timeout_s": bad,
                })))
                .is_err(),
                "boot_timeout_s={bad} must reject"
            );
        }
        // Unknown key rejected fail-closed.
        assert!(
            parse_compose_import_params(Some(&serde_json::json!({
                "compose_yaml": "services: {}",
                "surprise": true
            })))
            .is_err()
        );
        // Over-size compose rejected.
        let huge = "x".repeat(MAX_COMPOSE_YAML_BYTES + 1);
        assert!(
            parse_compose_import_params(Some(&serde_json::json!({ "compose_yaml": huge })))
                .is_err()
        );
    }

    /// The per-job rootfs override is ONE bound and ONE parser shared by the
    /// single-image import lanes (the compose lane's own coverage is above):
    /// `dockerfile_import` and `oci_image_import` accept the same range and
    /// reject the same shapes fail-closed. Absent ⇒ `None` ⇒ the builder config
    /// default at the call site (no compose floor — these pack one image).
    #[test]
    fn single_image_import_lanes_share_the_rootfs_size_mib_bound() {
        // Absent ⇒ None on both lanes.
        assert!(parse_import_params(None).unwrap().rootfs_size_mib.is_none());
        assert!(
            parse_oci_import_params(Some(&serde_json::json!({ "image": "redis:7" })))
                .unwrap()
                .rootfs_size_mib
                .is_none()
        );
        // Accepted across the whole 1..=MAX range, boundaries included.
        for good in [1u64, 1024, 4096, MAX_ROOTFS_SIZE_MIB as u64] {
            assert_eq!(
                parse_import_params(Some(&serde_json::json!({ "rootfs_size_mib": good })))
                    .unwrap()
                    .rootfs_size_mib,
                Some(good as u32),
                "dockerfile_import must accept rootfs_size_mib={good}"
            );
            assert_eq!(
                parse_oci_import_params(Some(&serde_json::json!({
                    "image": "redis:7",
                    "rootfs_size_mib": good,
                })))
                .unwrap()
                .rootfs_size_mib,
                Some(good as u32),
                "oci_image_import must accept rootfs_size_mib={good}"
            );
        }
        // Rejected fail-closed: zero, over-cap, negative, fractional, string,
        // bool, null — nothing is silently coerced or ignored. The needle is the
        // PARSER's own message, not the unknown-key message (which interpolates
        // the key name and would pass even on an unwired lane).
        let needle =
            format!("params.rootfs_size_mib must be an integer in 1..={MAX_ROOTFS_SIZE_MIB}");
        for bad in [
            serde_json::json!(0),
            serde_json::json!(MAX_ROOTFS_SIZE_MIB as u64 + 1),
            serde_json::json!(-1),
            serde_json::json!(1024.5),
            serde_json::json!("1024"),
            serde_json::json!(true),
            serde_json::json!(null),
        ] {
            let err =
                parse_import_params(Some(&serde_json::json!({ "rootfs_size_mib": bad.clone() })))
                    .unwrap_err();
            assert!(
                err.contains(&needle),
                "dockerfile_import rootfs_size_mib={bad} must reject: {err}"
            );
            let err = parse_oci_import_params(Some(&serde_json::json!({
                "image": "redis:7",
                "rootfs_size_mib": bad.clone(),
            })))
            .unwrap_err();
            assert!(
                err.contains(&needle),
                "oci_image_import rootfs_size_mib={bad} must reject: {err}"
            );
        }
    }

    #[test]
    fn parse_import_params_accepts_files_under_ephemeral_mounts() {
        // The unified contract: seed files live INSIDE an ephemeral_mounts
        // entry — the resolved job shape a Store recipe translates to.
        let params = serde_json::json!({
            "dockerfile_path": "Dockerfile",
            "ephemeral_mounts": [{
                "path": "/config",
                "seed": "copy-up",
                "size_mib": 16,
                "files": [{ "path": "config.yml", "source": "recipe/config.yml", "if_missing": true }]
            }]
        });
        let out = parse_import_params(Some(&params)).unwrap();
        assert_eq!(out.ephemeral_mounts.len(), 1);
        let m = &out.ephemeral_mounts[0];
        assert_eq!(m.path, "/config");
        assert_eq!(m.seed, EphemeralMountSeed::CopyUp);
        assert_eq!(m.size_mib, Some(16));
        assert_eq!(m.source, EphemeralMountSource::Explicit);
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].path, "config.yml");
        assert_eq!(m.files[0].source_path, "recipe/config.yml");
        assert!(m.files[0].if_missing);
        // The digest is a build-time output, never a parse input.
        assert!(m.files[0].source_digest.is_empty());
    }

    #[test]
    fn parse_import_params_pixel_rfb_port_is_strictly_bounded() {
        // Pixel Stream v1: same strict u16 discipline as port_override — a
        // malformed opt-in must fail closed, never degrade to a Web build.
        let ok = serde_json::json!({ "pixel_rfb_port": 5900 });
        assert_eq!(
            parse_import_params(Some(&ok)).unwrap().pixel_rfb_port,
            Some(5900)
        );
        assert_eq!(
            parse_import_params(None).unwrap().pixel_rfb_port,
            None,
            "absent param stays a Web-only import"
        );
        for bad in [
            serde_json::json!({ "pixel_rfb_port": 0 }),
            serde_json::json!({ "pixel_rfb_port": 65536 }),
            serde_json::json!({ "pixel_rfb_port": -1 }),
            serde_json::json!({ "pixel_rfb_port": "5900" }),
            serde_json::json!({ "pixel_rfb_port": 5900.5 }),
        ] {
            let err = parse_import_params(Some(&bad)).unwrap_err();
            assert!(err.contains("pixel_rfb_port"), "{err}");
        }
    }

    #[test]
    fn pixel_import_contract_seals_the_validated_endpoint_pair() {
        // None keeps the Web-only contract byte-identical: no requirement, no
        // endpoints (the legacy ports projection stays authoritative).
        let (requirement, endpoints) = pixel_import_contract(None, 8080, "/health").unwrap();
        assert!(requirement.is_none());
        assert!(endpoints.is_empty());

        // The opted-in pair: host_internal app_http readiness + guest_private
        // first_frame RFB, exactly what the runner's restore gate requires.
        let (requirement, endpoints) = pixel_import_contract(Some(5900), 8080, "/health").unwrap();
        let requirement = requirement.expect("pixel requirement");
        assert_eq!(requirement.kind, SessionSurfaceKind::PixelStream);
        assert_eq!(
            requirement.profiles.as_deref(),
            Some(&[PIXEL_STREAM_PROFILE.to_string()][..])
        );
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].role, EndpointRole::AppHttp);
        assert_eq!(endpoints[0].exposure, EndpointExposure::HostInternal);
        assert_eq!(endpoints[0].port, 8080);
        assert_eq!(
            endpoints[0].readiness,
            EndpointReadiness::HttpGet {
                path: "/health".to_string()
            }
        );
        assert_eq!(endpoints[1].role, EndpointRole::PixelRfb);
        assert_eq!(endpoints[1].exposure, EndpointExposure::GuestPrivate);
        assert_eq!(endpoints[1].port, 5900);
        assert_eq!(endpoints[1].readiness, EndpointReadiness::FirstFrame);

        // The RFB endpoint is guest-private and the app port is public — the
        // same port cannot carry both contracts.
        let err = pixel_import_contract(Some(8080), 8080, "/health").unwrap_err();
        assert!(err.contains("collides"), "{err}");
    }

    #[test]
    fn parse_import_params_rejects_the_retired_seed_mounts_param() {
        // The temporary Phase-1.5 split param must be an UNKNOWN param — a stale
        // producer still sending it fails closed instead of silently dropping
        // its seed files.
        let bad = serde_json::json!({
            "ephemeral_seed_mounts": [{ "path": "/config", "seed": "copy-up", "files": [] }]
        });
        let err = parse_import_params(Some(&bad)).unwrap_err();
        assert!(
            err.contains("unknown dockerfile_import param")
                && err.contains("ephemeral_seed_mounts"),
            "{err}"
        );
        // `files` is accepted ONLY under an ephemeral_mounts entry, not top-level.
        let bad = serde_json::json!({ "files": [{ "path": "x", "source": "y" }] });
        let err = parse_import_params(Some(&bad)).unwrap_err();
        assert!(err.contains("unknown dockerfile_import param"), "{err}");
    }

    #[test]
    fn parse_import_params_rejects_malformed_mount_files() {
        for bad in [
            // digest forgery: source_digest is never a parse input
            serde_json::json!({ "ephemeral_mounts": [{ "path": "/c", "seed": "empty", "files": [{ "path": "y", "source": "x", "source_digest": "blake3:00" }] }] }),
            serde_json::json!({ "ephemeral_mounts": [{ "path": "/c", "seed": "empty", "files": [{ "source": "x" }] }] }),
            serde_json::json!({ "ephemeral_mounts": [{ "path": "/c", "seed": "empty", "files": [{ "path": "y" }] }] }),
            serde_json::json!({ "ephemeral_mounts": [{ "path": "/c", "seed": "empty", "files": [{ "path": "y", "source": "x", "oops": true }] }] }),
            // dest escaping the mount / absolute source (single structural gate)
            serde_json::json!({ "ephemeral_mounts": [{ "path": "/c", "seed": "empty", "files": [{ "path": "../y", "source": "x" }] }] }),
            serde_json::json!({ "ephemeral_mounts": [{ "path": "/c", "seed": "empty", "files": [{ "path": "y", "source": "/etc/passwd" }] }] }),
            // duplicate destination within one mount
            serde_json::json!({ "ephemeral_mounts": [{ "path": "/c", "seed": "empty", "files": [{ "path": "y", "source": "a" }, { "path": "y", "source": "b" }] }] }),
        ] {
            assert!(parse_import_params(Some(&bad)).is_err(), "{bad}");
        }
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
        assert_eq!(
            resp.jobs[0].recipe_toml.as_deref(),
            Some("schema_version = \"0.3\"\ndefault_target = \"app\"\n")
        );
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
        assert!(
            err.1.contains("not supported by Ready-State builder v1"),
            "{}",
            err.1
        );
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
        assert_eq!(
            sup.env_map.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai_api_key")
        );
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
            hold_slot: None,
        }
    }

    fn import_job(kind: &str, params: Option<serde_json::Value>) -> ClaimedJob {
        ClaimedJob {
            pinned_source: None,
            id: "job_x".into(),
            capsule_id: "cap_x".into(),
            target_label: "app".into(),
            profile: "default".into(),
            source: Some(ClaimedSource {
                github_owner: "acme".into(),
                github_repo: "app".into(),
                commit_sha: "a".repeat(40),
                subdirectory: None,
            }),
            recipe_toml: None,
            kind: kind.into(),
            params,
            execution_contract: None,
            execution_id: None,
            execution_identity_schema: None,
            // §3.1: absent for every non-wizard kind — which is every kind this
            // helper builds.
            wire_contract_version: None,
            submission_attempt_id: None,
            worker_claim_id: None,
            lease_token: None,
            lease_expires_at: None,
        }
    }

    #[test]
    fn unknown_job_kind_fails_closed_at_claim_kind() {
        // Server/daemon contract skew (the claim advertised supported_kinds): an
        // unknown kind never guesses a lane — ack failed at stage claim_kind.
        let err = produce_build(
            &test_cfg(),
            &import_job("oci_image", None),
            Path::new("/nonexistent"),
        )
        .unwrap_err();
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
            let err = produce_build(
                &test_cfg(),
                &import_job("dockerfile_import", Some(bad.clone())),
                Path::new("/nonexistent"),
            )
            .unwrap_err();
            assert_eq!(err.0, "eligibility", "{bad}");
        }
    }

    #[test]
    fn import_params_parse_defaults_and_full_shape() {
        // Absent or null ⇒ all defaults (dockerfile_path = "Dockerfile").
        assert_eq!(
            parse_import_params(None).unwrap(),
            DockerfileImportParams::default()
        );
        assert_eq!(
            parse_import_params(Some(&serde_json::Value::Null)).unwrap(),
            DockerfileImportParams::default()
        );
        assert_eq!(
            parse_import_params(None).unwrap().dockerfile_path,
            "Dockerfile"
        );
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
        assert_eq!(
            parse_import_params(Some(&v))
                .unwrap()
                .readiness_http_path
                .as_deref(),
            Some(max.as_str())
        );
        // ato#1024: the legacy string "tmpfs" maps image VOLUMEs uncapped.
        let v = serde_json::json!({ "volumes": "tmpfs" });
        assert_eq!(
            parse_import_params(Some(&v)).unwrap().volumes,
            VolumePolicy::Tmpfs { size_mib: None }
        );
        assert_eq!(
            parse_import_params(None).unwrap().volumes,
            VolumePolicy::Reject
        );
        // Phase 1: the structured object form carries a size.
        let v = serde_json::json!({ "volumes": { "mode": "tmpfs", "size_mib": 512 } });
        assert_eq!(
            parse_import_params(Some(&v)).unwrap().volumes,
            VolumePolicy::Tmpfs {
                size_mib: Some(512)
            }
        );
        let v = serde_json::json!({ "volumes": { "mode": "tmpfs" } });
        assert_eq!(
            parse_import_params(Some(&v)).unwrap().volumes,
            VolumePolicy::Tmpfs { size_mib: None }
        );
        for bad in [
            serde_json::json!({"volumes": "rw"}),
            serde_json::json!({"volumes": true}),
            serde_json::json!({"volumes": null}),
            serde_json::json!({"volumes": {"mode": "rw"}}),
            serde_json::json!({"volumes": {"size_mib": 8}}), // missing mode
            serde_json::json!({"volumes": {"mode": "tmpfs", "x": 1}}), // unknown field
            serde_json::json!({"volumes": {"mode": "tmpfs", "size_mib": 0}}),
        ] {
            assert!(
                parse_import_params(Some(&bad))
                    .unwrap_err()
                    .contains("volumes"),
                "{bad}"
            );
        }
        // Phase 1: explicit ephemeral_mounts parse with seed + size.
        let v = serde_json::json!({ "ephemeral_mounts": [
            { "path": "/config", "seed": "copy-up", "size_mib": 16 },
            { "path": "/downloads", "seed": "empty", "size_mib": 512 },
        ]});
        let m = parse_import_params(Some(&v)).unwrap().ephemeral_mounts;
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].path, "/config");
        assert_eq!(m[0].seed, EphemeralMountSeed::CopyUp);
        assert_eq!(m[0].size_mib, Some(16));
        assert_eq!(m[0].source, EphemeralMountSource::Explicit);
        assert_eq!(m[1].seed, EphemeralMountSeed::Empty);
        // size_mib is optional on an explicit mount (uncapped tmpfs).
        let v = serde_json::json!({ "ephemeral_mounts": [{ "path": "/x", "seed": "empty" }] });
        assert_eq!(
            parse_import_params(Some(&v)).unwrap().ephemeral_mounts[0].size_mib,
            None
        );
        // ato#1026: host_bind_relay is a strict bool.
        assert!(
            parse_import_params(Some(&serde_json::json!({"host_bind_relay": true})))
                .unwrap()
                .host_bind_relay
        );
        assert!(!parse_import_params(None).unwrap().host_bind_relay);
        for bad in [
            serde_json::json!({"host_bind_relay": "yes"}),
            serde_json::json!({"host_bind_relay": 1}),
        ] {
            assert!(
                parse_import_params(Some(&bad))
                    .unwrap_err()
                    .contains("host_bind_relay")
            );
        }
    }

    #[test]
    fn import_params_reject_every_out_of_bounds_shape() {
        // The same strict bounds the ato-api enqueue validation enforces (ato#1002):
        // unknown keys, non-object params, path escape/length, port range/type,
        // readiness shape/length — each rejected with an actionable reason.
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (serde_json::json!({ "extra": 1 }), "unknown"),
            (
                serde_json::json!({ "dockerfile_path": "/abs/Dockerfile" }),
                "relative",
            ),
            (
                serde_json::json!({ "dockerfile_path": "a/../../Dockerfile" }),
                "..",
            ),
            (
                serde_json::json!({ "dockerfile_path": "x".repeat(201) }),
                "200",
            ),
            (serde_json::json!({ "dockerfile_path": 7 }), "string"),
            (serde_json::json!({ "port_override": 0 }), "1..65535"),
            (serde_json::json!({ "port_override": 65536 }), "1..65535"),
            (serde_json::json!({ "port_override": "8080" }), "integer"),
            (serde_json::json!({ "port_override": 8080.5 }), "integer"),
            (
                serde_json::json!({ "readiness_http_path": "health" }),
                "start with '/'",
            ),
            // 200, not the contract draft's 256: the ack's healthcheck_url_path
            // schema (ato-api, strict) caps at 200 — see parse_import_params.
            (
                serde_json::json!({ "readiness_http_path": format!("/{}", "x".repeat(200)) }),
                "200",
            ),
            (serde_json::json!({ "readiness_http_path": 1 }), "string"),
            // Shell-injection gate: the value lands in the builder-host pack
            // script, so NUL/CR/LF are rejected fail-closed (reject_control_chars).
            (
                serde_json::json!({ "readiness_http_path": "/x\nid > /tmp/pwned\n#" }),
                "newline",
            ),
            (
                serde_json::json!({ "readiness_http_path": "/x\rid" }),
                "newline",
            ),
            (
                serde_json::json!({ "readiness_http_path": "/x\u{0}y" }),
                "NUL",
            ),
            (serde_json::json!([1, 2]), "object"),
        ];
        for (v, needle) in cases {
            let err = parse_import_params(Some(&v)).unwrap_err();
            assert!(err.contains(needle), "{v}: {err}");
        }
    }

    #[test]
    fn ephemeral_mounts_reject_bad_shapes_and_paths() {
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (serde_json::json!({ "ephemeral_mounts": "nope" }), "array"),
            (serde_json::json!({ "ephemeral_mounts": [1] }), "object"),
            (
                serde_json::json!({ "ephemeral_mounts": [{ "seed": "empty" }] }),
                "requires \"path\"",
            ),
            (
                serde_json::json!({ "ephemeral_mounts": [{ "path": "/x" }] }),
                "requires \"seed\"",
            ),
            (
                serde_json::json!({ "ephemeral_mounts": [{ "path": "/x", "seed": "rw" }] }),
                "empty",
            ),
            (
                serde_json::json!({ "ephemeral_mounts": [{ "path": "/x", "seed": "empty", "extra": 1 }] }),
                "unknown",
            ),
            (
                serde_json::json!({ "ephemeral_mounts": [{ "path": "/x", "seed": "empty", "size_mib": 0 }] }),
                ">= 1",
            ),
            // Path validity is enforced fail-closed at parse (validate_ephemeral_mounts).
            (
                serde_json::json!({ "ephemeral_mounts": [{ "path": "rel", "seed": "empty" }] }),
                "ephemeral mount",
            ),
            (
                serde_json::json!({ "ephemeral_mounts": [{ "path": "/etc/app", "seed": "empty" }] }),
                "tmpfs-shadow",
            ),
            (
                serde_json::json!({ "ephemeral_mounts": [{ "path": "/x;rm", "seed": "empty" }] }),
                "characters outside",
            ),
            // Duplicate + nested among the explicit set are rejected at parse.
            (
                serde_json::json!({ "ephemeral_mounts": [
                    { "path": "/data", "seed": "empty" }, { "path": "/data", "seed": "empty" }
                ]}),
                "duplicate",
            ),
            (
                serde_json::json!({ "ephemeral_mounts": [
                    { "path": "/data", "seed": "empty" }, { "path": "/data/sub", "seed": "empty" }
                ]}),
                "overlap",
            ),
        ];
        for (v, needle) in cases {
            let err = parse_import_params(Some(&v)).unwrap_err();
            assert!(err.contains(needle), "{v}: {err}");
        }
    }

    #[test]
    fn ephemeral_mount_caps_enforced_fail_closed() {
        // Per-mount cap: an explicit mount over the cap is rejected; the volume
        // policy size is capped too; the total sum is bounded.
        let mk = |mounts: Vec<EphemeralMountSpec>, vol: VolumePolicy| DockerfileImportParams {
            ephemeral_mounts: mounts,
            volumes: vol,
            ..DockerfileImportParams::default()
        };
        let m = |path: &str, size: Option<u32>| EphemeralMountSpec {
            path: path.into(),
            seed: EphemeralMountSeed::Empty,
            size_mib: size,
            source: EphemeralMountSource::Explicit,
            files: Vec::new(),
        };
        // Within caps ⇒ ok.
        assert!(
            enforce_ephemeral_mount_caps(
                &mk(
                    vec![m("/a", Some(1000)), m("/b", Some(1000))],
                    VolumePolicy::Reject
                ),
                2048,
                8192
            )
            .is_ok()
        );
        // Per-mount over cap ⇒ rejected.
        let err = enforce_ephemeral_mount_caps(
            &mk(vec![m("/a", Some(4096))], VolumePolicy::Reject),
            2048,
            8192,
        )
        .unwrap_err();
        assert!(
            err.contains("per-mount cap") && err.contains("2048"),
            "{err}"
        );
        // Volume policy size over cap ⇒ rejected.
        let err = enforce_ephemeral_mount_caps(
            &mk(
                vec![],
                VolumePolicy::Tmpfs {
                    size_mib: Some(4096),
                },
            ),
            2048,
            8192,
        )
        .unwrap_err();
        assert!(
            err.contains("volumes.size_mib") && err.contains("per-mount"),
            "{err}"
        );
        // Total over cap (each within per-mount) ⇒ rejected.
        let err = enforce_ephemeral_mount_caps(
            &mk(
                vec![
                    m("/a", Some(2000)),
                    m("/b", Some(2000)),
                    m("/c", Some(2000)),
                    m("/d", Some(2000)),
                    m("/e", Some(2000)),
                ],
                VolumePolicy::Reject,
            ),
            2048,
            8192,
        )
        .unwrap_err();
        assert!(
            err.contains("total ephemeral mount size") && err.contains("8192"),
            "{err}"
        );
        // Uncapped (None) mounts contribute nothing to the total.
        assert!(
            enforce_ephemeral_mount_caps(
                &mk(
                    vec![m("/a", None), m("/b", None)],
                    VolumePolicy::Tmpfs { size_mib: None }
                ),
                2048,
                8192
            )
            .is_ok()
        );
    }

    #[test]
    fn ephemeral_mount_caps_read_env_with_defaults() {
        // Defaults hold when the env is unset/garbage (a per-test process env is
        // avoided; assert the fallback values directly).
        let (per, total) = ephemeral_mount_caps();
        assert!(per >= 1 && total >= 1);
    }

    #[test]
    fn import_clone_rejects_invalid_identities_before_any_git() {
        let src = |owner: &str, repo: &str, commit: &str, sub: Option<&str>| ClaimedSource {
            github_owner: owner.into(),
            github_repo: repo.into(),
            commit_sha: commit.into(),
            subdirectory: sub.map(String::from),
        };
        let dest =
            std::env::temp_dir().join(format!("never-created-clone-dest-{}", std::process::id()));
        let full = "a".repeat(40);
        // Bad owner / repo / non-pinned commit / escaping subdir — each fails BEFORE
        // any git command or directory creation (same gates as materialize_source;
        // only the capsule.toml requirement is deliberately absent).
        assert!(
            clone_pinned_source(&src("bad owner", "app", &full, None), &dest)
                .unwrap_err()
                .contains("owner")
        );
        assert!(
            clone_pinned_source(&src("acme", "bad repo", &full, None), &dest)
                .unwrap_err()
                .contains("repo")
        );
        assert!(
            clone_pinned_source(&src("acme", "app", "main", None), &dest)
                .unwrap_err()
                .contains("non-pinned")
        );
        assert!(
            clone_pinned_source(&src("acme", "app", &full[..12], None), &dest)
                .unwrap_err()
                .contains("non-pinned")
        );
        let err =
            clone_pinned_source(&src("acme", "app", &full, Some("../up")), &dest).unwrap_err();
        assert!(err.contains("subdirectory"), "{err}");
        assert!(!dest.exists(), "validation must reject before any clone IO");
    }

    // ── ato#1028: oci_image_import claim + params ────────────────────────────

    #[test]
    fn parses_an_oci_image_import_claim() {
        // An oci_image_import job carries kind = "oci_image_import" + params (image).
        let body = serde_json::json!({
            "jobs": [{
                "id": "job_5", "capsule_id": "cap_5",
                "source": { "source_kind": "github", "github_owner": "acme", "github_repo": "app", "commit_sha": "e".repeat(40), "subdirectory": null },
                "recipe_toml": null,
                "kind": "oci_image_import",
                "params": { "image": "ghcr.io/alexta69/metube:latest", "port_override": 8081, "readiness_http_path": "/", "ephemeral_mounts": ["/downloads"] },
                "target_label": "app", "profile": "default", "claim_expires_at": "2026-01-01T00:00:00.000Z"
            }]
        });
        let resp: ClaimResponse = serde_json::from_value(body).unwrap();
        assert_eq!(resp.jobs[0].kind, "oci_image_import");
        let p = parse_oci_import_params(resp.jobs[0].params.as_ref()).unwrap();
        assert_eq!(p.image, "ghcr.io/alexta69/metube:latest");
        assert_eq!(p.platform, "linux/amd64");
        assert_eq!(p.port_override, Some(8081));
        assert_eq!(p.readiness_http_path.as_deref(), Some("/"));
        assert_eq!(
            p.volumes,
            snapshot::docker_import::VolumePolicy::Tmpfs { size_mib: None }
        );
        assert!(!p.host_bind_relay);
    }

    #[test]
    fn oci_import_params_parse_full_shape_and_defaults() {
        // Minimal: just image ⇒ defaults (platform linux/amd64, Reject volumes, no relay).
        let p = parse_oci_import_params(Some(&serde_json::json!({ "image": "redis:7" }))).unwrap();
        assert_eq!(p.image, "redis:7");
        assert_eq!(p.platform, "linux/amd64");
        assert!(p.port_override.is_none());
        assert!(p.readiness_http_path.is_none());
        assert_eq!(p.volumes, snapshot::docker_import::VolumePolicy::Reject);
        assert!(!p.host_bind_relay);
        // Full object with explicit platform + relay + empty ephemeral_mounts (Reject).
        let v = serde_json::json!({
            "image": "ghcr.io/x/y@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "platform": "linux/amd64",
            "port_override": 3000,
            "readiness_http_path": "/health",
            "host_bind_relay": true,
            "ephemeral_mounts": [],
        });
        let p = parse_oci_import_params(Some(&v)).unwrap();
        assert!(is_digest_pinned(&p.image));
        assert_eq!(p.port_override, Some(3000));
        assert_eq!(p.readiness_http_path.as_deref(), Some("/health"));
        assert!(p.host_bind_relay);
        assert_eq!(
            p.volumes,
            snapshot::docker_import::VolumePolicy::Reject,
            "empty ephemeral_mounts keeps the fail-closed gate"
        );
    }

    fn is_digest_pinned(image: &str) -> bool {
        image.contains("@sha256:")
    }

    #[test]
    fn oci_import_params_reject_every_out_of_bounds_shape() {
        let cases: Vec<(serde_json::Value, &str)> = vec![
            // image is REQUIRED.
            (
                serde_json::json!({ "port_override": 3000 }),
                "image is required",
            ),
            (serde_json::json!({ "image": "" }), "empty"),
            (serde_json::json!({ "image": "-rm" }), "'-'"),
            (
                serde_json::json!({ "image": "metube;rm" }),
                "[A-Za-z0-9._/:@-]",
            ),
            (serde_json::json!({ "image": 7 }), "string"),
            // platform is linux/amd64 only in v1.
            (
                serde_json::json!({ "image": "redis:7", "platform": "linux/arm64" }),
                "linux/amd64",
            ),
            (
                serde_json::json!({ "image": "redis:7", "platform": 1 }),
                "string",
            ),
            // shared port/readiness bounds.
            (
                serde_json::json!({ "image": "redis:7", "port_override": 0 }),
                "1..65535",
            ),
            (
                serde_json::json!({ "image": "redis:7", "port_override": 65536 }),
                "1..65535",
            ),
            (
                serde_json::json!({ "image": "redis:7", "readiness_http_path": "health" }),
                "start with '/'",
            ),
            (
                serde_json::json!({ "image": "redis:7", "readiness_http_path": "/x\nid\n#" }),
                "newline",
            ),
            // ephemeral_mounts shape.
            (
                serde_json::json!({ "image": "redis:7", "ephemeral_mounts": "tmpfs" }),
                "array",
            ),
            (
                serde_json::json!({ "image": "redis:7", "ephemeral_mounts": ["data"] }),
                "absolute",
            ),
            (
                serde_json::json!({ "image": "redis:7", "ephemeral_mounts": ["/"] }),
                "absolute",
            ),
            (
                serde_json::json!({ "image": "redis:7", "ephemeral_mounts": ["/a", "/a"] }),
                "duplicated",
            ),
            (
                serde_json::json!({ "image": "redis:7", "ephemeral_mounts": [1] }),
                "strings",
            ),
            // host_bind_relay strict bool.
            (
                serde_json::json!({ "image": "redis:7", "host_bind_relay": "yes" }),
                "boolean",
            ),
            // unknown key + non-object + null.
            (
                serde_json::json!({ "image": "redis:7", "extra": 1 }),
                "unknown",
            ),
            (serde_json::json!("not-an-object"), "object"),
            (serde_json::json!(null), "required"),
        ];
        for (v, needle) in cases {
            let params = if v.is_null() { None } else { Some(&v) };
            let err = parse_oci_import_params(params).unwrap_err();
            assert!(err.contains(needle), "{v}: {err}");
        }
        // Absent params (None) also fail — image has no default.
        assert!(
            parse_oci_import_params(None)
                .unwrap_err()
                .contains("required")
        );
    }

    #[test]
    fn oci_import_params_validation_failures_fail_at_eligibility() {
        // Params are validated BEFORE any pull/build work, so a bad-params job acks
        // failed at eligibility without touching the network.
        for bad in [
            serde_json::json!({ "port_override": 3000 }), // missing image
            serde_json::json!({ "image": "-rm -rf" }),
            serde_json::json!({ "image": "redis:7", "platform": "linux/arm64" }),
            serde_json::json!({ "image": "redis:7", "unknown_key": true }),
            serde_json::json!("not-an-object"),
        ] {
            let err = produce_build(
                &test_cfg(),
                &import_job("oci_image_import", Some(bad.clone())),
                Path::new("/nonexistent"),
            )
            .unwrap_err();
            assert_eq!(err.0, "eligibility", "{bad}");
        }
    }

    #[test]
    fn oci_image_import_is_a_supported_kind_not_a_claim_kind_failure() {
        // The dispatcher routes "oci_image_import" to its producer (which then fails
        // at eligibility on absent params here), NOT to the unknown-kind claim_kind
        // path — proving the kind is advertised/handled.
        let err = produce_build(
            &test_cfg(),
            &import_job("oci_image_import", None),
            Path::new("/nonexistent"),
        )
        .unwrap_err();
        assert_eq!(err.0, "eligibility");
        assert!(err.1.contains("image"), "{}", err.1);
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
        let (exec, rc) =
            sealed_identity(Some("real-declared-id"), Some("blake3:rc".into())).unwrap();
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
            assert!(
                c.len() >= 12,
                "canary {:?} is too short to gate binary artifacts",
                String::from_utf8_lossy(c)
            );
        }
    }

    #[test]
    fn l4_canaries_flag_pem_but_not_bare_provider_prefixes() {
        // A random-binary AKIA occurrence (finding 4's false-positive class) must pass…
        let binary_with_akia =
            [b"\x00\x9fAKIA\xffQ\x11 random bytes".as_slice(), &[0u8; 64]].concat();
        assert!(no_secret_scan::blob_is_clean(
            &binary_with_akia,
            L4_CANARIES
        ));
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
            hold_slot: None,
        };
        // A real (long, random) token gates: an artifact containing it is dirty.
        let cfg = mk("0123456789abcdef0123456789abcdef");
        let canaries = live_secret_canaries(&cfg);
        assert_eq!(canaries.len(), 1);
        let leaked = [b"layer bytes ".as_slice(), cfg.token.as_bytes(), b" more"].concat();
        assert!(!no_secret_scan::blob_is_clean(&leaked, &canaries));
        assert!(no_secret_scan::blob_is_clean(
            b"layer bytes without the token",
            &canaries
        ));
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
            hold_slot: None,
        };
        let cas = std::env::temp_dir().join(format!("compat-planted-token-{}", std::process::id()));
        std::fs::create_dir_all(&cas).unwrap();
        std::fs::write(cas.join("layer.bin"), format!("prefix {fake_token} suffix")).unwrap();

        let targets = no_secret_scan::ScanTargets {
            cas: Some(cas.clone()),
            ..Default::default()
        };
        let canaries = live_secret_canaries(&cfg);
        let result = no_secret_scan::scan(&targets, &canaries);
        std::fs::remove_dir_all(&cas).ok();

        assert!(
            !result.clean,
            "a planted builder token must fail the CAS scan"
        );
        assert_eq!(result.hits.len(), 1);
        // The report (what would reach logs / the failed-ack reason) must never
        // contain the token value — only the target label and file path.
        let report = format!("{result:?}");
        assert!(
            !report.contains(fake_token),
            "scan report must not print the token value"
        );
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
            surface_requirement: None,
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
            oci_import_receipt: None,
            compose_import_receipt: None,
            screenshot_png_base64: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        // A NO-BINDING recipe ack omits supervisor_build, docker_import_receipt AND
        // oci_import_receipt entirely (byte-identical vs the pre-3e-2c / pre-#1002
        // schema, which the .strict() ato-api validator requires).
        assert_eq!(
            keys,
            [
                "artifact_location",
                "artifact_manifest_hash",
                "capsule_manifest_hash",
                "declared_command",
                "execution_id",
                "healthcheck_url_path",
                "manifest_source",
                "mem_bytes",
                "no_secret_scan_clean",
                "normalized_guest_command",
                "rootfs_bytes",
                "runner_class_id",
                "snapshot_backend",
                "snapshot_codec_id",
                "snapshot_format_id",
                "synthesized_probe",
                "vmstate_bytes"
            ]
        );
        assert_eq!(obj["no_secret_scan_clean"], serde_json::json!(true));
        // #932 provenance values are enum-safe for the ato-api schema.
        assert!(matches!(
            obj["manifest_source"].as_str().unwrap(),
            "recipe_toml" | "repo_capsule_toml"
        ));
        // No placeholder identity/location fields.
        for k in ["execution_id", "runner_class_id", "artifact_location"] {
            assert_ne!(obj[k].as_str().unwrap(), "unknown");
        }
    }

    #[test]
    fn artifact_ack_carries_the_immutable_surface_requirement_when_present() {
        let surface_requirement: SessionSurfaceRequirement =
            serde_json::from_value(serde_json::json!({
                "kind": "pixel_stream",
                "profiles": ["ato.pixel-stream.v1"]
            }))
            .expect("surface requirement");
        let artifact = Artifact {
            capsule_manifest_hash: "blake3:c".into(),
            execution_id: "exec-1".into(),
            artifact_manifest_hash: "blake3:a".into(),
            runner_class_id: "rc".into(),
            snapshot_backend: "firecracker".into(),
            artifact_location: "cas://job/blake3:a".into(),
            healthcheck_url_path: "/health".into(),
            surface_requirement: Some(surface_requirement),
            no_secret_scan_clean: true,
            rootfs_bytes: 1,
            mem_bytes: 2,
            vmstate_bytes: 3,
            snapshot_format_id: SNAPSHOT_FORMAT_ID.to_string(),
            snapshot_codec_id: SNAPSHOT_CODEC_ID.to_string(),
            manifest_source: "recipe_toml".into(),
            synthesized_probe: true,
            declared_command: "app.py".into(),
            normalized_guest_command: "python3 app.py".into(),
            supervisor_build: None,
            docker_import_receipt: None,
            oci_import_receipt: None,
            compose_import_receipt: None,
            screenshot_png_base64: None,
        };

        let value = serde_json::to_value(artifact).expect("serialize artifact ack");
        assert_eq!(value["surface_requirement"]["kind"], "pixel_stream");
        assert_eq!(
            value["surface_requirement"]["profiles"],
            serde_json::json!(["ato.pixel-stream.v1"])
        );
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
            surface_requirement: None,
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
            supervisor_build: Some(SupervisorAck {
                binding_names: vec![],
            }),
            docker_import_receipt: Some(serde_json::json!({
                "importer_version": "ato-docker-import/0.1.0",
                "build_tool": "podman",
                "resolved_base_images": [{ "original_ref": "node:20", "resolved_digest": "docker.io/library/node@sha256:ab" }],
            })),
            oci_import_receipt: None,
            compose_import_receipt: None,
            screenshot_png_base64: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["manifest_source"], "dockerfile_import");
        assert!(v["docker_import_receipt"].is_object());
        assert_eq!(v["docker_import_receipt"]["build_tool"], "podman");
        // The zero-binding supervisor facet serializes as an EXPLICIT empty set —
        // present, never omitted, never null.
        assert_eq!(
            v["supervisor_build"],
            serde_json::json!({ "binding_names": [] })
        );
        // The keys are present ONLY when Some — a recipe ack (None) never carries
        // docker_import_receipt, and a dockerfile_import ack never carries
        // oci_import_receipt.
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        assert!(keys.contains(&"docker_import_receipt"));
        assert!(keys.contains(&"supervisor_build"));
        assert!(!keys.contains(&"oci_import_receipt"));
    }

    #[test]
    fn oci_image_import_ack_carries_the_oci_receipt_and_manifest_source() {
        // ato#1028: an oci_image_import ack adds oci_import_receipt (arbitrary
        // non-secret JSON) + manifest_source = "oci_image_import"; both optional
        // server-side, so recipe / dockerfile_import acks keep validating. Like a
        // dockerfile import it ALWAYS carries supervisor_build (a zero-binding
        // import is still a supervisor artifact — EMPTY name set).
        let a = Artifact {
            capsule_manifest_hash: "blake3:d".into(),
            execution_id: "sha256:i".into(),
            artifact_manifest_hash: "blake3:a".into(),
            runner_class_id: "rc".into(),
            snapshot_backend: "firecracker".into(),
            artifact_location: "cas://job/blake3:a".into(),
            healthcheck_url_path: "/".into(),
            surface_requirement: None,
            no_secret_scan_clean: true,
            rootfs_bytes: 1,
            mem_bytes: 2,
            vmstate_bytes: 3,
            manifest_source: "oci_image_import".into(),
            synthesized_probe: true,
            declared_command: "docker-entrypoint.sh".into(),
            normalized_guest_command: "docker-entrypoint.sh".into(),
            snapshot_format_id: SNAPSHOT_FORMAT_ID.to_string(),
            snapshot_codec_id: SNAPSHOT_CODEC_ID.to_string(),
            supervisor_build: Some(SupervisorAck {
                binding_names: vec![],
            }),
            docker_import_receipt: None,
            oci_import_receipt: Some(serde_json::json!({
                "importer_version": "ato-docker-import/0.1.0",
                "pull_tool": "podman",
                "image": { "original_ref": "ghcr.io/alexta69/metube:latest", "resolved_digest": "ghcr.io/alexta69/metube@sha256:ab", "platform": "linux/amd64" },
            })),
            compose_import_receipt: None,
            screenshot_png_base64: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["manifest_source"], "oci_image_import");
        assert!(v["oci_import_receipt"].is_object());
        assert_eq!(v["oci_import_receipt"]["pull_tool"], "podman");
        assert_eq!(v["oci_import_receipt"]["image"]["platform"], "linux/amd64");
        assert_eq!(
            v["supervisor_build"],
            serde_json::json!({ "binding_names": [] })
        );
        // oci_import_receipt present, docker_import_receipt absent (mutually exclusive lanes).
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        assert!(keys.contains(&"oci_import_receipt"));
        assert!(!keys.contains(&"docker_import_receipt"));
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
            surface_requirement: None,
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
            supervisor_build: Some(SupervisorAck {
                binding_names: vec!["openai_api_key".into()],
            }),
            docker_import_receipt: None,
            oci_import_receipt: None,
            compose_import_receipt: None,
            screenshot_png_base64: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(
            v["supervisor_build"],
            serde_json::json!({ "binding_names": ["openai_api_key"] })
        );
        // Still no secret value anywhere in the ack.
        assert!(
            !serde_json::to_string(&v)
                .unwrap()
                .to_lowercase()
                .contains("sk-")
        );
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
            surface_requirement: None,
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
            oci_import_receipt: None,
            compose_import_receipt: None,
            screenshot_png_base64: None,
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
                && rest.chars().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-')
                })
        }
        assert!(matches_ato_api_label_charset(
            "asf.",
            v["snapshot_format_id"].as_str().unwrap()
        ));
        assert!(matches_ato_api_label_charset(
            "asc.",
            v["snapshot_codec_id"].as_str().unwrap()
        ));
    }

    // ── SOURCE_MATERIALIZATION_SPEC: source_materialize ack payloads ─────────

    #[test]
    fn source_materialized_ack_carries_exactly_the_result_fields() {
        // SOURCE_MATERIALIZATION_SPEC §4.1: success ack = the A1v2 identity, the
        // archive byte identity, the observed sizes/count, and WHERE it is stored.
        let ok = SourceMaterializeOk {
            materialized_source_tree_hash: "sha256:aa".into(),
            source_archive_hash: "sha256:bb".into(),
            file_count: 42,
            uncompressed_bytes: 4096,
            compressed_bytes: 1024,
            object_key: "source-archives/sha256/bb".into(),
        };
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v["materialized_source_tree_hash"], "sha256:aa");
        assert_eq!(v["source_archive_hash"], "sha256:bb");
        assert_eq!(v["file_count"], 42);
        assert_eq!(v["uncompressed_bytes"], 4096);
        assert_eq!(v["compressed_bytes"], 1024);
        assert_eq!(v["object_key"], "source-archives/sha256/bb");
        // No stray fields leak into the ack.
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 6, "unexpected ack fields: {v}");
    }

    #[test]
    fn the_ack_never_carries_a_builder_local_path() {
        // The regression this replaces: the ack used to report `archive_path`,
        // a path on THIS host. It named nothing to anyone else — not to another
        // builder, not to the API, not after this process exits — so a
        // submission could never reach a third party through it.
        //
        // Asserted on the serialized ack rather than on the struct, because the
        // ack is the contract and a private field that still serializes is
        // still a leak.
        let ok = SourceMaterializeOk {
            materialized_source_tree_hash: "sha256:aa".into(),
            source_archive_hash: "sha256:bb".into(),
            file_count: 1,
            uncompressed_bytes: 1,
            compressed_bytes: 1,
            object_key: "source-archives/sha256/bb".into(),
        };
        let rendered = serde_json::to_string(&ok).unwrap();
        assert!(!rendered.contains("archive_path"));
        assert!(
            !rendered.contains('/') || !rendered.contains("/work/"),
            "the ack must carry a content-addressed key, never a host path"
        );
    }

    #[test]
    fn admissibility_failure_maps_to_terminal_blocked_repo() {
        // An A1v2 admissibility violation (here: a symlink) is a terminal blocked_repo
        // with a stable machine code; the ack carries pipeline_state/error_code/detail.
        let err = SourceMaterializeError::Inadmissible(
            capsule::foundation::blob::SourceAdmissibilityError::Symlink {
                path: std::path::PathBuf::from("link"),
            },
        );
        let fail = SourceMaterializeFail::from_materialize_error(&err);
        assert_eq!(fail.pipeline_state, "blocked_repo");
        assert_eq!(fail.error_code, "inadmissible_source_tree");
        assert!(
            fail.error_detail.contains("symlink"),
            "{}",
            fail.error_detail
        );

        let v = serde_json::to_value(&fail).unwrap();
        assert_eq!(v["pipeline_state"], "blocked_repo");
        assert_eq!(v["error_code"], "inadmissible_source_tree");
        assert!(v.get("error_detail").is_some());
        assert_eq!(
            v.as_object().unwrap().len(),
            3,
            "unexpected fail fields: {v}"
        );
    }

    #[test]
    fn archive_cap_failure_maps_to_blocked_repo() {
        // An archive-level cap violation is also terminal blocked_repo, with its own
        // machine code so the pipeline need not string-match the detail.
        let err = SourceMaterializeError::CompressedTooLarge {
            bytes: 200 * 1024 * 1024,
            limit: 100 * 1024 * 1024,
        };
        let fail = SourceMaterializeFail::from_materialize_error(&err);
        assert_eq!(fail.pipeline_state, "blocked_repo");
        assert_eq!(fail.error_code, "compressed_cap_exceeded");
    }

    #[test]
    fn archive_io_failure_is_retryable_failed_internal() {
        // An archive-side IO error is transient — failed_internal (ato-api retries it,
        // max 3), never a block.
        let err = SourceMaterializeError::Io {
            context: "write archive".into(),
            source: std::io::Error::other("disk full"),
        };
        let fail = SourceMaterializeFail::from_materialize_error(&err);
        assert_eq!(fail.pipeline_state, "failed_internal");
        assert_eq!(fail.error_code, "io_error");
    }

    #[test]
    fn builder_side_contract_skew_is_failed_internal() {
        // A source_materialize job with no server-resolved source (or a checkout
        // failure) is a builder/daemon contract skew — failed_internal, retryable.
        let fail = SourceMaterializeFail::internal(
            "source_missing",
            "source_materialize job carries no server-resolved source".into(),
        );
        assert_eq!(fail.pipeline_state, "failed_internal");
        assert_eq!(fail.error_code, "source_missing");
        let v = serde_json::to_value(&fail).unwrap();
        assert_eq!(v["pipeline_state"], "failed_internal");
    }

    // ---- attempt_v1_execution_identity / content_digest_of ----
    //
    // `expected` is always hand-built by the TEST (playing the part of a
    // control plane that already pinned a job's execution contract — no code
    // under test invents it), and every real measurement the function under
    // test performs is checked against a genuinely independent computation
    // (a real tree hash for source.digest, a direct blake3/sha256 call for
    // the layer digest).

    fn v1_placeholder_opaque_digest() -> capsule::execution_contract::OpaqueContractDigestV1 {
        capsule::execution_contract::opaque_subcontract_digest(
            capsule::execution_contract::OpaqueContractDomainV1::SourceProjection,
            &serde_json::json!({}),
        )
        .expect("placeholder opaque digest")
    }

    /// Eligibility is minted only when the pinned contract and the pinned
    /// `execution_id` actually agree.
    ///
    /// This is the whole strength of the declaration-based gate: the builder does
    /// not trust either half alone, it recomputes the canonical hash. A contract
    /// swapped under a stale id — or an id swapped under a contract — must not
    /// mint a proof that then lets a live workload be captured.
    #[test]
    fn eligibility_needs_the_contract_and_its_id_to_agree() {
        use crate::claim_eligibility::ClaimContractEligibility;
        use crate::hold_phase::EligibilitySource;

        let source_digest = content_digest_of(b"src", DigestAlgorithm::Blake3);
        let readonly_layer = content_digest_of(b"rootfs", DigestAlgorithm::Blake3);
        let contract = v1_minimal_contract(source_digest, readonly_layer);
        let real_id = contract
            .compute_execution_id()
            .expect("a well-formed contract hashes");

        // Agreeing pair ⇒ a proof (this contract declares no External State).
        let mut ok =
            ClaimContractEligibility::from_claim(Some(&contract), Some(&real_id.to_string()));
        assert!(
            ok.eligibility().is_ok(),
            "an agreeing contract/id pair with no External State must be eligible"
        );

        // Same contract, a DIFFERENT well-formed id ⇒ refused.
        let other_id = format!("blake3:{}", "b".repeat(64));
        assert_ne!(other_id, real_id.to_string());
        let mut mismatched = ClaimContractEligibility::from_claim(Some(&contract), Some(&other_id));
        assert!(
            mismatched.eligibility().is_err(),
            "a contract must never be eligible under an id that is not its own hash"
        );

        // A malformed id is refused too — never coerced into an `ExecutionId`.
        let mut malformed =
            ClaimContractEligibility::from_claim(Some(&contract), Some("not-a-blake3-id"));
        assert!(malformed.eligibility().is_err());
    }

    /// A minimal but fully valid `ExecutionContractV1`: zero dependencies,
    /// exactly one readonly layer, and every opaque facet filled with the
    /// same placeholder digest (none of them are ever measured by the
    /// function under test, so their exact value never matters — only that
    /// the contract as a whole is well-formed enough to compute a real
    /// `execution_id`).
    fn v1_minimal_contract(
        source_digest: ContentDigest,
        readonly_layer: ContentDigest,
    ) -> ExecutionContractV1 {
        use capsule::execution_contract::{
            EXECUTION_CONTRACT_V1_SCHEMA, GuestPath, GuestSurfaceContract,
            ResolvedArtifactContract, ResolvedFilesystemContract, ResolvedLaunchContract,
            ResolvedPolicyContract, ResolvedSourceContract, ResolvedTargetContract,
        };
        let placeholder = v1_placeholder_opaque_digest();
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                digest: source_digest,
                projection_digest: placeholder,
            },
            target: ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                abi: "gnu".to_string(),
                libc: None,
                observable_features: BTreeMap::new(),
            },
            runtime: ResolvedArtifactContract {
                kind: "node".to_string(),
                digest: ContentDigest::new(DigestAlgorithm::Blake3, [2u8; 32]),
                dynamic_contract_digest: placeholder,
            },
            dependencies: Vec::new(),
            build_outputs: Vec::new(),
            launch: ResolvedLaunchContract {
                argv: vec!["sh".to_string(), "-lc".to_string(), "run.sh".to_string()],
                cwd: GuestPath::parse("/workspace").unwrap(),
                process_model_digest: placeholder,
                environment: Vec::new(),
                environment_policy_digest: placeholder,
                secret_bindings: Vec::new(),
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: ContentDigest::new(DigestAlgorithm::Blake3, [7u8; 32]),
                topology_digest: placeholder,
                readonly_layers: vec![readonly_layer],
                writable_paths: Vec::new(),
            },
            policy: ResolvedPolicyContract {
                network_digest: placeholder,
                capability_digest: placeholder,
                filesystem_digest: placeholder,
            },
            guest_surface: GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "ato-guest/v1".to_string(),
                port: None,
                features: Vec::new(),
            },
            external_state: Vec::new(),
        }
    }

    #[test]
    fn attempt_v1_execution_identity_refuses_on_unmeasured_facet_even_when_the_three_real_measurements_match()
     {
        let src = tempfile::tempdir().expect("tempdir");
        std::fs::write(src.path().join("main.js"), b"console.log(1);\n").expect("write source");
        let source_hash =
            materialized_source_tree_hash(src.path()).expect("hash checked-out source tree");
        let source_digest = ContentDigest::try_from(source_hash).expect("parse source digest");

        let rootfs_bytes = b"the-sealed-rootfs-bytes".to_vec();
        let readonly_layer = content_digest_of(&rootfs_bytes, DigestAlgorithm::Blake3);

        let expected = v1_minimal_contract(source_digest, readonly_layer);

        // The 3 G0-2 facets this function measures all genuinely agree (real
        // source tree hash, zero dependencies, and the real rootfs layer
        // digest) — proving those measurements are wired correctly — yet
        // `.finalize()` still legitimately refuses because
        // `source.projection_digest` (checked right after `source.digest`)
        // has no measurement producer. That refusal must surface as
        // `Ok(None)`, never `Err`: if any of the 3 real measurements were
        // wrong, this would instead see a `FacetMismatch` surfaced as `Err`.
        let result = attempt_v1_execution_identity(&expected, src.path(), &rootfs_bytes).expect(
            "an UnmeasuredFacet refusal must be reported as Ok(None), never Err — if this \
             instead errors, one of the 3 real measurements disagreed with the fixture",
        );
        assert!(result.is_none());
    }

    #[test]
    fn attempt_v1_execution_identity_errors_on_a_genuine_source_digest_mismatch() {
        let src = tempfile::tempdir().expect("tempdir");
        std::fs::write(src.path().join("main.js"), b"console.log(1);\n").expect("write source");

        // Deliberately wrong: does not match the real tree hash of `src`.
        let wrong_source_digest = ContentDigest::new(DigestAlgorithm::Sha256, [0xEE; 32]);
        let rootfs_bytes = b"the-sealed-rootfs-bytes".to_vec();
        let readonly_layer = content_digest_of(&rootfs_bytes, DigestAlgorithm::Blake3);
        let expected = v1_minimal_contract(wrong_source_digest, readonly_layer);

        let (stage, reason) = attempt_v1_execution_identity(&expected, src.path(), &rootfs_bytes)
            .expect_err("a real source.digest mismatch is caught drift and must be surfaced");
        assert_eq!(stage, "execution_identity");
        assert!(
            reason.contains("Capsule v1 execution identity check failed"),
            "{reason}"
        );
    }

    #[test]
    fn attempt_v1_execution_identity_errors_when_dependencies_are_declared_but_mismatch() {
        // A NON-empty expected dependency list is left unmeasured by this
        // function (no per-dependency producer exists) UNLESS something else
        // fails first. Here the source digest itself is correct, so finalize
        // reaches (and refuses on) an earlier unmeasured facet regardless —
        // this test exists to document/pin that a non-empty `dependencies`
        // never gets a fabricated `vec![]` measurement.
        let src = tempfile::tempdir().expect("tempdir");
        std::fs::write(src.path().join("main.js"), b"console.log(1);\n").expect("write source");
        let source_hash =
            materialized_source_tree_hash(src.path()).expect("hash checked-out source tree");
        let source_digest = ContentDigest::try_from(source_hash).expect("parse source digest");
        let rootfs_bytes = b"bytes".to_vec();
        let readonly_layer = content_digest_of(&rootfs_bytes, DigestAlgorithm::Blake3);
        let mut expected = v1_minimal_contract(source_digest, readonly_layer);
        expected.dependencies = vec![capsule::execution_contract::ResolvedDependencyContract {
            name: "npm".to_string(),
            derivation_digest: ContentDigest::new(DigestAlgorithm::Blake3, [3u8; 32]),
            output_digest: ContentDigest::new(DigestAlgorithm::Blake3, [4u8; 32]),
        }];

        let result = attempt_v1_execution_identity(&expected, src.path(), &rootfs_bytes)
            .expect("still a legitimate UnmeasuredFacet refusal, never Err");
        assert!(result.is_none());
    }

    #[test]
    fn content_digest_of_matches_independent_hashing_for_both_algorithms() {
        let bytes = b"hello capsule v1";

        let blake3_digest = content_digest_of(bytes, DigestAlgorithm::Blake3);
        assert_eq!(blake3_digest.algorithm(), DigestAlgorithm::Blake3);
        assert_eq!(blake3_digest.bytes(), *blake3::hash(bytes).as_bytes());

        let sha256_digest = content_digest_of(bytes, DigestAlgorithm::Sha256);
        assert_eq!(sha256_digest.algorithm(), DigestAlgorithm::Sha256);
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(bytes);
        let expected: [u8; 32] = hasher.finalize().into();
        assert_eq!(sha256_digest.bytes(), expected);
    }

    #[test]
    fn claimed_job_without_execution_contract_field_still_parses() {
        // Forward-compat: an ato-api that does not send `execution_contract`
        // yet (every ato-api today) must still parse cleanly.
        let raw = serde_json::json!({
            "id": "job_1",
            "capsule_id": "cap_1",
            "target_label": "app",
            "profile": "default",
        });
        let job: ClaimedJob = serde_json::from_value(raw).expect("parse without the new field");
        assert!(job.execution_contract.is_none());
    }
}
