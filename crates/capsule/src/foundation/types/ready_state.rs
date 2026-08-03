//! Ready-State Capsule authoring tables (parse-only, schema_version = "0.3").
//!
//! These tables describe how a capsule is *sealed into* and *restored from* a
//! warm/booted runtime snapshot (the "Ready-State Capsule" line) plus the
//! post-restore injection surfaces (secrets, bindings, external capabilities,
//! user context store) that must never be baked into a snapshot.
//!
//! They are **additive and parse-only** in this milestone:
//!
//! * Every field is `Option<…>` or a `#[serde(default)]` collection, so older
//!   binaries that predate these structs ignore the tables (no
//!   `deny_unknown_fields` anywhere in the capsule crate) and newer binaries
//!   read them. No `schema_version` bump is required — a capsule that omits all
//!   of these tables behaves exactly as before.
//! * Nothing here drives runtime behavior yet; the runtime backend
//!   (`crates/snapshot`) and the build/run pipeline wiring land in later
//!   milestones. The structs exist so recipes can start *declaring intent* and
//!   so the eligibility validator can compute Public Instant Run posture from
//!   the manifest alone.
//!
//! Name-collision note: the heavy-external-capability concept uses `[external.*]`
//! (not `[capabilities.*]`) because `capabilities` is already taken twice — by
//! [`CapsuleManifest::capabilities`](super::manifest::CapsuleManifest) (inference
//! model caps) and by `requirements.capabilities` (security posture). See the
//! Ready-State implementation plan §3.2 / capsule-redefinition-research §4.1.

use serde::{Deserialize, Serialize};

use super::manifest::CapsuleManifest;

/// `[snapshot]` — how this capsule is sealed and restored.
///
/// Bound at parse time only. `mode = "none"` (the default when the table is
/// omitted) means the legacy cold path; any other mode marks the capsule as
/// Ready-State-eligible once a snapshot backend + runner capability is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotConfig {
    /// Sealing strategy. `Warm`/`Booted` require a snapshot backend; `Cold`
    /// is the legacy fast-cold-boot path; `None` disables Ready-State.
    #[serde(default)]
    pub mode: SnapshotMode,

    /// How far to pre-advance the app before capturing the snapshot. The seal
    /// point is always reached with **no secrets and no user data** present.
    #[serde(default)]
    pub boot_until: BootUntil,

    /// Run the post-resume sanitizer (regenerate ids/entropy, reset
    /// sockets/clock, clear request-local state) before exposing a restored
    /// session. Defaults to `true` — sanitizing a clone is the safe default.
    #[serde(default = "default_true")]
    pub sanitize_after_restore: bool,

    /// Author-supplied restore-compatibility constraint (a `runner_class`
    /// label such as `"managed/linux-aarch64"`). The full `runner_class_id` is
    /// resolved from build-host facts; this only *constrains* it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_class: Option<String>,

    /// Restore-latency SLO in seconds. Placement may reject a runner that
    /// cannot meet it. `None` means unspecified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_restore_seconds: Option<u32>,

    /// User-facing paths the build must warm up before sealing the snapshot —
    /// each path receives an HTTP GET (after the healthcheck answers) so the
    /// sealed memory already contains the user's first-screen work (template
    /// generation, JIT, DB init, First Frame prep). Empty ⇒ unchanged v1
    /// behavior (healthcheck-only seal point). Each path must start with `/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warmup_paths: Vec<String>,

    /// Consecutive stable successes required across `warmup_paths` (plus the
    /// healthcheck) before the Pause+Snapshot. `1` reuses the v1 behavior: the
    /// first 2xx/3xx of every path is enough. Higher values reduce the chance
    /// the snapshot captures a half-started state (a server that answers /
    /// once then reloads routes, for instance) at the cost of build time.
    #[serde(
        default = "default_stable_successes",
        skip_serializing_if = "is_default_stable_successes"
    )]
    pub stable_successes: u32,

    /// Polling interval between stability checks, in milliseconds. Default 250.
    #[serde(
        default = "default_stable_interval_ms",
        skip_serializing_if = "is_default_stable_interval_ms"
    )]
    pub stable_interval_ms: u64,

    /// The path the runner hits to judge RESTORE readiness — i.e. the path the
    /// browser actually loads first. Defaults to `healthcheck`, then `/` when
    /// neither is set. Without this, runners report "ready" after only `/health`
    /// answers, while the user's first request to `/` still hits template/DB
    /// init that was NOT in the snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_ready_path: Option<String>,
}

/// `[seal_at]` — the Capsule-authored Snapshot acceptance program
/// (`CAPSULE_V1_EXECUTION_MODEL_SPEC.md` §6/§6.3).
///
/// `command` is an arbitrary verification program (an HTTP request, an API
/// workflow, browser automation, a database-init check, …). Ato interprets ONLY
/// its process result: exit 0 accepts the candidate Snapshot, any other exit
/// status or a timeout rejects it (§6.3). There is deliberately no Ato-specific
/// HTTP / gate / readiness-level / publish-at DSL here.
///
/// It is evaluated against a **disposable restore** of an immutable candidate,
/// never against the build guest whose state is sealed (§8.1), so what this
/// command does cannot enter the accepted Snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealAtConfig {
    /// Exact argv, executed with no implicit shell; argument boundaries are
    /// preserved exactly (§6.1). Shell behavior is available only through an
    /// explicitly selected shell in the argv, e.g. `["sh", "-lc", "…"]`.
    pub command: Vec<String>,

    /// Per-attempt verification timeout. MUST be positive and bounded by
    /// platform policy (§6.1) — see [`MAX_SEAL_AT_TIMEOUT_SECONDS`]. `None`
    /// leaves the bound to the acceptance-loop default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

/// Platform-policy ceiling for `seal_at.timeout_seconds` (§6.1: "bounded by
/// platform policy").
///
/// Kept in lockstep with the ceiling this repo already expresses for a per-job
/// builder timeout — `snapshot::firecracker::MAX_JOB_BOOT_TIMEOUT_S` and
/// `snapshot-builder`'s `MAX_BOOT_TIMEOUT_S`, both 600 s with the same
/// rationale: a single build job must not be able to pin the builder
/// indefinitely. `seal_at` verification runs inside that same build job, so it
/// inherits that job's ceiling rather than introducing a second policy number.
pub const MAX_SEAL_AT_TIMEOUT_SECONDS: u32 = 600;

/// Validate an authored `[seal_at]` table per §6.1/§6.3, naming the offender.
///
/// Shared by manifest validation and any producer that reads the table
/// directly, so both reject the same inputs. The argv rules mirror
/// `snapshot::acceptance`'s `AcceptanceConfig` contract exactly (argv[0] is the
/// program and must be non-empty; later arguments MAY be empty strings — a real
/// argv element; no argument may contain a NUL, which no exec boundary carries).
pub fn validate_seal_at(seal_at: &SealAtConfig) -> Result<(), String> {
    if seal_at.command.is_empty() {
        return Err("seal_at.command must be a non-empty argv array".to_string());
    }
    if seal_at.command[0].is_empty() {
        return Err("seal_at.command[0] (the program) must not be empty".to_string());
    }
    if let Some(index) = seal_at
        .command
        .iter()
        .position(|argument| argument.contains('\0'))
    {
        return Err(format!(
            "seal_at.command[{index}] must not contain a NUL byte"
        ));
    }
    match seal_at.timeout_seconds {
        None => Ok(()),
        Some(seconds) if (1..=MAX_SEAL_AT_TIMEOUT_SECONDS).contains(&seconds) => Ok(()),
        Some(seconds) => Err(format!(
            "seal_at.timeout_seconds must be an integer in 1..={MAX_SEAL_AT_TIMEOUT_SECONDS}, \
             got {seconds}"
        )),
    }
}

/// v1 warmup stability: the first 2xx/3xx of every path is enough.
pub const DEFAULT_STABLE_SUCCESSES: u32 = 1;
/// v1 warmup poll interval between stability rounds.
pub const DEFAULT_STABLE_INTERVAL_MS: u64 = 250;

fn default_stable_successes() -> u32 {
    DEFAULT_STABLE_SUCCESSES
}
fn default_stable_interval_ms() -> u64 {
    DEFAULT_STABLE_INTERVAL_MS
}
fn is_default_stable_successes(v: &u32) -> bool {
    *v == DEFAULT_STABLE_SUCCESSES
}
fn is_default_stable_interval_ms(v: &u64) -> bool {
    *v == DEFAULT_STABLE_INTERVAL_MS
}

/// Is `p` usable as the request-target of the HTTP/1.0 probe the build (warmup)
/// and the runner (content-ready) send into the guest?
///
/// The probe formats `GET {p} HTTP/1.0\r\nHost: …`, so `p` must be origin-form
/// (leading `/`) and free of any character that would break out of the request
/// line — a space would shift the version token, a CR/LF would split the request
/// into two. Rejecting here keeps an authoring typo from surfacing as an opaque
/// "never stabilized" build failure or a full boot-timeout restore hang.
pub fn is_valid_probe_path(p: &str) -> bool {
    p.starts_with('/') && !p.chars().any(|c| c == ' ' || c.is_control())
}

/// Validate every author-supplied probe path, naming the offender. Shared by the
/// builder lanes and the snapshot backend so both reject the same inputs.
pub fn validate_probe_paths(
    warmup_paths: &[String],
    content_ready_path: Option<&str>,
) -> Result<(), String> {
    for p in warmup_paths {
        if !is_valid_probe_path(p) {
            return Err(format!(
                "warmup_paths: {p:?} is not a valid probe path \
                 (must start with `/` and contain no spaces or control characters)"
            ));
        }
    }
    match content_ready_path {
        Some(p) if !is_valid_probe_path(p) => Err(format!(
            "content_ready_path: {p:?} is not a valid probe path \
             (must start with `/` and contain no spaces or control characters)"
        )),
        _ => Ok(()),
    }
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            mode: SnapshotMode::default(),
            boot_until: BootUntil::default(),
            sanitize_after_restore: true,
            runner_class: None,
            max_restore_seconds: None,
            warmup_paths: Vec::new(),
            stable_successes: default_stable_successes(),
            stable_interval_ms: default_stable_interval_ms(),
            content_ready_path: None,
        }
    }
}

impl SnapshotConfig {
    /// Whether this capsule opts into the Ready-State (warm/booted) line.
    /// `cold`/`none` stay on the legacy path.
    pub fn is_ready_state(&self) -> bool {
        matches!(self.mode, SnapshotMode::Warm | SnapshotMode::Booted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotMode {
    /// No snapshot; legacy install-then-cold-boot (default).
    #[default]
    None,
    /// Fast cold-boot from frozen file state (no warm memory).
    Cold,
    /// Warm memory snapshot captured after boot.
    Warm,
    /// Fully booted-to-readiness snapshot.
    Booted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BootUntil {
    /// Boot until the healthcheck endpoint answers (default; must be reachable
    /// pre-auth / secret-free).
    #[default]
    Healthcheck,
    /// Boot until the app reports fully ready.
    Ready,
    /// Boot just far enough to seal; defer secret-dependent init to the first
    /// post-restore request.
    FirstRequest,
}

/// `[secrets.<name>]` — a required secret as a **ref**, never a value.
///
/// Resolved post-restore via the existing secret-injection path. `delivery =
/// "proxy"` routes through the capability broker so the raw value never enters
/// the app process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretSpec {
    /// Whether the app cannot reach `boot_until` / serve without this secret.
    #[serde(default)]
    pub required: bool,

    /// Human-readable purpose, surfaced verbatim at grant/preflight time
    /// ("OpenAI API key used to summarize documents") — v1.2, never a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Target environment variable name inside the guest (when `delivery` is
    /// `env`/`fd`/`file`). `None` for proxy-only delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,

    /// How the value reaches the guest.
    #[serde(default)]
    pub delivery: SecretDelivery,

    /// Coarse classification (drives proxy-vs-raw policy; ADR-005).
    #[serde(default)]
    pub class: SecretClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SecretDelivery {
    /// Passed via an inherited file descriptor.
    Fd,
    /// Injected as an environment variable (default).
    #[default]
    Env,
    /// Written to a file the guest reads.
    File,
    /// Mediated by the capability proxy; the raw value never reaches the app.
    Proxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SecretClass {
    /// A provider API key (default).
    #[default]
    ApiKey,
    /// An OAuth token / grant.
    Oauth,
    /// A value generated per session/install.
    Generated,
    /// A short-lived session credential.
    Session,
}

/// `[generated_bindings.<name>]` — a RUN-time generated INTERNAL secret (Phase 7).
///
/// The value (a DB password, redis password, internal session/JWT key, …) is
/// generated per RUN inside the guest from the OS RNG and injected into every
/// `targets` service — it is NEVER stored in the artifact, receipt, logs, or
/// identity. Only this SPEC is (name/generator/bytes/scope/targets), so two runs
/// of the same artifact share identity but get different runtime values. Distinct
/// from an EXTERNAL `[secrets.*]` api key, which stays on the binding-lease path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedBindingSpec {
    /// How the value is generated at run (default `random_base64`).
    #[serde(default)]
    pub generator: GeneratedGenerator,
    /// Bytes of OS randomness drawn before encoding (default 32).
    #[serde(default = "default_generated_bytes")]
    pub bytes: u32,
    /// Lifetime scope (`run` = a fresh value per run; the default).
    #[serde(default)]
    pub scope: GeneratedBindingScope,
    /// Services whose env receives this value. Each must be a declared service.
    #[serde(default)]
    pub targets: Vec<String>,
}

fn default_generated_bytes() -> u32 {
    32
}

/// Generator method for a `[generated_bindings.*]` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedGenerator {
    /// Draw `bytes` bytes from the OS RNG and standard-base64-encode them.
    #[default]
    RandomBase64,
}

/// Lifetime scope of a generated internal binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedBindingScope {
    /// A fresh value generated per run.
    #[default]
    Run,
}

/// `[bindings.<name>]` — a post-restore, user/billing/env-specific injection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingSpec {
    /// What kind of resource this binding attaches.
    pub kind: BindingKind,

    /// Whether the session cannot run without it.
    #[serde(default)]
    pub required: bool,

    /// Whose scope the binding is resolved in.
    #[serde(default)]
    pub scope: BindingScope,

    /// Optional guest mount path for mountable kinds (`state`, `user_files`,
    /// `context`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,

    /// Access mode for mountable kinds (v1.2). `read_only` is enforced at the
    /// block layer for `user_files` input mounts; `None` means the kind's
    /// default (`state` ⇒ read-write; `user_files` must declare explicitly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<BindingMode>,

    /// Optional provider hint (e.g. `dedicated`, `cloud`, `local` for a
    /// `runner` binding).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// Access mode of a mountable binding (v1.2 — `[bindings.*].mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingMode {
    /// The guest cannot write the mount (block-layer enforced for drives).
    ReadOnly,
    /// The guest may write the mount.
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    Secret,
    Oauth,
    State,
    UserFiles,
    Llm,
    Runner,
    Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BindingScope {
    /// Bound to the invoking user (default; survives capsule swaps).
    #[default]
    User,
    /// Bound to this capsule install.
    Capsule,
    /// Bound to a single session.
    Session,
}

/// `[external.<name>]` — a heavy external capability (LLM, vector-db, browser).
///
/// Public Instant Run allows at most [`MAX_EXTERNAL_CAPABILITIES`]. The
/// `degraded` policy keeps the session usable (as a demo) when provisioning
/// fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalCapabilitySpec {
    /// Capability category (`llm`, `service`, `browser_worker`, …). Authored as
    /// the TOML key `type`.
    #[serde(rename = "type")]
    pub kind: String,

    /// Whether the session cannot run without it.
    #[serde(default)]
    pub required: bool,

    /// Acceptable providers, in preference order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,

    /// Single provider shorthand (when `providers` is not used).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Whether independent capabilities may be provisioned concurrently.
    #[serde(default)]
    pub provision: ProvisionMode,

    /// Locality preference for the provider.
    #[serde(default)]
    pub locality: Locality,

    /// What to do when provisioning fails.
    #[serde(default)]
    pub degraded: DegradedMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionMode {
    /// Provision concurrently with other capabilities (default).
    #[default]
    Parallel,
    /// Provision one at a time.
    Sequential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    /// Prefer a local provider, fall back to remote (default).
    #[default]
    LocalPreferred,
    /// Any provider is acceptable.
    Any,
    /// Require a cloud provider.
    Cloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DegradedMode {
    /// Fall back to a demo / sample-data mode (default).
    #[default]
    Demo,
    /// Disable the capability but keep the session running.
    Disable,
    /// Fail the run.
    Fail,
}

/// `[context]` — User Context Store binding (state that survives capsule swaps).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Sharing scope of the bound store.
    #[serde(default)]
    pub store: ContextStore,

    /// Persist outputs/files to the user's artifact store.
    #[serde(default)]
    pub artifacts: bool,

    /// Maintain a user-side index/history.
    #[serde(default)]
    pub index: bool,

    /// Guest mount path for the context store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,

    /// Record grant/provenance metadata for each access.
    #[serde(default)]
    pub provenance: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextStore {
    /// Private to this capsule (default).
    #[default]
    AppPrivate,
    /// Bound to the user, single capsule at a time.
    User,
    /// Bound to the user, shared across capsules.
    UserShared,
}

fn default_true() -> bool {
    true
}

/// Maximum number of `[external.*]` capabilities permitted for a Public Instant
/// Run. Authored capsules may declare more, but they fail the eligibility gate.
pub const MAX_EXTERNAL_CAPABILITIES: usize = 3;

/// How a capsule's GPU requirement (if any) relates to a Ready-State snapshot.
///
/// The invariant is "GPU **execution** is supported; GPU **snapshot** is not":
/// GPU/accelerator device state is never sealed into a Ready-State memory image.
/// A GPU declared as a post-restore `[external.*]` capability ([`External`]) is
/// fine; an in-VM GPU with no external binding ([`Passthrough`]) makes the
/// capsule Ready-State-ineligible (it must use a GPU runner class / external
/// capability instead). This is the single shared `GpuMode` — the `snapshot`
/// crate's `BackendCapabilities.gpu_mode` reuses it rather than minting a copy.
///
/// [`External`]: GpuMode::External
/// [`Passthrough`]: GpuMode::Passthrough
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuMode {
    /// No GPU requirement (default).
    #[default]
    None,
    /// GPU provided post-restore as an external capability — snapshot-safe.
    External,
    /// In-VM GPU with no external binding — NOT snapshottable, Ready-State-ineligible.
    Passthrough,
}

/// `[external.<name>]` `type` strings that denote a GPU/accelerator capability
/// (matched ASCII-case-insensitively).
pub const GPU_EXTERNAL_KINDS: &[&str] = &["gpu", "accelerator"];

/// Computed Public Instant Run eligibility, derived entirely from the manifest.
///
/// This is a *read-only projection*: it never mutates the manifest and is safe
/// to compute at parse time, at store-apply time, or in the run pipeline. It
/// records each predicate independently so callers can surface exactly which
/// gate failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstantRunEligibility {
    /// All predicates passed.
    pub eligible: bool,
    /// Network posture is default-deny (no unrestricted egress).
    pub network_default_deny: bool,
    /// No persistent state is declared.
    pub ephemeral_only: bool,
    /// No required secret blocks a no-setup run.
    pub no_secrets_required: bool,
    /// Number of declared external capabilities.
    pub external_count: usize,
    /// `external_count <= MAX_EXTERNAL_CAPABILITIES`.
    pub external_within_limit: bool,
    /// A readiness/healthcheck signal exists (so an instant run can be detected
    /// as ready).
    pub has_healthcheck: bool,
    /// The capsule declares a GPU requirement (in-VM or external).
    #[serde(default)]
    pub requires_gpu: bool,
    /// The GPU requirement is satisfied by a post-restore `[external.*]` binding.
    #[serde(default)]
    pub gpu_external_binding: bool,
    /// An in-VM GPU with no external binding blocks Ready-State (not snapshottable).
    #[serde(default)]
    pub gpu_blocks_ready_state: bool,
    /// Human-readable reasons the capsule is ineligible (empty when eligible).
    pub blocking_reasons: Vec<String>,
}

impl CapsuleManifest {
    /// The `[snapshot]` table, or the default (`mode = none`) when omitted.
    pub fn snapshot_config(&self) -> SnapshotConfig {
        self.snapshot.clone().unwrap_or_default()
    }

    /// Whether this capsule opts into the Ready-State (warm/booted) line.
    pub fn is_ready_state_eligible(&self) -> bool {
        self.snapshot
            .as_ref()
            .map(SnapshotConfig::is_ready_state)
            .unwrap_or(false)
    }

    /// Compute Public Instant Run eligibility from the manifest alone.
    ///
    /// Predicates (all must hold):
    /// * **network default-deny** — declared `requirements.capabilities.network`
    ///   is `none`, or it is `egress`/`ingress`/`bidirectional` but constrained
    ///   by a non-empty egress allowlist. An undeclared network posture is
    ///   treated as default-deny (nothing was requested).
    /// * **ephemeral only** — no `[state.*]` entry is `persistent`.
    /// * **no required secrets** — neither `capabilities.secrets_required` nor
    ///   any `[secrets.*]` entry marked `required`.
    /// * **external count ≤ 3** — at most [`MAX_EXTERNAL_CAPABILITIES`]
    ///   `[external.*]` entries.
    /// * **healthcheck present** — a readiness probe exists on the *serving*
    ///   target (`default_target`, or the sole target when none is named) or a
    ///   service in that target's dependency graph. A probe on an unrelated
    ///   target does not count.
    pub fn instant_run_eligibility(&self) -> InstantRunEligibility {
        let mut blocking_reasons = Vec::new();

        // ── network default-deny ──────────────────────────────────────────
        let declared_network = self
            .requirements
            .capabilities
            .as_ref()
            .and_then(|c| c.network);
        let has_egress_allowlist = self
            .network
            .as_ref()
            .map(|n| !n.egress_allow.is_empty() || !n.egress_id_allow.is_empty())
            .unwrap_or(false);
        let network_default_deny = match declared_network {
            None => true,
            Some(crate::schema::capabilities::Network::None) => true,
            Some(_) => has_egress_allowlist,
        };
        if !network_default_deny {
            blocking_reasons.push(
                "network posture is not default-deny: an unrestricted-egress capability is \
                 declared without an egress allowlist"
                    .to_string(),
            );
        }

        // ── ephemeral only ────────────────────────────────────────────────
        let persistent_states: Vec<&str> = self
            .state
            .iter()
            .filter(|(_, req)| {
                matches!(req.durability, super::manifest::StateDurability::Persistent)
            })
            .map(|(name, _)| name.as_str())
            .collect();
        let ephemeral_only = persistent_states.is_empty();
        if !ephemeral_only {
            blocking_reasons.push(format!(
                "persistent state declared: [state.{}]",
                persistent_states.join("], [state.")
            ));
        }

        // ── no required secrets ───────────────────────────────────────────
        let caps_secrets_required = self
            .requirements
            .capabilities
            .as_ref()
            .and_then(|c| c.secrets_required)
            .unwrap_or(false);
        let required_secret_names: Vec<&str> = self
            .secrets
            .iter()
            .filter(|(_, spec)| spec.required)
            .map(|(name, _)| name.as_str())
            .collect();
        let no_secrets_required = !caps_secrets_required && required_secret_names.is_empty();
        if caps_secrets_required {
            blocking_reasons.push("requirements.capabilities.secrets_required is true".to_string());
        }
        if !required_secret_names.is_empty() {
            blocking_reasons.push(format!(
                "required secret(s): [secrets.{}]",
                required_secret_names.join("], [secrets.")
            ));
        }

        // ── external capability count ─────────────────────────────────────
        let external_count = self.external.len();
        let external_within_limit = external_count <= MAX_EXTERNAL_CAPABILITIES;
        if !external_within_limit {
            blocking_reasons.push(format!(
                "{external_count} external capabilities declared, limit is \
                 {MAX_EXTERNAL_CAPABILITIES}"
            ));
        }

        // ── healthcheck present (scoped to the SERVING target) ────────────
        // A readiness probe only makes a run ready-detectable if it sits on the
        // target that actually serves — the `default_target` (or, when none is
        // named, the sole target). A probe on some *other* target does not let
        // `ato run` decide the served capsule is ready, so it must NOT count.
        let named = self.targets.as_ref().map(|t| t.named_targets());
        let serving_label: Option<&str> = {
            let dt = self.default_target.trim();
            if !dt.is_empty() {
                Some(dt)
            } else {
                // No default named: unambiguous only when exactly one target.
                named.and_then(|nt| {
                    if nt.len() == 1 {
                        nt.keys().next().map(String::as_str)
                    } else {
                        None
                    }
                })
            }
        };
        let serving_target = serving_label.and_then(|label| named.and_then(|nt| nt.get(label)));

        let target_has_probe = serving_target
            .map(|nt| nt.readiness_probe.is_some())
            .unwrap_or(false);

        // Services count only when they belong to the serving target's graph:
        // a service whose `target` is the serving label (or omitted — the router
        // falls back to `default_target`), plus that service's `depends_on`
        // closure. A probe on a service bound to a different target is ignored.
        let service_has_probe = match (serving_label, self.services.as_ref()) {
            (Some(label), Some(services)) => {
                let mut stack: Vec<&str> = services
                    .iter()
                    .filter(|(_, svc)| match &svc.target {
                        Some(t) => t == label,
                        None => true, // router binds target-less services to default_target
                    })
                    .map(|(name, _)| name.as_str())
                    .collect();
                let mut seen = std::collections::HashSet::new();
                let mut found = false;
                while let Some(name) = stack.pop() {
                    if !seen.insert(name) {
                        continue;
                    }
                    if let Some(svc) = services.get(name) {
                        if svc.readiness_probe.is_some() {
                            found = true;
                            break;
                        }
                        if let Some(deps) = &svc.depends_on {
                            stack.extend(deps.iter().map(String::as_str));
                        }
                    }
                }
                found
            }
            _ => false,
        };

        let has_healthcheck = target_has_probe || service_has_probe;
        if !has_healthcheck {
            let where_ = serving_label.unwrap_or("<unresolved default target>");
            blocking_reasons.push(format!(
                "no readiness probe on the serving target '{where_}' or its service graph"
            ));
        }

        // ── GPU posture (snapshot-safety) ─────────────────────────────────
        // "GPU execution yes, GPU snapshot no": an in-VM GPU with no external
        // binding cannot be sealed into a Ready-State snapshot, so it blocks
        // Ready-State (use a GPU runner class / external capability instead).
        let gpu_mode = self.gpu_mode();
        let requires_gpu = gpu_mode != GpuMode::None;
        let gpu_external_binding = gpu_mode == GpuMode::External;
        let gpu_blocks_ready_state = gpu_mode == GpuMode::Passthrough;
        if gpu_blocks_ready_state {
            blocking_reasons.push(
                "GPU is required in-VM but GPU state is not snapshottable; declare it as an \
                 [external.*] GPU capability or use a GPU runner class"
                    .to_string(),
            );
        }

        let eligible = blocking_reasons.is_empty();
        InstantRunEligibility {
            eligible,
            network_default_deny,
            ephemeral_only,
            no_secrets_required,
            external_count,
            external_within_limit,
            has_healthcheck,
            requires_gpu,
            gpu_external_binding,
            gpu_blocks_ready_state,
            blocking_reasons,
        }
    }

    /// Whether the capsule asks for an in-VM GPU (vram requirement or a build
    /// GPU flag). Broader than just `[requirements]` by deliberate fail-closed
    /// choice: `[build].gpu` also counts, so GPU state can never be sealed by
    /// accident.
    pub fn requires_in_vm_gpu(&self) -> bool {
        self.requirements.vram_min.is_some()
            || self.requirements.vram_recommended.is_some()
            || self.build.as_ref().map(|b| b.gpu).unwrap_or(false)
    }

    /// Whether a GPU/accelerator is declared as a post-restore `[external.*]`
    /// capability (snapshot-safe — provisioned after restore, never sealed).
    pub fn has_external_gpu_capability(&self) -> bool {
        self.external.values().any(|spec| {
            GPU_EXTERNAL_KINDS
                .iter()
                .any(|kind| spec.kind.trim().eq_ignore_ascii_case(kind))
        })
    }

    /// Classify the capsule's GPU posture. An external binding wins (it is the
    /// snapshot-safe path); an in-VM GPU with no external binding is
    /// `Passthrough` (Ready-State-ineligible); otherwise `None`.
    pub fn gpu_mode(&self) -> GpuMode {
        let in_vm = self.requires_in_vm_gpu();
        let external = self.has_external_gpu_capability();
        match (in_vm, external) {
            (_, true) => GpuMode::External,
            (true, false) => GpuMode::Passthrough,
            (false, false) => GpuMode::None,
        }
    }
}

#[cfg(test)]
mod tests;
