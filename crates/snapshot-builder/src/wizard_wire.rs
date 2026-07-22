//! Submission Wizard **PR-0 wire contract** — types + validation only, 機能未配線.
//!
//! Serde mirrors of the interactive-capture submission-wizard wire messages
//! defined in the PR-0 contract spec (`wizard-pr0-wire-contract.md`), plus the
//! `[cache.*]` / `[state.*]` capsule.toml declaration schema (§7). The ato-api
//! side carries the same shapes as zod schemas in
//! `src/services/submission_wizard/wire.ts`; both sides test against the exact
//! snake_case wire names in the spec's §9 seam checklist.
//!
//! PR-0 explicitly wires NOTHING:
//! - `"interactive_capture"` is NOT added to the claim loop's `supported_kinds`
//!   and no enqueue accepts it; `"holding"` cannot be persisted (DB CHECK
//!   unchanged server-side).
//! - No polling loop, no hold/quiesce/capture execution, no new ack path is
//!   taken at runtime; nothing in this module is called outside its tests.
//! - The TOML declaration schema is NOT consulted by any build path.
//!
//! The module-level `dead_code` allow below exists because of exactly that:
//! PR-1 wires the claim/control loop and removes it.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Constants (spec §0 / §2)
// ─────────────────────────────────────────────────────────────────────────────

/// NEW job kind for wizard jobs. Defined but NOT wired in PR-0: never added to
/// the daemon's advertised `supported_kinds` (an unknown kind keeps failing the
/// job closed at stage `claim_kind`, never guessed), and no enqueue accepts it.
pub const JOB_KIND_INTERACTIVE_CAPTURE: &str = "interactive_capture";

/// NEW job status for a wizard job whose app is up and held for the submitter.
/// Defined but NOT wired in PR-0: the server-side status DB CHECK is unchanged,
/// so this value cannot yet be persisted (migration is PR-1+).
pub const JOB_STATUS_HOLDING: &str = "holding";

/// Header carrying the `lease_token` on the one GET (control poll, §3.3) —
/// never a query param, keeping the secret out of URLs/access logs.
pub const LEASE_TOKEN_HEADER: &str = "x-ato-lease-token";

/// `error` code of the `409 { "error": "fenced", "message": ... }` envelope
/// rejecting any FENCING-5 violation (spec §1). A fenced request has NO side
/// effects server-side.
pub const ERROR_CODE_FENCED: &str = "fenced";

/// ID prefixes (spec §0): `<prefix><ULID>`. The prefix is a debugging/log
/// affordance ONLY — receivers must treat all of these as opaque strings and
/// never parse a prefix for meaning.
pub const SUBMISSION_ATTEMPT_ID_PREFIX: &str = "subatt_";
/// See [`SUBMISSION_ATTEMPT_ID_PREFIX`].
pub const WORKER_CLAIM_ID_PREFIX: &str = "claim_";
/// See [`SUBMISSION_ATTEMPT_ID_PREFIX`].
pub const CANDIDATE_ID_PREFIX: &str = "cand_";
/// See [`SUBMISSION_ATTEMPT_ID_PREFIX`].
pub const VERIFY_SESSION_ID_PREFIX: &str = "vsess_";

// ─────────────────────────────────────────────────────────────────────────────
// Enums (spec §2 — exact wire strings)
// ─────────────────────────────────────────────────────────────────────────────

/// Coarse progress stage (§2, §3.4). Exactly these 9 values are legal on the
/// progress message — the two failure discriminators (`capture_seal`,
/// `acceptance`) live only on [`WizardFailureStage`], enforced by the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardStage {
    Fetch,
    Runtime,
    Deps,
    Build,
    Launch,
    Holding,
    Quiescing,
    Capturing,
    Accepting,
}

/// `failure_stage` on the wizard ack (§2, §3.7): any coarse stage value plus
/// the two failure-only discriminators — `capture_seal` (failure while sealing
/// the captured filesystem/snapshot) vs `acceptance` (server-side rejection of
/// an otherwise-sealed candidate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardFailureStage {
    Fetch,
    Runtime,
    Deps,
    Build,
    Launch,
    Holding,
    Quiescing,
    Capturing,
    Accepting,
    CaptureSeal,
    Acceptance,
}

impl From<WizardStage> for WizardFailureStage {
    /// Every coarse stage is also a legal failure stage (§2).
    fn from(stage: WizardStage) -> Self {
        match stage {
            WizardStage::Fetch => WizardFailureStage::Fetch,
            WizardStage::Runtime => WizardFailureStage::Runtime,
            WizardStage::Deps => WizardFailureStage::Deps,
            WizardStage::Build => WizardFailureStage::Build,
            WizardStage::Launch => WizardFailureStage::Launch,
            WizardStage::Holding => WizardFailureStage::Holding,
            WizardStage::Quiescing => WizardFailureStage::Quiescing,
            WizardStage::Capturing => WizardFailureStage::Capturing,
            WizardStage::Accepting => WizardFailureStage::Accepting,
        }
    }
}

/// Control directive delivered on the control poll (§3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlDirective {
    /// Keep the session alive and keep polling.
    Hold,
    /// Perform capture for the response's `capture_epoch`.
    Capture,
    /// Tear down without capturing; the attempt is over for this claim.
    Discard,
}

/// Candidate lifecycle status (§2, §3.6 response).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Reported,
    Verifying,
    Accepted,
    Rejected,
    Expired,
}

/// Verify-session lifecycle status (§2, §4): `pending → active → ended`, or
/// `failed`/`expired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifySessionStatus {
    Pending,
    Active,
    Ended,
    Failed,
    Expired,
}

/// Ack `status` (§3.7) — the existing enum, unchanged. `sealed` for a wizard
/// job means "a candidate was accepted".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardAckStatus {
    Sealed,
    Failed,
}

// ─────────────────────────────────────────────────────────────────────────────
// FENCING-5 (spec §1)
// ─────────────────────────────────────────────────────────────────────────────

/// **FENCING-5**: the 5-tuple every builder-originated request after claim —
/// control poll, lease renew, progress, hold-ready, candidate report, ack —
/// MUST carry. The server compares all five against its authoritative row; any
/// mismatch, or an expired lease, rejects with `409 { "error": "fenced" }` and
/// the request has NO side effects.
///
/// Transport: on POST bodies the five fields appear top-level (this struct is
/// `#[serde(flatten)]`-embedded); on the control GET, `job_id` is in the path,
/// `submission_attempt_id`/`worker_claim_id`/`capture_epoch` are query params
/// ([`ControlQuery`]), and `lease_token` rides the [`LEASE_TOKEN_HEADER`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FencingTuple {
    /// `job_` — issued at enqueue (existing convention).
    pub job_id: String,
    /// `subatt_` — issued at enqueue of the wizard job; 1:1 with the enqueue,
    /// stable for the attempt's lifetime (retries WITHIN the claim included).
    /// An interactive attempt is never re-claimed (ADR-008 v3.1): lease expiry
    /// fails the attempt, and a subsequent claim serves a NEW attempt with a
    /// new `subatt_`.
    pub submission_attempt_id: String,
    /// `claim_` — a generation that fences duplicate/stale workers WITHIN an
    /// attempt. It is NOT a workspace-migration id, and an attempt is never
    /// re-claimed: lease expiry on an interactive attempt fails the attempt
    /// (ADR-008 v3.1), and the next claim carries a new `subatt_` + new
    /// `claim_`.
    pub worker_claim_id: String,
    /// Opaque secret string, no format promise (server mints ≥ 32 bytes of
    /// entropy, base64url), minted per claim generation alongside
    /// `worker_claim_id`. **Server storage is hash-only** — the server persists
    /// a hash, never the token (PR-1 concern; PR-0 documents it here). Builders
    /// must never log it and never put it in URLs.
    pub lease_token: String,
    /// Monotonically increasing capture **command counter** — an integer ≥ 0,
    /// NOT an id and NOT a boolean. `0` ⇔ no capture has ever been requested on
    /// this claim's job. In the tuple it is the highest epoch the builder has
    /// observed; reporting a stale epoch after the server advanced it is fenced
    /// — this is what makes a superseded claim/capture unable to write
    /// anything.
    pub capture_epoch: u64,
}

/// The standard error envelope, e.g. the §1 fencing rejection
/// `409 { "error": "fenced", "message": "..." }` (see [`ERROR_CODE_FENCED`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: String,
    pub message: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Builder-lane messages (spec §3)
// ─────────────────────────────────────────────────────────────────────────────

/// §3.1 — the fields the per-job object in the claim response gains when (and
/// only when) the job kind is [`JOB_KIND_INTERACTIVE_CAPTURE`]. PR-1 merges
/// these onto the live `ClaimedJob` as `#[serde(default)]` optionals so
/// builders that never advertise the kind are untouched; PR-0 keeps the live
/// claim parser byte-identical and carries the extension here only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveCaptureClaimExt {
    /// Fixed at enqueue; echoed in FENCING-5.
    pub submission_attempt_id: String,
    /// Fresh per claim generation; echoed in FENCING-5.
    pub worker_claim_id: String,
    /// Opaque secret; echoed in FENCING-5 (see [`FencingTuple::lease_token`]).
    pub lease_token: String,
    /// ISO-8601 UTC lease deadline; the builder must renew before this.
    pub lease_expires_at: String,
}

/// §3.2 request — `POST /v1/capsule-snapshots/jobs/:job_id/lease/renew`.
/// The body is exactly the FENCING-5 fields, nothing else.
pub type LeaseRenewRequest = FencingTuple;

/// §3.2 response. The `lease_token` is **stable within a claim generation** —
/// renew extends expiry, it does not rotate the token; a new token comes only
/// with a new `worker_claim_id`. An expired lease cannot be renewed
/// (`409 fenced`), and an interactive attempt is never re-claimed (ADR-008
/// v3.1): lease expiry marks the attempt expired/failed, and a subsequent
/// claim starts a NEW attempt from build, minting a new `subatt_` + new
/// `claim_`/token pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRenewResponse {
    /// ISO-8601 UTC — the new lease deadline.
    pub lease_expires_at: String,
}

/// §3.3 / §1 — query params of the control poll GET
/// (`GET /v1/capsule-snapshots/jobs/:job_id/control?...`). `job_id` rides the
/// path and `lease_token` rides the [`LEASE_TOKEN_HEADER`] header, completing
/// FENCING-5 for the one GET.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlQuery {
    pub submission_attempt_id: String,
    pub worker_claim_id: String,
    /// Highest epoch the builder has observed (0 if none).
    pub capture_epoch: u64,
}

/// §3.3 — control poll response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub directive: ControlDirective,
    /// Current authoritative epoch. With `directive: "capture"` it is ≥ 1 and
    /// names the command; the builder adopts it as its observed epoch. `0` ⇔ no
    /// capture ever requested.
    pub capture_epoch: u64,
    /// Present only when `directive: "capture"`: the pre-minted candidate for
    /// this epoch (epoch ↔ candidate is 1:1); the builder echoes it back in the
    /// candidate report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<String>,
    /// ISO-8601 UTC; required when `directive: "hold"` — the server-side hold
    /// deadline. After it, expect `discard`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_expires_at: Option<String>,
    /// **Causality carrier for the quiesce contract (§5)**: the API sets this
    /// `true` for a given epoch only after it has received the proxy's
    /// `quiesced { epoch, inflight: 0 }` ack. The builder MUST NOT pause/freeze
    /// the guest for capture until it observes `pause_permitted: true` at the
    /// capture epoch.
    pub pause_permitted: bool,
}

impl ControlResponse {
    /// Structural refinements the schema alone cannot express (mirrored in the
    /// ato-api zod refinements; PR-0 encodes them here + in tests).
    pub fn validate(&self) -> Result<(), String> {
        match self.directive {
            ControlDirective::Capture => {
                if self.capture_epoch == 0 {
                    return Err("directive \"capture\" requires capture_epoch >= 1".into());
                }
                if self.candidate_id.is_none() {
                    return Err("directive \"capture\" requires candidate_id".into());
                }
            }
            ControlDirective::Hold => {
                if self.hold_expires_at.is_none() {
                    return Err("directive \"hold\" requires hold_expires_at".into());
                }
                if self.candidate_id.is_some() {
                    return Err("candidate_id is only delivered with directive \"capture\"".into());
                }
            }
            ControlDirective::Discard => {
                if self.candidate_id.is_some() {
                    return Err("candidate_id is only delivered with directive \"capture\"".into());
                }
            }
        }
        Ok(())
    }
}

/// §3.4 — `POST /v1/capsule-snapshots/jobs/:job_id/progress`. Response `200 {}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressRequest {
    #[serde(flatten)]
    pub fencing: FencingTuple,
    /// Coarse progress only — the type ([`WizardStage`], 9 values) excludes the
    /// failure discriminators by construction. Monotonic advance is NOT
    /// enforced on the wire (retries/restarts within a claim may repeat a
    /// stage).
    pub stage: WizardStage,
}

/// §3.5 — `POST /v1/capsule-snapshots/jobs/:job_id/hold-ready`, sent once when
/// the app is up and the builder enters `holding`. Response `200 {}`.
///
/// **Deliberately absent (ADR-004, SSRF)**: there is NO self-reported upstream
/// URL/host/address field, and the api rejects unknown fields here
/// (`.strict()` server-side). The api derives the proxy upstream itself from
/// `(builder_id, slot_id, session_id, guest_port)` against its own registry of
/// builder ingress addresses — a builder can never point the proxy at an
/// arbitrary URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldReadyRequest {
    #[serde(flatten)]
    pub fencing: FencingTuple,
    /// Stable builder identity, 1..120 chars (same charset/limits as
    /// `agent_id`).
    pub builder_id: String,
    /// Slot on that builder hosting the held session, 1..120 chars.
    pub slot_id: String,
    /// Builder-local session identity for the held app, 1..120 chars.
    pub session_id: String,
    /// Port inside the guest the app listens on, 1..65535.
    pub guest_port: u16,
}

impl HoldReadyRequest {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("builder_id", &self.builder_id),
            ("slot_id", &self.slot_id),
            ("session_id", &self.session_id),
        ] {
            if value.is_empty() || value.chars().count() > 120 {
                return Err(format!("{name} must be 1..120 chars"));
            }
        }
        if self.guest_port == 0 {
            return Err("guest_port must be 1..65535".into());
        }
        Ok(())
    }
}

/// §3.6 — `POST /v1/capsule-snapshots/jobs/:job_id/candidates`: reports a
/// **sealed** candidate after a `capture` directive completes. Seal-time
/// failures go through the ack (§3.7, `failure_stage: "capture_seal"`), never
/// through this message. The tuple's `capture_epoch` doubles as the epoch being
/// reported (≥ 1); a report for a superseded epoch is fenced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReportRequest {
    #[serde(flatten)]
    pub fencing: FencingTuple,
    /// Must equal the id the control channel delivered for this epoch (the
    /// server cross-checks the epoch ↔ candidate 1:1 mapping).
    pub candidate_id: String,
    /// **The canonical identity** of the captured execution (§6 naming rule:
    /// no field may imply a snapshot is the canonical launch key).
    pub execution_id: String,
    /// Sealed snapshot produced for this candidate.
    pub snapshot_id: String,
    /// Same semantics as the existing sealed-ack `artifact_location`.
    pub artifact_location: String,
    /// `true` ⇒ the live session died/was destroyed during or after capture;
    /// the candidate is still reportable but no further captures can come from
    /// this claim without a fresh launch.
    pub source_lost: bool,
}

impl CandidateReportRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.fencing.capture_epoch == 0 {
            return Err("candidate report requires capture_epoch >= 1".into());
        }
        Ok(())
    }
}

/// §3.6 — candidate report response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReportResponse {
    pub candidate_id: String,
    pub status: CandidateStatus,
}

/// §3.7 — the wizard extension of the existing
/// `POST /v1/capsule-snapshots/jobs/:job_id/ack`. For
/// [`JOB_KIND_INTERACTIVE_CAPTURE`] jobs the existing ack body (`agent_id`,
/// `status`, `failure_stage?`, `failure_reason?`, sealed-receipt shape)
/// additionally carries FENCING-5 and the acceptance result. Existing
/// non-wizard acks are untouched: on the shared server schema these fields are
/// optional, required-by-refinement for wizard jobs ([`Self::validate`];
/// route-enforced in PR-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WizardAck {
    /// Existing field, unchanged.
    pub agent_id: String,
    #[serde(flatten)]
    pub fencing: FencingTuple,
    pub status: WizardAckStatus,
    /// Required when `status: "sealed"` — which candidate the acceptance
    /// applies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_candidate_id: Option<String>,
    /// Required when `status: "sealed"`. The existing sealed-receipt shape
    /// (`capsule_manifest_hash`, `execution_id`, `artifact_manifest_hash`,
    /// `runner_class_id`, `snapshot_backend`, `artifact_location`,
    /// `hardware_contract_id`, `snapshot_format_id`, `snapshot_codec_id`,
    /// `manifest_source`, ...) — i.e. what the builder serializes from its
    /// `Artifact` struct. Kept opaque here in PR-0: the vocabulary is reused,
    /// not redefined, and `manifest_source` gains no new value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_receipt: Option<serde_json::Value>,
    /// Required when `status: "failed"` — discriminates seal-time
    /// (`capture_seal`) vs acceptance-time (`acceptance`) failure, or any
    /// coarse stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<WizardFailureStage>,
    /// Optional, ≤ 2000 chars server-side (the builder truncates at 1800, as
    /// the existing failed ack does).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

impl WizardAck {
    /// The §3.7 required-by-refinement rules for a wizard ack.
    pub fn validate(&self) -> Result<(), String> {
        match self.status {
            WizardAckStatus::Sealed => {
                if self.accepted_candidate_id.is_none() {
                    return Err("status \"sealed\" requires accepted_candidate_id".into());
                }
                if self.acceptance_receipt.is_none() {
                    return Err("status \"sealed\" requires acceptance_receipt".into());
                }
            }
            WizardAckStatus::Failed => {
                if self.failure_stage.is_none() {
                    return Err("status \"failed\" requires failure_stage".into());
                }
            }
        }
        if let Some(reason) = &self.failure_reason
            && reason.chars().count() > 2000
        {
            return Err("failure_reason must be <= 2000 chars".into());
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Verify sessions (spec §4 — types only)
// ─────────────────────────────────────────────────────────────────────────────

/// §4 — a verify session is an INDEPENDENT resource, 1:N per candidate (a
/// candidate may be verified zero or many times). Returned by future
/// wizard-facing routes, not builder routes. Deleting/expiring it never mutates
/// the candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifySession {
    /// `vsess_` — its own lifecycle.
    pub verify_session_id: String,
    /// Parent candidate (`cand_`).
    pub candidate_id: String,
    pub status: VerifySessionStatus,
    /// ISO-8601 UTC hard deadline.
    pub expires_at: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// api ⇄ proxy quiesce contract (spec §5 — internal, types only)
// ─────────────────────────────────────────────────────────────────────────────

/// §5 — the three api ⇄ proxy quiesce messages, discriminated by `type`. The
/// transport (service binding / DO) is PR-2 and lives entirely in ato-api /
/// the proxy; the shapes are mirrored here so the Rust side can seam-test the
/// exact wire encoding the causality in §3.3 `pause_permitted` depends on.
///
/// **Drain timeout is fail-closed**: if the proxy cannot reach `inflight: 0`
/// within the drain window, the api never sets `pause_permitted`, marks the
/// capture attempt failed, and sends `unquiesce`. The system NEVER
/// force-captures under live traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuiesceMessage {
    /// api → proxy: stop admitting new requests to the held session's upstream
    /// for this capture epoch; drain in-flight.
    Quiesce { epoch: u64 },
    /// proxy → api: drain complete. Only after receiving this ack for epoch N
    /// may the api set `pause_permitted: true` at epoch N (§3.3). `inflight` is
    /// fixed at literal `0` — an ack with traffic still in flight is not a
    /// valid message ([`Self::validate`]).
    Quiesced { epoch: u64, inflight: u64 },
    /// api → proxy: resume proxying (after capture completes or is aborted).
    Unquiesce { epoch: u64 },
}

impl QuiesceMessage {
    pub fn validate(&self) -> Result<(), String> {
        let epoch = match self {
            QuiesceMessage::Quiesce { epoch } | QuiesceMessage::Unquiesce { epoch } => *epoch,
            QuiesceMessage::Quiesced { epoch, inflight } => {
                if *inflight != 0 {
                    return Err("quiesced.inflight must be the literal 0".into());
                }
                *epoch
            }
        };
        if epoch == 0 {
            return Err("quiesce messages require epoch >= 1".into());
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Publish-semantics naming (spec §6 — referenced types only)
// ─────────────────────────────────────────────────────────────────────────────

/// §6 — publish-semantics attestation: the snapshot the publisher personally
/// verified. **`execution_id` is the canonical identity**; no wizard field may
/// be named to imply a snapshot is the canonical launch key (banned shapes:
/// `launch_snapshot_id`, `canonical_snapshot_id`, `snapshot_launch_key`). Any
/// new `*snapshot_id` field must be one of these two names or be renamed.
pub const FIELD_PUBLISHER_VERIFIED_SNAPSHOT_ID: &str = "publisher_verified_snapshot_id";
/// §6 — optional routing hint (see [`FIELD_PUBLISHER_VERIFIED_SNAPSHOT_ID`]).
pub const FIELD_PREFERRED_SNAPSHOT_ID: &str = "preferred_snapshot_id";

// ─────────────────────────────────────────────────────────────────────────────
// capsule.toml declaration schema (spec §7 — parse + validation only)
// ─────────────────────────────────────────────────────────────────────────────

/// §7 — `capture` on a `[cache.<name>]` declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    Include,
    Exclude,
}

/// §7 — `snapshot` on a `[state.<name>]` declaration: ONLY `"exclude"` is
/// legal for state (never baked; runtime durable state is a restore-time
/// binding per the v1.6 rule). `"include"` fails at parse by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateSnapshotMode {
    Exclude,
}

/// §7 — `[cache.<name>]`: a path baked into the captured snapshot (declared
/// cache surface). Unknown keys INSIDE a declaration are rejected
/// (`deny_unknown_fields`), mirroring the api's `.strict()` projection — only
/// unknown tables elsewhere in the manifest are ignored
/// ([`CaptureDeclarations`]); the same declaration must get one verdict on
/// both sides.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheDeclaration {
    /// Relative path (see [`validate_declared_path`]).
    pub path: String,
    pub capture: CaptureMode,
}

/// §7 — `[state.<name>]`: never baked; runtime durable state. Unknown keys
/// INSIDE the declaration are rejected, same as [`CacheDeclaration`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDeclaration {
    /// Relative path (see [`validate_declared_path`]).
    pub path: String,
    pub snapshot: StateSnapshotMode,
    /// Free-form schema id, 1..60 chars, `[a-z0-9_.-]` (e.g. `"sqlite"`,
    /// `"kv-dir"`).
    pub schema: String,
}

/// §7 — the `[cache.*]` / `[state.*]` tables of a capsule.toml. Other tables in
/// the manifest are ignored here; this parser is NOT consulted by any build
/// path in PR-0 (and `crates/capsule` is untouched — local wire-structs
/// precedent).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CaptureDeclarations {
    #[serde(default)]
    pub cache: BTreeMap<String, CacheDeclaration>,
    #[serde(default)]
    pub state: BTreeMap<String, StateDeclaration>,
}

/// §7 — everything the PR-0 parser produces: the declared-path set plus the
/// capture-refusal-domain complement marker. Any filesystem write at capture
/// time outside every declared path is grounds for the capture to be refused
/// (enforcement is a later PR; PR-0 only marks the domain).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredPaths {
    /// Every declared `[cache.*]`/`[state.*]` path (include AND exclude — the
    /// declaration itself is what bounds the refusal domain).
    pub paths: BTreeSet<String>,
    pub refusal_domain: RefusalDomain,
}

/// §7 — marker: the capture-refusal domain is the complement of the declared
/// paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalDomain {
    ComplementOfDeclared,
}

/// §7 declaration `<name>`: `[a-z0-9_-]{1,40}`, unique across cache ∪ state
/// (uniqueness is checked in [`CaptureDeclarations::validate`]).
fn validate_declaration_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > 40 {
        return Err(format!("declaration name {name:?} must be 1..40 chars"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(format!(
            "declaration name {name:?} must match [a-z0-9_-]{{1,40}}"
        ));
    }
    Ok(())
}

/// §7 `path`: relative, non-empty, no leading `/`, no `..` segments, no
/// backslashes — the same constraints as ato-api's `mountRelativePath` helper
/// (`snapshot_registry.ts`: min 1 / max 200 / relative / no `..` component),
/// plus the spec's explicit backslash rejection. The workspace's closest
/// existing helper (`snapshot::docker_import::validate_dockerfile_path`) gates
/// `..`/prefix components but not backslashes or the 200-char cap, so the
/// contract gets its own validator here rather than a near-miss reuse.
fn validate_declared_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("path must be non-empty".into());
    }
    if path.chars().count() > 200 {
        return Err("path must be <= 200 chars".into());
    }
    if path.contains('\\') {
        return Err("path must not contain backslashes".into());
    }
    if path.starts_with('/') {
        return Err("path must be relative (no leading '/')".into());
    }
    if path.split('/').any(|seg| seg == "..") {
        return Err("path must not contain a '..' component".into());
    }
    Ok(())
}

/// §7 `schema`: free-form id, 1..60 chars, `[a-z0-9_.-]`.
fn validate_state_schema(schema: &str) -> Result<(), String> {
    if schema.is_empty() || schema.chars().count() > 60 {
        return Err(format!("schema {schema:?} must be 1..60 chars"));
    }
    if !schema
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.' || c == '-')
    {
        return Err(format!("schema {schema:?} must match [a-z0-9_.-]{{1,60}}"));
    }
    Ok(())
}

/// True when `inner` is strictly nested inside `outer` (segment-boundary
/// aware: `data/app.db` is inside `data`, but `database` is not).
fn path_is_nested_inside(inner: &str, outer: &str) -> bool {
    let outer = outer.trim_end_matches('/');
    inner.len() > outer.len()
        && inner.starts_with(outer)
        && inner.as_bytes().get(outer.len()) == Some(&b'/')
}

impl CaptureDeclarations {
    /// The §7 validation rules, producing the PR-0 output ([`DeclaredPaths`]):
    /// name charset + cross-section uniqueness, per-path constraints, no two
    /// identical paths, and no cross-section nesting (a state path inside a
    /// cache path or vice versa).
    pub fn validate(&self) -> Result<DeclaredPaths, String> {
        let mut seen_names: BTreeSet<&str> = BTreeSet::new();
        // (section, name, path) over cache ∪ state, for the cross-checks.
        let mut declared: Vec<(&str, &str, &str)> = Vec::new();

        for (name, decl) in &self.cache {
            validate_declaration_name(name)?;
            validate_declared_path(&decl.path).map_err(|e| format!("[cache.{name}]: {e}"))?;
            seen_names.insert(name);
            declared.push(("cache", name, &decl.path));
        }
        for (name, decl) in &self.state {
            validate_declaration_name(name)?;
            if !seen_names.insert(name) {
                return Err(format!(
                    "declaration name {name:?} is used by both [cache.{name}] and [state.{name}] (names are unique across cache \u{222a} state)"
                ));
            }
            validate_declared_path(&decl.path).map_err(|e| format!("[state.{name}]: {e}"))?;
            validate_state_schema(&decl.schema).map_err(|e| format!("[state.{name}]: {e}"))?;
            declared.push(("state", name, &decl.path));
        }

        for (i, (section_a, name_a, path_a)) in declared.iter().enumerate() {
            for (section_b, name_b, path_b) in &declared[i + 1..] {
                if path_a == path_b {
                    return Err(format!(
                        "[{section_a}.{name_a}] and [{section_b}.{name_b}] declare the identical path {path_a:?}"
                    ));
                }
                if section_a != section_b
                    && (path_is_nested_inside(path_a, path_b)
                        || path_is_nested_inside(path_b, path_a))
                {
                    return Err(format!(
                        "[{section_a}.{name_a}] ({path_a:?}) and [{section_b}.{name_b}] ({path_b:?}) nest across sections"
                    ));
                }
            }
        }

        Ok(DeclaredPaths {
            paths: declared.iter().map(|(_, _, p)| (*p).to_string()).collect(),
            refusal_domain: RefusalDomain::ComplementOfDeclared,
        })
    }
}

/// Parse the `[cache.*]`/`[state.*]` tables out of a capsule.toml text and run
/// the §7 validation. Unknown tables/keys elsewhere in the manifest are
/// ignored, but unknown keys INSIDE a `[cache.*]`/`[state.*]` declaration are
/// rejected (mirrors the api's `.strict()`); invalid literal values
/// (`capture = "sometimes"`, `snapshot = "include"`, a missing `schema`) fail
/// at parse by construction.
pub fn parse_capture_declarations(toml_text: &str) -> Result<DeclaredPaths, String> {
    let decls: CaptureDeclarations =
        toml::from_str(toml_text).map_err(|e| format!("capsule.toml capture declarations: {e}"))?;
    decls.validate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fencing() -> FencingTuple {
        FencingTuple {
            job_id: "job_01J1XY".into(),
            submission_attempt_id: "subatt_01J1XY".into(),
            worker_claim_id: "claim_01J1XZ".into(),
            lease_token: "b64u-opaque-token".into(),
            capture_epoch: 0,
        }
    }

    fn sorted_keys(v: &Value) -> Vec<String> {
        let mut keys: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        keys.sort();
        keys
    }

    // ── §2 enum wire strings ────────────────────────────────────────────────

    #[test]
    fn stage_enum_serializes_to_the_nine_coarse_wire_strings() {
        let expected = [
            (WizardStage::Fetch, "fetch"),
            (WizardStage::Runtime, "runtime"),
            (WizardStage::Deps, "deps"),
            (WizardStage::Build, "build"),
            (WizardStage::Launch, "launch"),
            (WizardStage::Holding, "holding"),
            (WizardStage::Quiescing, "quiescing"),
            (WizardStage::Capturing, "capturing"),
            (WizardStage::Accepting, "accepting"),
        ];
        for (stage, wire) in expected {
            assert_eq!(serde_json::to_value(stage).unwrap(), json!(wire));
            // Every coarse stage is also a failure stage with the SAME string.
            assert_eq!(
                serde_json::to_value(WizardFailureStage::from(stage)).unwrap(),
                json!(wire)
            );
        }
    }

    #[test]
    fn failure_stage_adds_exactly_capture_seal_and_acceptance() {
        assert_eq!(
            serde_json::to_value(WizardFailureStage::CaptureSeal).unwrap(),
            json!("capture_seal")
        );
        assert_eq!(
            serde_json::to_value(WizardFailureStage::Acceptance).unwrap(),
            json!("acceptance")
        );
        // The coarse progress stage REJECTS the failure discriminators.
        assert!(serde_json::from_value::<WizardStage>(json!("capture_seal")).is_err());
        assert!(serde_json::from_value::<WizardStage>(json!("acceptance")).is_err());
    }

    #[test]
    fn directive_candidate_verify_and_ack_enums_use_exact_wire_strings() {
        for (v, wire) in [
            (ControlDirective::Hold, "hold"),
            (ControlDirective::Capture, "capture"),
            (ControlDirective::Discard, "discard"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(wire));
        }
        for (v, wire) in [
            (CandidateStatus::Reported, "reported"),
            (CandidateStatus::Verifying, "verifying"),
            (CandidateStatus::Accepted, "accepted"),
            (CandidateStatus::Rejected, "rejected"),
            (CandidateStatus::Expired, "expired"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(wire));
        }
        for (v, wire) in [
            (VerifySessionStatus::Pending, "pending"),
            (VerifySessionStatus::Active, "active"),
            (VerifySessionStatus::Ended, "ended"),
            (VerifySessionStatus::Failed, "failed"),
            (VerifySessionStatus::Expired, "expired"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(wire));
        }
        for (v, wire) in [
            (WizardAckStatus::Sealed, "sealed"),
            (WizardAckStatus::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(wire));
        }
        assert_eq!(JOB_KIND_INTERACTIVE_CAPTURE, "interactive_capture");
        assert_eq!(JOB_STATUS_HOLDING, "holding");
    }

    // ── §1 fencing tuple ────────────────────────────────────────────────────

    #[test]
    fn fencing_tuple_carries_exactly_the_five_fields_top_level() {
        let v = serde_json::to_value(fencing()).unwrap();
        assert_eq!(
            sorted_keys(&v),
            vec![
                "capture_epoch",
                "job_id",
                "lease_token",
                "submission_attempt_id",
                "worker_claim_id",
            ]
        );
        assert_eq!(v["capture_epoch"], json!(0));
        let back: FencingTuple = serde_json::from_value(v).unwrap();
        assert_eq!(back, fencing());
    }

    #[test]
    fn fenced_error_envelope_matches_the_spec_shape() {
        let e: ErrorEnvelope =
            serde_json::from_value(json!({ "error": "fenced", "message": "stale epoch" })).unwrap();
        assert_eq!(e.error, ERROR_CODE_FENCED);
    }

    // ── §3.1 claim response extension ───────────────────────────────────────

    /// The §3.1 per-job object, verbatim from the spec.
    fn claim_job_example() -> Value {
        json!({
            "id": "job_01J1XY",
            "capsule_id": "cap_x",
            "kind": "interactive_capture",
            "target_label": "web",
            "profile": "default",
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "lease_token": "b64u-opaque-token",
            "lease_expires_at": "2026-07-22T09:15:00.000Z"
        })
    }

    #[test]
    fn claim_extension_parses_the_spec_example() {
        let ext: InteractiveCaptureClaimExt = serde_json::from_value(claim_job_example()).unwrap();
        assert_eq!(ext.submission_attempt_id, "subatt_01J1XY");
        assert_eq!(ext.worker_claim_id, "claim_01J1XZ");
        assert_eq!(ext.lease_token, "b64u-opaque-token");
        assert_eq!(ext.lease_expires_at, "2026-07-22T09:15:00.000Z");
    }

    #[test]
    fn claim_extension_fields_do_not_break_the_live_claim_parser() {
        // PR-0 non-goal: existing builders are untouched. The live `ClaimedJob`
        // parser must keep accepting a per-job object that carries the wizard
        // extension fields (it ignores them — no `deny_unknown_fields`).
        let job: crate::ClaimedJob = serde_json::from_value(claim_job_example()).unwrap();
        assert_eq!(job.id, "job_01J1XY");
        // The kind flows through as an opaque string; dispatch keeps failing
        // unknown kinds closed at `claim_kind` (nothing advertises this kind).
        assert_eq!(job.kind, JOB_KIND_INTERACTIVE_CAPTURE);
    }

    // ── §3.2 lease renew ────────────────────────────────────────────────────

    #[test]
    fn lease_renew_request_is_exactly_the_fencing_five() {
        let v = serde_json::to_value::<LeaseRenewRequest>(fencing()).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 5);
        let resp: LeaseRenewResponse =
            serde_json::from_value(json!({ "lease_expires_at": "2026-07-22T09:20:00.000Z" }))
                .unwrap();
        assert_eq!(resp.lease_expires_at, "2026-07-22T09:20:00.000Z");
    }

    // ── §3.3 control poll ───────────────────────────────────────────────────

    #[test]
    fn control_response_parses_the_spec_capture_example() {
        let resp: ControlResponse = serde_json::from_value(json!({
            "directive": "capture",
            "capture_epoch": 3,
            "candidate_id": "cand_01J1Z0",
            "hold_expires_at": "2026-07-22T09:45:00.000Z",
            "pause_permitted": true
        }))
        .unwrap();
        assert_eq!(resp.directive, ControlDirective::Capture);
        assert_eq!(resp.capture_epoch, 3);
        assert_eq!(resp.candidate_id.as_deref(), Some("cand_01J1Z0"));
        assert!(resp.pause_permitted);
        resp.validate().unwrap();
    }

    #[test]
    fn control_response_refinements_fail_closed() {
        // capture without a candidate id
        let no_candidate = ControlResponse {
            directive: ControlDirective::Capture,
            capture_epoch: 3,
            candidate_id: None,
            hold_expires_at: None,
            pause_permitted: true,
        };
        assert!(no_candidate.validate().is_err());
        // capture at epoch 0 (epoch 0 ⇔ "no capture ever requested")
        let epoch_zero = ControlResponse {
            capture_epoch: 0,
            candidate_id: Some("cand_x".into()),
            ..no_candidate.clone()
        };
        assert!(epoch_zero.validate().is_err());
        // hold without a hold deadline
        let hold_no_deadline = ControlResponse {
            directive: ControlDirective::Hold,
            capture_epoch: 0,
            candidate_id: None,
            hold_expires_at: None,
            pause_permitted: false,
        };
        assert!(hold_no_deadline.validate().is_err());
        // a candidate id is only delivered with a capture directive
        let hold_with_candidate = ControlResponse {
            candidate_id: Some("cand_x".into()),
            hold_expires_at: Some("2026-07-22T09:45:00.000Z".into()),
            ..hold_no_deadline.clone()
        };
        assert!(hold_with_candidate.validate().is_err());
        // valid hold / valid discard
        let hold = ControlResponse {
            hold_expires_at: Some("2026-07-22T09:45:00.000Z".into()),
            ..hold_no_deadline
        };
        hold.validate().unwrap();
        let discard = ControlResponse {
            directive: ControlDirective::Discard,
            capture_epoch: 3,
            candidate_id: None,
            hold_expires_at: None,
            pause_permitted: false,
        };
        discard.validate().unwrap();
    }

    #[test]
    fn control_query_carries_the_three_query_params() {
        let q = ControlQuery {
            submission_attempt_id: "subatt_01J1XY".into(),
            worker_claim_id: "claim_01J1XZ".into(),
            capture_epoch: 2,
        };
        assert_eq!(
            sorted_keys(&serde_json::to_value(&q).unwrap()),
            vec!["capture_epoch", "submission_attempt_id", "worker_claim_id"]
        );
        // The token never rides the query string — it is a header (§1).
        assert_eq!(LEASE_TOKEN_HEADER, "x-ato-lease-token");
    }

    // ── §3.4 progress ───────────────────────────────────────────────────────

    #[test]
    fn progress_request_flattens_fencing_plus_stage() {
        let req = ProgressRequest {
            fencing: fencing(),
            stage: WizardStage::Deps,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(
            sorted_keys(&v),
            vec![
                "capture_epoch",
                "job_id",
                "lease_token",
                "stage",
                "submission_attempt_id",
                "worker_claim_id",
            ]
        );
        assert_eq!(v["stage"], json!("deps"));
        let back: ProgressRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back, req);
    }

    // ── §3.5 hold-ready ─────────────────────────────────────────────────────

    fn hold_ready() -> HoldReadyRequest {
        HoldReadyRequest {
            fencing: fencing(),
            builder_id: "builder-sugamo-1".into(),
            slot_id: "slot-3".into(),
            session_id: "sess_01J1Y9".into(),
            guest_port: 8000,
        }
    }

    #[test]
    fn hold_ready_carries_identity_tuple_and_no_upstream_url() {
        let v = serde_json::to_value(hold_ready()).unwrap();
        // ADR-004: the exact key set — no self-reported upstream URL/host/
        // address field exists to smuggle an SSRF target through.
        assert_eq!(
            sorted_keys(&v),
            vec![
                "builder_id",
                "capture_epoch",
                "guest_port",
                "job_id",
                "lease_token",
                "session_id",
                "slot_id",
                "submission_attempt_id",
                "worker_claim_id",
            ]
        );
        hold_ready().validate().unwrap();
    }

    #[test]
    fn hold_ready_validation_bounds() {
        let mut bad_port = hold_ready();
        bad_port.guest_port = 0;
        assert!(bad_port.validate().is_err());
        // > 65535 is unrepresentable: u16 rejects it at parse.
        let mut v = serde_json::to_value(hold_ready()).unwrap();
        v["guest_port"] = json!(70000);
        assert!(serde_json::from_value::<HoldReadyRequest>(v).is_err());

        let mut empty_builder = hold_ready();
        empty_builder.builder_id = String::new();
        assert!(empty_builder.validate().is_err());
        let mut long_slot = hold_ready();
        long_slot.slot_id = "s".repeat(121);
        assert!(long_slot.validate().is_err());
    }

    // ── §3.6 candidate report ───────────────────────────────────────────────

    #[test]
    fn candidate_report_matches_the_spec_example() {
        let v = json!({
            "job_id": "job_01J1XY",
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "lease_token": "b64u-opaque-token",
            "capture_epoch": 3,
            "candidate_id": "cand_01J1Z0",
            "execution_id": "exec_01J1Z1",
            "snapshot_id": "snap_01J1Z2",
            "artifact_location": "r2://snapshots/cand_01J1Z0/seal",
            "source_lost": false
        });
        let req: CandidateReportRequest = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(req.fencing.capture_epoch, 3);
        assert_eq!(req.candidate_id, "cand_01J1Z0");
        assert!(!req.source_lost);
        req.validate().unwrap();
        // Round-trip preserves the exact key set (fencing five + five fields).
        assert_eq!(sorted_keys(&serde_json::to_value(&req).unwrap()), {
            let mut k = sorted_keys(&v);
            k.sort();
            k
        });

        // Epoch 0 can never name a capture command.
        let mut epoch_zero = req.clone();
        epoch_zero.fencing.capture_epoch = 0;
        assert!(epoch_zero.validate().is_err());

        let resp: CandidateReportResponse =
            serde_json::from_value(json!({ "candidate_id": "cand_01J1Z0", "status": "reported" }))
                .unwrap();
        assert_eq!(resp.status, CandidateStatus::Reported);
    }

    // ── §3.7 ack extension ──────────────────────────────────────────────────

    #[test]
    fn sealed_wizard_ack_matches_the_spec_example_and_refinement() {
        let ack: WizardAck = serde_json::from_value(json!({
            "agent_id": "builder-sugamo-1",
            "job_id": "job_01J1XY",
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "lease_token": "b64u-opaque-token",
            "capture_epoch": 3,
            "status": "sealed",
            "accepted_candidate_id": "cand_01J1Z0",
            "acceptance_receipt": {
                "capsule_manifest_hash": "blake3:aa",
                "execution_id": "exec_01J1Z1",
                "artifact_manifest_hash": "blake3:bb",
                "runner_class_id": "rc.kvm.x86_64",
                "snapshot_backend": "firecracker",
                "artifact_location": "r2://snapshots/cand_01J1Z0/seal",
                "snapshot_format_id": "asf.fc-memsnap-v1",
                "snapshot_codec_id": "asc.raw-v1.v1",
                "manifest_source": "recipe_toml"
            }
        }))
        .unwrap();
        assert_eq!(ack.status, WizardAckStatus::Sealed);
        assert_eq!(ack.fencing.capture_epoch, 3);
        ack.validate().unwrap();

        let mut missing_candidate = ack.clone();
        missing_candidate.accepted_candidate_id = None;
        assert!(missing_candidate.validate().is_err());
        let mut missing_receipt = ack.clone();
        missing_receipt.acceptance_receipt = None;
        assert!(missing_receipt.validate().is_err());
    }

    #[test]
    fn failed_wizard_ack_discriminates_capture_seal_vs_acceptance() {
        let mut ack = WizardAck {
            agent_id: "builder-sugamo-1".into(),
            fencing: FencingTuple {
                capture_epoch: 3,
                ..fencing()
            },
            status: WizardAckStatus::Failed,
            accepted_candidate_id: None,
            acceptance_receipt: None,
            failure_stage: Some(WizardFailureStage::CaptureSeal),
            failure_reason: Some("seal aborted".into()),
        };
        ack.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&ack).unwrap()["failure_stage"],
            json!("capture_seal")
        );
        ack.failure_stage = Some(WizardFailureStage::Acceptance);
        ack.validate().unwrap();

        ack.failure_stage = None;
        assert!(ack.validate().is_err());

        ack.failure_stage = Some(WizardFailureStage::Quiescing);
        ack.failure_reason = Some("x".repeat(2001));
        assert!(ack.validate().is_err());
    }

    #[test]
    fn wizard_ack_omits_absent_optionals_on_the_wire() {
        // A failed wizard ack serializes WITHOUT the sealed-only fields, so the
        // shared server-side schema sees no nulls to trip on.
        let ack = WizardAck {
            agent_id: "b".into(),
            fencing: fencing(),
            status: WizardAckStatus::Failed,
            accepted_candidate_id: None,
            acceptance_receipt: None,
            failure_stage: Some(WizardFailureStage::Launch),
            failure_reason: None,
        };
        let v = serde_json::to_value(&ack).unwrap();
        assert_eq!(
            sorted_keys(&v),
            vec![
                "agent_id",
                "capture_epoch",
                "failure_stage",
                "job_id",
                "lease_token",
                "status",
                "submission_attempt_id",
                "worker_claim_id",
            ]
        );
    }

    // ── §4 verify session ───────────────────────────────────────────────────

    #[test]
    fn verify_session_object_round_trips_the_spec_example() {
        let v = json!({
            "verify_session_id": "vsess_01J1Z5",
            "candidate_id": "cand_01J1Z0",
            "status": "active",
            "expires_at": "2026-07-22T10:00:00.000Z"
        });
        let s: VerifySession = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(s.status, VerifySessionStatus::Active);
        assert_eq!(serde_json::to_value(&s).unwrap(), v);
    }

    // ── §5 quiesce messages ─────────────────────────────────────────────────

    #[test]
    fn quiesce_messages_serialize_to_the_three_spec_shapes() {
        assert_eq!(
            serde_json::to_value(QuiesceMessage::Quiesce { epoch: 3 }).unwrap(),
            json!({ "type": "quiesce", "epoch": 3 })
        );
        assert_eq!(
            serde_json::to_value(QuiesceMessage::Quiesced {
                epoch: 3,
                inflight: 0
            })
            .unwrap(),
            json!({ "type": "quiesced", "epoch": 3, "inflight": 0 })
        );
        assert_eq!(
            serde_json::to_value(QuiesceMessage::Unquiesce { epoch: 3 }).unwrap(),
            json!({ "type": "unquiesce", "epoch": 3 })
        );
        let parsed: QuiesceMessage =
            serde_json::from_value(json!({ "type": "quiesced", "epoch": 3, "inflight": 0 }))
                .unwrap();
        parsed.validate().unwrap();
    }

    #[test]
    fn quiesced_with_inflight_traffic_is_not_a_valid_message() {
        // The causality carrier: pause_permitted may only follow inflight: 0.
        assert!(
            QuiesceMessage::Quiesced {
                epoch: 3,
                inflight: 2
            }
            .validate()
            .is_err()
        );
        assert!(QuiesceMessage::Quiesce { epoch: 0 }.validate().is_err());
    }

    // ── §7 capsule.toml declarations ────────────────────────────────────────

    const VALID_DECLS: &str = r#"
[cache.pip]
path = ".venv"
capture = "include"

[cache.models]
path = "models"
capture = "exclude"

[state.db]
path = "data/app.db"
snapshot = "exclude"
schema = "sqlite"
"#;

    #[test]
    fn valid_spec_example_produces_the_declared_path_set() {
        let declared = parse_capture_declarations(VALID_DECLS).unwrap();
        assert_eq!(
            declared.paths,
            BTreeSet::from([
                ".venv".to_string(),
                "models".to_string(),
                "data/app.db".to_string()
            ])
        );
        assert_eq!(declared.refusal_domain, RefusalDomain::ComplementOfDeclared);
    }

    #[test]
    fn declarations_are_optional_and_other_manifest_tables_are_ignored() {
        let declared = parse_capture_declarations("[app]\nname = \"x\"\n").unwrap();
        assert!(declared.paths.is_empty());
        assert_eq!(declared.refusal_domain, RefusalDomain::ComplementOfDeclared);
    }

    #[test]
    fn unknown_key_inside_a_declaration_is_rejected() {
        // Mirrors the api's `.strict()` projection test: `{path, capture,
        // extra}` is invalid. Unknown TABLES elsewhere in the manifest stay
        // ignored (test above); an unknown key INSIDE a declaration must not
        // validate GREEN on one side of the seam only.
        assert!(
            parse_capture_declarations(
                "[cache.pip]\npath = \".venv\"\ncapture = \"include\"\nextra = true\n",
            )
            .is_err()
        );
        assert!(
            parse_capture_declarations(
                "[state.db]\npath = \"data/app.db\"\nsnapshot = \"exclude\"\nschema = \"sqlite\"\nextra = true\n",
            )
            .is_err()
        );
    }

    #[test]
    fn absolute_path_is_rejected() {
        let err = parse_capture_declarations(
            "[cache.pip]\npath = \"/root/.venv\"\ncapture = \"include\"\n",
        )
        .unwrap_err();
        assert!(err.contains("relative"), "{err}");
    }

    #[test]
    fn dotdot_segment_is_rejected() {
        let err =
            parse_capture_declarations("[cache.up]\npath = \"../shared\"\ncapture = \"include\"\n")
                .unwrap_err();
        assert!(err.contains(".."), "{err}");
    }

    #[test]
    fn backslash_path_is_rejected() {
        let err = parse_capture_declarations(
            "[cache.win]\npath = \"data\\\\cache\"\ncapture = \"include\"\n",
        )
        .unwrap_err();
        assert!(err.contains("backslash"), "{err}");
    }

    #[test]
    fn capture_value_outside_include_exclude_fails_at_parse() {
        assert!(
            parse_capture_declarations(
                "[cache.maybe]\npath = \".cache\"\ncapture = \"sometimes\"\n",
            )
            .is_err()
        );
    }

    #[test]
    fn state_is_never_snapshot_included() {
        assert!(
            parse_capture_declarations(
                "[state.db]\npath = \"data/app.db\"\nsnapshot = \"include\"\nschema = \"sqlite\"\n",
            )
            .is_err()
        );
    }

    #[test]
    fn state_requires_a_schema() {
        assert!(
            parse_capture_declarations("[state.nodecl]\npath = \"data\"\nsnapshot = \"exclude\"\n")
                .is_err()
        );
    }

    #[test]
    fn identical_paths_across_sections_are_rejected() {
        let toml_text =
            format!("{VALID_DECLS}\n[cache.dup]\npath = \"data/app.db\"\ncapture = \"include\"\n");
        let err = parse_capture_declarations(&toml_text).unwrap_err();
        assert!(err.contains("identical path"), "{err}");
    }

    #[test]
    fn cross_section_nesting_is_rejected_but_same_section_nesting_is_not() {
        // state path inside a cache path
        let err = parse_capture_declarations(
            "[cache.data]\npath = \"data\"\ncapture = \"include\"\n\n[state.db]\npath = \"data/app.db\"\nsnapshot = \"exclude\"\nschema = \"sqlite\"\n",
        )
        .unwrap_err();
        assert!(err.contains("nest across sections"), "{err}");
        // cache path inside a state path
        assert!(
            parse_capture_declarations(
                "[state.store]\npath = \"var\"\nsnapshot = \"exclude\"\nschema = \"kv-dir\"\n\n[cache.sub]\npath = \"var/cache\"\ncapture = \"include\"\n",
            )
            .is_err()
        );
        // nesting WITHIN a section is not a §7 error (only across sections is)
        parse_capture_declarations(
            "[cache.outer]\npath = \"vendor\"\ncapture = \"include\"\n\n[cache.inner]\npath = \"vendor/bin\"\ncapture = \"exclude\"\n",
        )
        .unwrap();
    }

    #[test]
    fn declaration_names_are_unique_across_cache_and_state() {
        let err = parse_capture_declarations(
            "[cache.db]\npath = \"cachedir\"\ncapture = \"include\"\n\n[state.db]\npath = \"data/app.db\"\nsnapshot = \"exclude\"\nschema = \"sqlite\"\n",
        )
        .unwrap_err();
        assert!(err.contains("unique"), "{err}");
    }

    #[test]
    fn declaration_name_charset_and_length_are_enforced() {
        assert!(
            parse_capture_declarations("[cache.\"UPPER\"]\npath = \"x\"\ncapture = \"include\"\n")
                .is_err()
        );
        let long_name = "a".repeat(41);
        assert!(
            parse_capture_declarations(&format!(
                "[cache.{long_name}]\npath = \"x\"\ncapture = \"include\"\n"
            ))
            .is_err()
        );
    }

    #[test]
    fn state_schema_charset_and_length_are_enforced() {
        assert!(
            parse_capture_declarations(
                "[state.db]\npath = \"d\"\nsnapshot = \"exclude\"\nschema = \"NOT VALID\"\n",
            )
            .is_err()
        );
        let long_schema = "s".repeat(61);
        assert!(
            parse_capture_declarations(&format!(
                "[state.db]\npath = \"d\"\nsnapshot = \"exclude\"\nschema = \"{long_schema}\"\n"
            ))
            .is_err()
        );
        // spec example ids stay valid
        parse_capture_declarations(
            "[state.kv]\npath = \"d\"\nsnapshot = \"exclude\"\nschema = \"kv-dir\"\n",
        )
        .unwrap();
    }

    #[test]
    fn empty_path_is_rejected() {
        assert!(
            parse_capture_declarations("[cache.e]\npath = \"\"\ncapture = \"include\"\n").is_err()
        );
    }

    #[test]
    fn nesting_check_is_segment_boundary_aware() {
        // "database" is NOT inside "data" — no false positive on a shared
        // string prefix.
        parse_capture_declarations(
            "[cache.data]\npath = \"data\"\ncapture = \"include\"\n\n[state.other]\npath = \"database\"\nsnapshot = \"exclude\"\nschema = \"sqlite\"\n",
        )
        .unwrap();
    }
}
