//! Submission Wizard **PR-0 wire contract** — types + validation only, 機能未配線.
//!
//! Serde mirrors of the interactive-capture submission-wizard wire messages
//! defined in the SSOT contract spec at
//! `docs/contracts/SUBMISSION_WIZARD_WIRE_V1.md` (wire contract version
//! [`WIRE_CONTRACT_VERSION`]; any copy outside the repo is non-normative), plus
//! the `[cache.*]` / `[state.*]` capsule.toml declaration schema (§7). The
//! ato-api side carries the same shapes as zod schemas in
//! `src/services/submission_wizard/wire.ts`; both sides test against the exact
//! snake_case wire names in the spec's §9 seam checklist.
//!
//! What is wired, as of PR-2 slice 2:
//! - `"interactive_capture"` is STILL NOT in the claim loop's
//!   `SUPPORTED_JOB_KINDS`, so the api never hands this builder such a job and
//!   nothing below runs in prod against a live hold. That list is the lane's
//!   master switch and stays off until the VM half exists.
//! - The §3.1 claim extension IS attached to the live `ClaimedJob`
//!   (`#[serde(default)]`, so every other kind parses unchanged), and the §3.2
//!   /§3.3/§3.4/§3.5/§3.6/§3.7/§3.8 bodies ARE the request/response types of
//!   `crate::wizard_api` — no wire shape is redeclared there.
//! - Still unwired: the hold/quiesce/capture EXECUTION (no live guest), and the
//!   TOML declaration schema (§7), which no build path consults.
//!
//! The module-level `dead_code` allow below covers the shapes that still have
//! no caller (the §4 verify-session and §5 quiesce types are api-internal
//! mirrors, kept here so the Rust side can seam-test the exact encoding).

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Constants (spec §0 / §2 / wire contract version)
// ─────────────────────────────────────────────────────────────────────────────

/// The wire-contract version literal, pinned on BOTH sides (the api mirror is
/// `WIRE_CONTRACT_VERSION` in `wire.ts`). The claim response extension (§3.1)
/// carries it as a **required literal field** ([`WireContractVersion`]): a
/// value other than this exact string is a schema rejection on both sides, so
/// a version mismatch **fails closed at parse time**, before any semantics run.
pub const WIRE_CONTRACT_VERSION: &str = "ato.submission-wizard-wire/v1";

/// NEW job kind for wizard jobs. Defined but NOT wired in PR-0: never added to
/// the daemon's advertised `supported_kinds` (an unknown kind keeps failing the
/// job closed at stage `claim_kind`, never guessed), and no enqueue accepts it.
///
/// This constant belongs to the *enqueue* kind vocabulary (the api mirror is
/// `WIZARD_WIRE_JOB_KINDS` in `wire.ts`). That vocabulary is NOT the union of
/// the kinds a builder advertises in `supported_kinds`: live builders also
/// advertise `"source_materialize"`, which is not an enqueue kind and must
/// never be rejected when PR-1 validates a claimed kind. Do NOT fold
/// `source_materialize` into the wire-kind vocabulary. (There is no
/// full-vocabulary job-kind enum on this side — only this new constant.)
pub const JOB_KIND_INTERACTIVE_CAPTURE: &str = "interactive_capture";

/// NEW job status for a wizard job whose app is up and held for the submitter.
/// Defined but NOT wired in PR-0: the server-side status DB CHECK is unchanged,
/// so this value cannot yet be persisted (migration is PR-1+).
pub const JOB_STATUS_HOLDING: &str = "holding";

/// Header carrying the `lease_token` on **ALL** builder endpoints after claim,
/// GET and POST alike (§1.1, D2). Canonical spelling per the spec
/// (case-insensitive per HTTP; HTTP/2 lowercases on the wire). The token is
/// never a query param and never a body field — this keeps the secret out of
/// URLs, access logs, and body-logging/tracing pipelines. Strict bodies REJECT
/// a `lease_token` key (tested below); the only JSON appearance of the token
/// is the claim response that mints it (§3.1).
pub const LEASE_TOKEN_HEADER: &str = "X-Ato-Lease-Token";

/// `error` code of the `409 { "error": "fenced", "message": ... }` envelope
/// rejecting any FENCING-4 or per-endpoint epoch-rule violation (spec §1). A
/// fenced request has NO side effects server-side.
pub const ERROR_CODE_FENCED: &str = "fenced";

/// Required literal for `acceptance_receipt.receipt_schema` (§3.7, D3): the
/// receipt is a **versioned envelope**, not a pinned field list. See
/// [`AcceptanceReceipt`].
pub const ACCEPTANCE_RECEIPT_SCHEMA: &str = "ato.snapshot-acceptance/v1";

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

/// Required-literal field type for `wire_contract_version` (§3.1). Serializes
/// to exactly [`WIRE_CONTRACT_VERSION`]; deserializing ANY other value fails —
/// the fail-closed version gate lives in the type, mirroring the api's
/// `z.literal(WIRE_CONTRACT_VERSION)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WireContractVersion;

impl Serialize for WireContractVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(WIRE_CONTRACT_VERSION)
    }
}

impl<'de> Deserialize<'de> for WireContractVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == WIRE_CONTRACT_VERSION {
            Ok(WireContractVersion)
        } else {
            Err(serde::de::Error::custom(format!(
                "wire_contract_version mismatch: expected {WIRE_CONTRACT_VERSION:?}, got {value:?} (fail-closed version gate)"
            )))
        }
    }
}

/// A `wire_contract_version` as it arrives on ONE job of a CLAIM BATCH (§3.1).
///
/// [`WireContractVersion`] rejects a skewed value at parse, which is exactly
/// right for a wizard message: the whole body belongs to one attempt, so a
/// version skew must fail the body. A claim response is not that shape — it is a
/// batch of jobs of several kinds parsed as ONE document, so a required-literal
/// field there gives a single skewed wizard job the power to fail the parse of
/// the healthy recipe / import jobs beside it, dropping work that has nothing to
/// do with the wizard.
///
/// This type keeps the gate and SCOPES it to the job it gates: any string
/// deserializes here, and the literal is enforced by [`Self::supported`], which
/// the job's own claim-extension assembly calls before any wizard semantics run.
/// A skewed job therefore still fails closed — it simply fails alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimedWireContractVersion {
    /// Exactly [`WIRE_CONTRACT_VERSION`].
    Supported(WireContractVersion),
    /// Any other value. Carried so the diagnostic can name it, never honoured.
    Skewed(String),
}

impl<'de> Deserialize<'de> for ClaimedWireContractVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == WIRE_CONTRACT_VERSION {
            Ok(ClaimedWireContractVersion::Supported(WireContractVersion))
        } else {
            Ok(ClaimedWireContractVersion::Skewed(value))
        }
    }
}

impl ClaimedWireContractVersion {
    /// The fail-closed version gate, applied to the one job that carries it.
    pub fn supported(&self) -> Result<WireContractVersion, String> {
        match self {
            ClaimedWireContractVersion::Supported(version) => Ok(*version),
            ClaimedWireContractVersion::Skewed(value) => Err(format!(
                "wire_contract_version mismatch: expected {WIRE_CONTRACT_VERSION:?}, got {value:?} (fail-closed version gate)"
            )),
        }
    }
}

/// Required-literal field type for `receipt_schema` (§3.7, D3). Serializes to
/// exactly [`ACCEPTANCE_RECEIPT_SCHEMA`]; any other value fails at parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcceptanceReceiptSchema;

impl Serialize for AcceptanceReceiptSchema {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(ACCEPTANCE_RECEIPT_SCHEMA)
    }
}

impl<'de> Deserialize<'de> for AcceptanceReceiptSchema {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == ACCEPTANCE_RECEIPT_SCHEMA {
            Ok(AcceptanceReceiptSchema)
        } else {
            Err(serde::de::Error::custom(format!(
                "receipt_schema mismatch: expected {ACCEPTANCE_RECEIPT_SCHEMA:?}, got {value:?}"
            )))
        }
    }
}

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

/// `failure_stage` on the wizard terminal ack (§2, §3.8): any coarse stage
/// value plus the two failure-only discriminators — `capture_seal` (failure
/// while sealing the captured filesystem/snapshot) vs `acceptance`
/// (acceptance-time failure). On the wizard terminal ack this is an OPTIONAL
/// diagnostic refinement of `reason`, never a substitute for it.
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
    /// Perform capture for the response's `server_capture_epoch`.
    Capture,
    /// Tear down without capturing; the attempt is over for this claim
    /// (terminal ack `reason: "discarded"`, §3.8).
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

/// Acceptance outcome (§2, §3.7 body + response) — the outcome of one
/// candidate's acceptance run, NOT a job-terminal status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceStatus {
    Accepted,
    Rejected,
}

/// Wizard terminal-ack `reason` (§2, §3.8) — the ONLY legal job-terminal
/// reasons for an `interactive_capture` job. The legacy `status: "sealed"`
/// terminal ack is **not used** by wizard jobs (candidate acceptance is §3.7's
/// per-candidate endpoint and does not end the job); the api side enforces
/// that with a refinement on the shared ack schema, this side by construction
/// ([`WizardTerminalAck`] has no `status` member — a `"sealed"` payload fails
/// its strict body, tested below).
///
/// `"lease_expired"` is deliberately **not** a member: lease expiry is
/// SERVER-OWNED. The API's lease sweep transitions the attempt to `expired`
/// and revokes its bindings; the builder observes `409 fenced` on its next
/// renew/control call and tears down LOCALLY, **without** sending a terminal
/// ack. An expired-lease terminal ack is unsendable — FENCING-4 would `409`
/// it because the lease is already dead — and the sweep alone moves the
/// attempt to `expired` (no builder ack required). Reason → job-terminal
/// projection: `Discarded`/`AttemptEnded` → `ended`;
/// `BuildFailed`/`AcceptanceFailedSourceLost` → `failed`; lease expiry →
/// server-owned `expired` (sweep, no builder ack). Server enforcement lands in
/// PR-1; PR-0 pins the enum + spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAckReason {
    /// Server directed `discard` on the control channel.
    Discarded,
    /// Build/boot never reached `holding`.
    BuildFailed,
    /// ADR-012 terminal branch: source lost AND the acceptance run failed.
    AcceptanceFailedSourceLost,
    /// Orderly end of the interactive attempt (publisher done / session
    /// ended).
    AttemptEnded,
}

// ─────────────────────────────────────────────────────────────────────────────
// FENCING-4 + per-endpoint epoch rules (spec §1)
// ─────────────────────────────────────────────────────────────────────────────

/// The `lease_token` (§1.1, D2) as a type that **cannot leak through a derived
/// `Debug`**. The token is the one builder-held secret of the wizard lane, and
/// the concrete leak path is not a deliberate `println!` — it is a `{:?}` of
/// some struct that happens to carry it (a claimed job, a fencing tuple, an
/// error context). A bare `String` field makes that leak the DEFAULT for every
/// future struct; this newtype makes it impossible, and funnels every legitimate
/// use through the single, greppable [`LeaseToken::expose`] call site (only the
/// [`LEASE_TOKEN_HEADER`] value ever needs it).
///
/// `#[serde(transparent)]` keeps the wire byte-identical: the token still
/// serializes as a bare JSON string in the one message that mints it (§3.1), and
/// still never appears in any request body (the strict bodies have no such key).
///
/// `PartialEq` here is the ordinary derived comparison: the builder never
/// compares tokens (it only echoes its own). The constant-time comparison
/// against a stored HASH is the SERVER's rule, on the server's side.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseToken(String);

impl LeaseToken {
    pub fn new(token: String) -> Self {
        LeaseToken(token)
    }

    /// The ONLY way to read the secret. Legitimate callers: the
    /// [`LEASE_TOKEN_HEADER`] value on a builder request. Nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Redacted — see the type doc. Deliberately hand-written, never derived, and
/// deliberately WITHOUT a `Display` impl (a `Display` would make `{}` of the
/// token compile, which is exactly the accident this type exists to prevent).
impl std::fmt::Debug for LeaseToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LeaseToken(<redacted>)")
    }
}

/// **FENCING-4** (§1.1): the 4-tuple every builder-originated request after
/// claim — control poll, lease renew, progress, hold-ready, candidate report,
/// candidate acceptance, terminal ack — MUST carry. The server compares all
/// four against its authoritative row with exact equality; any mismatch, or an
/// expired lease, rejects with `409 { "error": "fenced" }` and the request has
/// NO side effects.
///
/// `capture_epoch` is **NOT** part of claim fencing (B1): it is a
/// message-specific command cursor — see [`control_poll_epoch_rule`] and
/// [`candidate_epoch_rule`].
///
/// This struct is deliberately **not serializable**: the tuple never appears
/// whole in any one wire position. Its transport is split (§1.1, uniform
/// across ALL endpoints, GET and POST):
///
/// - `job_id`: always in the URL path (`/jobs/:job_id/...`), never repeated in
///   a body;
/// - `lease_token`: always in the [`LEASE_TOKEN_HEADER`] request header, never
///   a query param and never a body field (strict bodies reject the key);
/// - `submission_attempt_id` + `worker_claim_id`: top-level body fields on
///   POSTs, query params on the control-poll GET.
///
/// `Debug` is safe to derive ONLY because `lease_token` is a [`LeaseToken`],
/// which redacts itself.
#[derive(Debug, Clone)]
pub struct Fencing4 {
    /// `job_` — issued at enqueue (existing convention). Path-only.
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
    /// `worker_claim_id`. **Server storage is hash-only** — the server
    /// persists a hash, never the token (PR-1 concern; PR-0 documents it
    /// here). Builders must never log it, never put it in URLs or request
    /// bodies — it travels ONLY in the [`LEASE_TOKEN_HEADER`] header, and its
    /// only JSON appearance is the claim response that mints it (§3.1).
    pub lease_token: LeaseToken,
}

/// §1.2 SERVER rule for the control poll (route enforcement is PR-1; PR-0 pins
/// the rule here so tests §1.3 (a)/(b) name it): the builder's
/// `observed_capture_epoch` is **ACCEPTED when `observed <= server_epoch`** —
/// a stale observer is *behind*, not impostored, and is served the current
/// authoritative state so it can catch up — and **REJECTED (`409 fenced`) only
/// when `observed > server_epoch`** (a builder cannot have observed the
/// future; treat as corrupt/forged state).
pub fn control_poll_epoch_rule(
    observed_capture_epoch: u64,
    server_capture_epoch: u64,
) -> Result<(), String> {
    if observed_capture_epoch > server_capture_epoch {
        return Err(format!(
            "fenced: observed_capture_epoch {observed_capture_epoch} is ahead of the server epoch {server_capture_epoch}"
        ));
    }
    Ok(())
}

/// §1.2 SERVER rule for candidate report (§3.6) and candidate acceptance
/// (§3.7) (route enforcement is PR-1; PR-0 pins the rule here so tests §1.3
/// (c)/(d) name it): the message's `capture_epoch` must **exactly equal** the
/// epoch of the candidate named by `candidate_id` (epoch ↔ candidate is 1:1).
/// A report/acceptance for a superseded epoch is rejected `409 fenced`.
/// `capture_epoch` here is NOT a fencing-tuple member — it is cross-checked
/// against the candidate, in addition to FENCING-4 on the claim.
pub fn candidate_epoch_rule(
    message_capture_epoch: u64,
    candidate_capture_epoch: u64,
) -> Result<(), String> {
    if message_capture_epoch != candidate_capture_epoch {
        return Err(format!(
            "fenced: capture_epoch {message_capture_epoch} does not match the candidate's epoch {candidate_capture_epoch}"
        ));
    }
    Ok(())
}

/// The standard error envelope, e.g. the §1 fencing rejection
/// `409 { "error": "fenced", "message": "..." }` (see [`ERROR_CODE_FENCED`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: String,
    pub message: String,
}

/// Shared bound: `failure_reason` ≤ 2000 **UTF-16 code units**
/// (`encode_utf16().count()`), matching the api's `z.string().max(2000)` (zod
/// counts `String.length` = UTF-16 code units) — an astral scalar counts as 2,
/// so a green `failure_reason` on one side is green on the other (seam: one
/// verdict on both sides). The builder truncates at 1800, as the existing
/// failed ack does.
fn validate_failure_reason_bound(failure_reason: Option<&str>) -> Result<(), String> {
    if let Some(reason) = failure_reason
        && reason.encode_utf16().count() > 2000
    {
        return Err("failure_reason must be <= 2000 UTF-16 code units".into());
    }
    Ok(())
}

/// §3 null policy — optional fields are encoded by **omission**: an absent
/// optional is OMITTED from the JSON entirely, and an explicit `null` is a
/// schema reject on BOTH sides. The api's zod `.optional()` admits only an
/// absent key (`undefined`), never `null` — so this side must reject `null`
/// too, or a body green here would be 400'd there (one verdict on the seam).
/// A bare serde `Option` would parse `null` as `None`; this `deserialize_with`
/// fn is reached only when the key IS present, where the inner type's own
/// deserializer then rejects `null`. Absence still goes through
/// `#[serde(default)]` → `None`, and serialization still omits the key
/// (`skip_serializing_if`) — this side never emits `null`.
fn reject_explicit_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

// ─────────────────────────────────────────────────────────────────────────────
// Builder-lane messages (spec §3)
// ─────────────────────────────────────────────────────────────────────────────

/// §3.1 — the fields the per-job object in the claim response gains when (and
/// only when) the job kind is [`JOB_KIND_INTERACTIVE_CAPTURE`]. PR-1 merges
/// these onto the live `ClaimedJob` as `#[serde(default)]` optionals so
/// builders that never advertise the kind are untouched; PR-0 keeps the live
/// claim parser byte-identical and carries the extension here only.
///
/// This claim response is the ONLY message in which the `lease_token` appears
/// in a JSON payload — every subsequent request carries it in the
/// [`LEASE_TOKEN_HEADER`] header instead (§1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveCaptureClaimExt {
    /// Required literal [`WIRE_CONTRACT_VERSION`]; any other value is a schema
    /// reject (fail-closed version gate on both sides).
    pub wire_contract_version: WireContractVersion,
    /// Fixed at enqueue; echoed in FENCING-4.
    pub submission_attempt_id: String,
    /// Fresh per claim generation; echoed in FENCING-4.
    pub worker_claim_id: String,
    /// Opaque secret (see [`Fencing4::lease_token`]). A [`LeaseToken`], so a
    /// `{:?}` of a claimed job can never print it.
    pub lease_token: LeaseToken,
    /// ISO-8601 UTC lease deadline; the builder must renew before this.
    pub lease_expires_at: String,
}

/// §3.2 request — `POST /v1/capsule-snapshots/jobs/:job_id/lease/renew`.
/// Header: [`LEASE_TOKEN_HEADER`]. Fencing: FENCING-4 (`job_id` in the path).
/// Epoch: none (§1.2). The body is exactly these two fields; the strict body
/// rejects a `lease_token` or `job_id` key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRenewRequest {
    pub submission_attempt_id: String,
    pub worker_claim_id: String,
}

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
/// FENCING-4. Strict, so neither the token nor `job_id` can ride the query
/// string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlQuery {
    pub submission_attempt_id: String,
    pub worker_claim_id: String,
    /// Highest epoch the builder has observed (`0` if none). NOT a fencing
    /// field (B1): server rule is [`control_poll_epoch_rule`] — accepted when
    /// `<=` the server epoch, fenced only when ahead of it.
    pub observed_capture_epoch: u64,
}

/// §3.3 — control poll response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub directive: ControlDirective,
    /// Current authoritative epoch. With `directive: "capture"` it is ≥ 1 and
    /// names the command; the builder adopts it as its observed epoch. `0` ⇔ no
    /// capture ever requested. MAY exceed the request's
    /// `observed_capture_epoch` — that is how a stale observer catches up
    /// (§1.3 test (a)).
    pub server_capture_epoch: u64,
    /// Present only when `directive: "capture"`: the pre-minted candidate for
    /// this epoch (epoch ↔ candidate is 1:1); the builder echoes it back in the
    /// candidate report and in the acceptance path. Absent ⇒ omitted, never
    /// `null` ([`reject_explicit_null`]).
    #[serde(
        default,
        deserialize_with = "reject_explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub candidate_id: Option<String>,
    /// ISO-8601 UTC; required when `directive: "hold"` — the server-side hold
    /// deadline. After it, expect `discard`. Absent ⇒ omitted, never `null`
    /// ([`reject_explicit_null`]).
    #[serde(
        default,
        deserialize_with = "reject_explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
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
                if self.server_capture_epoch == 0 {
                    return Err("directive \"capture\" requires server_capture_epoch >= 1".into());
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

/// §3.4 — `POST /v1/capsule-snapshots/jobs/:job_id/progress`.
/// Header: [`LEASE_TOKEN_HEADER`]. Fencing: FENCING-4. Epoch: none (§1.2).
/// Response `200 {}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressRequest {
    pub submission_attempt_id: String,
    pub worker_claim_id: String,
    /// Coarse progress only — the type ([`WizardStage`], 9 values) excludes the
    /// failure discriminators by construction. Monotonic advance is NOT
    /// enforced on the wire (retries/restarts within a claim may repeat a
    /// stage).
    pub stage: WizardStage,
}

/// §3.5 — `POST /v1/capsule-snapshots/jobs/:job_id/hold-ready`, sent once when
/// the app is up and the builder enters `holding`.
/// Header: [`LEASE_TOKEN_HEADER`]. Fencing: FENCING-4. Epoch: none (§1.2).
/// Response `200 {}`.
///
/// **Deliberately absent (ADR-004, SSRF)**: there is NO self-reported upstream
/// URL/host/address field, and the api rejects unknown fields here
/// (`.strict()` server-side, `deny_unknown_fields` here). The api derives the
/// proxy upstream itself from `(builder_id, slot_id, session_id, guest_port)`
/// against its own registry of builder ingress addresses — a builder can never
/// point the proxy at an arbitrary URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldReadyRequest {
    pub submission_attempt_id: String,
    pub worker_claim_id: String,
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
    /// Length bounds are measured in **UTF-16 code units**
    /// (`encode_utf16().count()`), matching the api's `builderLocalIdSchema`
    /// (`z.string().min(1).max(120)`, whose zod `.max()` counts `String.length`
    /// = UTF-16 code units) — never `chars().count()` — so a string holding an
    /// astral scalar (2 code units) gets one verdict on both sides of the seam.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("builder_id", &self.builder_id),
            ("slot_id", &self.slot_id),
            ("session_id", &self.session_id),
        ] {
            if value.is_empty() || value.encode_utf16().count() > 120 {
                return Err(format!("{name} must be 1..120 UTF-16 code units"));
            }
        }
        if self.guest_port == 0 {
            return Err("guest_port must be 1..65535".into());
        }
        Ok(())
    }
}

/// §3.6 — `POST /v1/capsule-snapshots/jobs/:job_id/candidates`: reports a
/// **sealed** candidate after a `capture` directive completes at seal.
/// Header: [`LEASE_TOKEN_HEADER`]. Fencing: FENCING-4.
/// Epoch rule (§1.2): body `capture_epoch` must exactly equal the epoch of the
/// candidate named by `candidate_id` ([`candidate_epoch_rule`], tests §1.3
/// (c)/(d)).
///
/// A capture that fails before seal produces NO candidate report — with the
/// source VM alive the attempt simply returns to `holding` and resumes
/// polling; only a terminal condition goes through the ack (§3.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateReportRequest {
    pub submission_attempt_id: String,
    pub worker_claim_id: String,
    /// The epoch being reported, ≥ 1. NOT a fencing-tuple member (B1) — the
    /// server exact-matches it against the candidate's epoch; a report for a
    /// superseded epoch is rejected `409 fenced`.
    pub capture_epoch: u64,
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
    /// `true` ⇒ the live session died/was destroyed during or after capture
    /// (ADR-012 `accepting_source_lost`); the candidate is still reportable
    /// but no further captures can come from this claim without a fresh
    /// launch.
    pub source_lost: bool,
}

impl CandidateReportRequest {
    /// String length bounds are measured in **UTF-16 code units**
    /// (`encode_utf16().count()`), matching the api's zod
    /// (`execution_id`/`snapshot_id` `z.string().min(1).max(200)`,
    /// `artifact_location` `z.string().min(1).max(500)`) — an astral scalar
    /// counts as 2, so a green report on one side is green on the other (seam:
    /// one verdict on both sides).
    pub fn validate(&self) -> Result<(), String> {
        if self.capture_epoch == 0 {
            return Err("candidate report requires capture_epoch >= 1".into());
        }
        for (name, value, max) in [
            ("execution_id", &self.execution_id, 200usize),
            ("snapshot_id", &self.snapshot_id, 200),
            ("artifact_location", &self.artifact_location, 500),
        ] {
            if value.is_empty() || value.encode_utf16().count() > max {
                return Err(format!("{name} must be 1..{max} UTF-16 code units"));
            }
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

/// §3.7, D3 — the acceptance receipt is a **versioned envelope**, NOT a pinned
/// field list. `receipt` is a required opaque JSON **object** whose payload
/// schema is defined as a shared type in ato#1088 (post-Gate-0); until then
/// BOTH sides validate ONLY the envelope (literal + "is an object"), never
/// individual payload keys. The earlier 9-key required core from the seam
/// round is superseded and its pinning tests are removed.
///
/// The envelope is **strict on both sides** (api `.strict()`,
/// `deny_unknown_fields` here): payload keys live INSIDE `receipt`, never
/// beside it. `deny_unknown_fields` on the outer
/// [`CandidateAcceptanceRequest`] does NOT propagate to nested structs, so the
/// envelope must pin its own strictness or an unknown key beside `receipt`
/// would parse green here and be rejected by the api (tested below).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceReceipt {
    /// Required literal [`ACCEPTANCE_RECEIPT_SCHEMA`].
    pub receipt_schema: AcceptanceReceiptSchema,
    /// Opaque payload object — the `Map` type enforces "is an object" at
    /// parse; no payload key is validated in PR-0.
    pub receipt: serde_json::Map<String, serde_json::Value>,
}

/// §3.7 (B2) — NEW endpoint
/// `POST /v1/capsule-snapshots/jobs/:job_id/candidates/:candidate_id/acceptance`.
/// Header: [`LEASE_TOKEN_HEADER`]. Fencing: FENCING-4.
/// Epoch rule (§1.2): body `capture_epoch` must exactly equal the epoch of the
/// path `candidate_id` ([`candidate_epoch_rule`]).
///
/// Reports the outcome of the acceptance run (disposable-restore validation,
/// ato#1088) for one candidate. **This is NOT a job-terminal ack.** With the
/// source VM available, the attempt returns to `holding` after acceptance —
/// whether accepted (publisher may retake) or rejected (candidate discarded,
/// re-capture possible) — per the design doc §5 state machine and ADR-012.
/// Only the `accepting_source_lost` + acceptance-failure branch ends the
/// attempt, and that goes through the terminal ack (§3.8,
/// `acceptance_failed_source_lost`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAcceptanceRequest {
    pub submission_attempt_id: String,
    pub worker_claim_id: String,
    /// ≥ 1; exact-match against the candidate's epoch (§1.2).
    pub capture_epoch: u64,
    /// Outcome of the acceptance run.
    pub status: AcceptanceStatus,
    /// Required when `status: "accepted"`; absent when `"rejected"`
    /// ([`Self::validate`]). Absent ⇒ omitted, never `null`
    /// ([`reject_explicit_null`]).
    #[serde(
        default,
        deserialize_with = "reject_explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub acceptance_receipt: Option<AcceptanceReceipt>,
    /// Optional, only with `status: "rejected"`; ≤ 2000 UTF-16 code units
    /// (builder truncates at 1800). Absent ⇒ omitted, never `null`
    /// ([`reject_explicit_null`]).
    #[serde(
        default,
        deserialize_with = "reject_explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub failure_reason: Option<String>,
}

impl CandidateAcceptanceRequest {
    /// The §3.7 required-by-refinement rules (mirrored in the api's zod
    /// refinements).
    pub fn validate(&self) -> Result<(), String> {
        if self.capture_epoch == 0 {
            return Err("candidate acceptance requires capture_epoch >= 1".into());
        }
        match self.status {
            AcceptanceStatus::Accepted => {
                if self.acceptance_receipt.is_none() {
                    return Err("status \"accepted\" requires acceptance_receipt".into());
                }
                if self.failure_reason.is_some() {
                    return Err("failure_reason is only legal with status \"rejected\"".into());
                }
            }
            AcceptanceStatus::Rejected => {
                if self.acceptance_receipt.is_some() {
                    return Err("acceptance_receipt is absent when status is \"rejected\"".into());
                }
            }
        }
        validate_failure_reason_bound(self.failure_reason.as_deref())
    }
}

/// §3.7 — candidate acceptance response (acceptance status enum; the
/// candidate's own status moves to `accepted`/`rejected` per §2's candidate
/// status enum).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateAcceptanceResponse {
    pub candidate_id: String,
    pub status: AcceptanceStatus,
}

/// §3.8 — the wizard **terminal-ack payload** for the existing
/// `POST /v1/capsule-snapshots/jobs/:job_id/ack`, used by (and only by)
/// [`JOB_KIND_INTERACTIVE_CAPTURE`] jobs (discriminated by job kind; existing
/// non-wizard acks are untouched).
/// Header: [`LEASE_TOKEN_HEADER`]. Fencing: FENCING-4. Epoch: none (§1.2).
///
/// **The legacy `status: "sealed"` terminal ack is NOT used by wizard jobs**
/// (§2 note): there is no `status`, no `accepted_candidate_id`, and no receipt
/// here — candidate acceptance is §3.7's per-candidate endpoint and is not
/// job-terminal. The api side refines `"sealed"` invalid for this kind on the
/// shared ack schema; this side enforces the absence by construction (the
/// strict body rejects those keys, tested below).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WizardTerminalAck {
    /// Existing field, unchanged (existing bounds).
    pub agent_id: String,
    pub submission_attempt_id: String,
    pub worker_claim_id: String,
    /// The ONLY legal job-terminal reasons for a wizard job
    /// ([`TerminalAckReason`]).
    pub reason: TerminalAckReason,
    /// Optional diagnostic refinement of a failure `reason` — never a
    /// substitute for it (§2). Absent ⇒ omitted, never `null`
    /// ([`reject_explicit_null`]).
    #[serde(
        default,
        deserialize_with = "reject_explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub failure_stage: Option<WizardFailureStage>,
    /// Optional, ≤ 2000 UTF-16 code units server-side (the builder truncates
    /// at 1800, as the existing failed ack does). Absent ⇒ omitted, never
    /// `null` ([`reject_explicit_null`]).
    #[serde(
        default,
        deserialize_with = "reject_explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub failure_reason: Option<String>,
}

impl WizardTerminalAck {
    /// The §3.8 bounds (the reason enum itself is enforced by the type).
    /// `agent_id` is 1..120 **UTF-16 code units** (`encode_utf16().count()`),
    /// matching the api's `builderLocalIdSchema`
    /// (`z.string().min(1).max(120)`) — one verdict on both sides.
    pub fn validate(&self) -> Result<(), String> {
        if self.agent_id.is_empty() || self.agent_id.encode_utf16().count() > 120 {
            return Err("agent_id must be 1..120 UTF-16 code units".into());
        }
        validate_failure_reason_bound(self.failure_reason.as_deref())
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
/// within the drain window, the api never sets `pause_permitted`, aborts the
/// capture for that epoch (the attempt returns to `holding` per ADR-007; a
/// later epoch may retry), and sends `unquiesce`. The system NEVER
/// force-captures under live traffic.
///
/// **Strictness (seam):** the api's three `quiesce`/`quiesced`/`unquiesce`
/// zod schemas are each `.strict()`, so an unknown key on any shape (e.g.
/// `inflight` on `unquiesce`, or a stray field beside a valid message) is a
/// 400 there. An internally-tagged serde enum cannot carry
/// `deny_unknown_fields` (the tag is buffered into the variant), so it would
/// silently *ignore* those keys — two verdicts on the §5 seam. `QuiesceMessage`
/// therefore hand-discriminates on `type` (custom `Deserialize` below) and
/// parses the remainder with one of three `deny_unknown_fields` body structs,
/// rejecting unknown keys exactly as the api does. `Serialize` still emits the
/// internally-tagged `{ "type": …, … }` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

/// One `deny_unknown_fields` body per §5 shape — the strict mirror of the api's
/// three `.strict()` zod schemas. The `type` discriminant is stripped by
/// [`QuiesceMessage`]'s custom `Deserialize` before the remainder is parsed
/// here, so an unknown key (e.g. `inflight` on [`UnquiesceBody`]) is rejected.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuiesceBody {
    epoch: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuiescedBody {
    epoch: u64,
    inflight: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnquiesceBody {
    epoch: u64,
}

impl<'de> Deserialize<'de> for QuiesceMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        // Buffer the object, lift out the `type` discriminant, then parse the
        // remaining fields with the matching strict body struct — the tag is
        // gone, so `deny_unknown_fields` sees only the shape's own fields and
        // rejects anything extra, exactly as the api's per-schema `.strict()`.
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| D::Error::custom("quiesce message must be a JSON object"))?;
        let tag = match obj.remove("type") {
            Some(serde_json::Value::String(s)) => s,
            Some(_) => return Err(D::Error::custom("quiesce message `type` must be a string")),
            None => return Err(D::Error::missing_field("type")),
        };
        let rest = serde_json::Value::Object(std::mem::take(obj));
        match tag.as_str() {
            "quiesce" => {
                let body = QuiesceBody::deserialize(rest).map_err(D::Error::custom)?;
                Ok(QuiesceMessage::Quiesce { epoch: body.epoch })
            }
            "quiesced" => {
                let body = QuiescedBody::deserialize(rest).map_err(D::Error::custom)?;
                Ok(QuiesceMessage::Quiesced {
                    epoch: body.epoch,
                    inflight: body.inflight,
                })
            }
            "unquiesce" => {
                let body = UnquiesceBody::deserialize(rest).map_err(D::Error::custom)?;
                Ok(QuiesceMessage::Unquiesce { epoch: body.epoch })
            }
            other => Err(D::Error::unknown_variant(
                other,
                &["quiesce", "quiesced", "unquiesce"],
            )),
        }
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
    /// Guest-absolute path (see [`validate_declared_path`]).
    pub path: String,
    pub capture: CaptureMode,
}

/// §7 — `[state.<name>]`: never baked; runtime durable state. Unknown keys
/// INSIDE the declaration are rejected, same as [`CacheDeclaration`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDeclaration {
    /// Guest-absolute path (see [`validate_declared_path`]).
    pub path: String,
    pub snapshot: StateSnapshotMode,
    /// Free-form schema id, 1..60 chars, `[a-z0-9_.-]` (e.g. `"sqlite"`,
    /// `"kv-dir"`, `"1"`).
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
    // Length in UTF-16 code units to match the api's `.max()` (moot given the
    // ASCII-only charset below, but kept uniform with the other mirrored bounds).
    if name.is_empty() || name.encode_utf16().count() > 40 {
        return Err(format!(
            "declaration name {name:?} must be 1..40 UTF-16 code units"
        ));
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

/// §7 `path` (D1) — **GUEST-ABSOLUTE**, identical validator on BOTH sides (the
/// api mirror lives in `wire.ts`; design doc §1.2 examples are
/// `/var/cache/example-model` and `/data` — the declarations name guest
/// filesystem surfaces, and there is no defined base to resolve a relative
/// path against). The rules, in check order:
///
/// - non-empty, ≤ 200 **UTF-16 code units** (`encode_utf16().count()`,
///   matching the api's `z.string().max(200)` — a path may hold astral
///   scalars, each 2 code units, so counting scalars would diverge from the
///   api; this is the load-bearing length bound of the seam);
/// - no backslashes anywhere;
/// - no scheme prefix: any `:` before the first `/` is rejected (e.g.
///   `r2://x`; `file:///models` also fails the leading-`/` rule);
/// - MUST start with a **single** `/` (relative paths and a leading `//` are
///   both rejected);
/// - bare `"/"` is rejected (the path must name a surface below the root);
/// - no empty segments (no `//` anywhere, no trailing `/`) and no `.` or `..`
///   segments — so every input that would need normalizing is rejected, and
///   the collision/nesting checks can run on the absolute paths exactly as
///   declared.
fn validate_declared_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("path must be non-empty".into());
    }
    if path.encode_utf16().count() > 200 {
        return Err("path must be <= 200 UTF-16 code units".into());
    }
    if path.contains('\\') {
        return Err("path must not contain backslashes".into());
    }
    if let Some(colon) = path.find(':')
        && path.find('/').is_none_or(|slash| colon < slash)
    {
        return Err("path must not carry a scheme prefix (':' before the first '/')".into());
    }
    if !path.starts_with('/') {
        return Err("path must be guest-absolute (start with a single '/')".into());
    }
    if path.starts_with("//") {
        return Err("path must start with a SINGLE '/' (leading '//' is rejected)".into());
    }
    let rest = &path[1..];
    if rest.is_empty() {
        return Err("bare '/' is rejected (the path must name a surface below the root)".into());
    }
    for segment in rest.split('/') {
        if segment.is_empty() {
            return Err("path must not contain an empty segment (no '//' or trailing '/')".into());
        }
        if segment == "." || segment == ".." {
            return Err("path must not contain '.' or '..' segments".into());
        }
    }
    Ok(())
}

/// §7 `schema`: free-form id, 1..60 chars, `[a-z0-9_.-]`.
fn validate_state_schema(schema: &str) -> Result<(), String> {
    // Length in UTF-16 code units to match the api's `.max()` (moot given the
    // ASCII-only charset below, but kept uniform with the other mirrored bounds).
    if schema.is_empty() || schema.encode_utf16().count() > 60 {
        return Err(format!("schema {schema:?} must be 1..60 UTF-16 code units"));
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
/// aware: `/data/app.db` is inside `/data`, but `/database` is not).
///
/// Inputs are pre-validated by [`validate_declared_path`], which rejects
/// every normalizable input (`//`, `.`/`..`, trailing `/`) — so this helper
/// runs on the **absolute paths exactly as declared** and is the exact logical
/// twin of the api's `pathNestedInside` (`wire.ts`): no normalization pass on
/// either side (spec §7: one verdict on both sides).
fn path_is_nested_inside(inner: &str, outer: &str) -> bool {
    inner.len() > outer.len()
        && inner.starts_with(outer)
        && inner.as_bytes().get(outer.len()) == Some(&b'/')
}

impl CaptureDeclarations {
    /// The §7 validation rules, producing the PR-0 output ([`DeclaredPaths`]):
    /// name charset + cross-section uniqueness, per-path constraints, no two
    /// identical paths, and no nesting between ANY two declarations —
    /// cache↔cache, cache↔state, state↔state alike (an ancestor/descendant
    /// relation on the declared paths, not only across sections). Collision +
    /// nesting are computed on the absolute paths as declared (no
    /// normalization — see [`validate_declared_path`]). Nested surfaces
    /// (longest-prefix precedence etc.) are deferred to a future contract
    /// version.
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
                // ALL nesting is forbidden — cache↔cache, cache↔state,
                // state↔state alike (not only across sections).
                if path_is_nested_inside(path_a, path_b) || path_is_nested_inside(path_b, path_a) {
                    return Err(format!(
                        "[{section_a}.{name_a}] ({path_a:?}) and [{section_b}.{name_b}] ({path_b:?}) nest"
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
///
/// **§7 envelope asymmetry is intentional (division of labor).** This Rust
/// parser reads the *whole capsule.toml* and, by construction
/// ([`CaptureDeclarations`] carries only `cache`/`state`), ignores every other
/// top-level manifest table. The api's `captureDeclarationsSchema` is
/// `.strict()` and rejects unknown top-level keys — but it never sees the whole
/// manifest: the JSON projection it validates contains ONLY the `cache`/`state`
/// keys, produced by extracting those two tables from the manifest, never by
/// serializing the whole manifest. So both sides agree on the declaration set
/// even though one tolerates a full manifest and the other forbids extra
/// top-level keys.
pub fn parse_capture_declarations(toml_text: &str) -> Result<DeclaredPaths, String> {
    let decls: CaptureDeclarations =
        toml::from_str(toml_text).map_err(|e| format!("capsule.toml capture declarations: {e}"))?;
    decls.validate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn sorted_keys(v: &Value) -> Vec<String> {
        let mut keys: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        keys.sort();
        keys
    }

    // ── spec-example request bodies (§3), used as strict-mode baselines ─────

    fn renew_json() -> Value {
        json!({
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ"
        })
    }

    fn control_query_json() -> Value {
        json!({
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "observed_capture_epoch": 2
        })
    }

    fn progress_json() -> Value {
        json!({
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "stage": "deps"
        })
    }

    fn hold_ready_json() -> Value {
        json!({
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "builder_id": "builder-sugamo-1",
            "slot_id": "slot-3",
            "session_id": "sess_01J1Y9",
            "guest_port": 8000
        })
    }

    fn candidate_report_json() -> Value {
        json!({
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "capture_epoch": 3,
            "candidate_id": "cand_01J1Z0",
            "execution_id": "exec_01J1Z1",
            "snapshot_id": "snap_01J1Z2",
            "artifact_location": "r2://snapshots/cand_01J1Z0/seal",
            "source_lost": false
        })
    }

    fn acceptance_json() -> Value {
        json!({
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "capture_epoch": 3,
            "status": "accepted",
            "acceptance_receipt": {
                "receipt_schema": "ato.snapshot-acceptance/v1",
                "receipt": { "any": "opaque payload" }
            }
        })
    }

    fn terminal_ack_json() -> Value {
        // The §3.8 spec example: the absent optionals (failure_stage /
        // failure_reason) are OMITTED — explicit null is a schema reject on
        // both sides (§3 null policy, tested below).
        json!({
            "agent_id": "builder-sugamo-1",
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "reason": "attempt_ended"
        })
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
    fn directive_candidate_verify_acceptance_and_reason_enums_use_exact_wire_strings() {
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
        // NEW (§2): acceptance status.
        for (v, wire) in [
            (AcceptanceStatus::Accepted, "accepted"),
            (AcceptanceStatus::Rejected, "rejected"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(wire));
        }
        // NEW (§2): the four wizard terminal-ack reasons.
        for (v, wire) in [
            (TerminalAckReason::Discarded, "discarded"),
            (TerminalAckReason::BuildFailed, "build_failed"),
            (
                TerminalAckReason::AcceptanceFailedSourceLost,
                "acceptance_failed_source_lost",
            ),
            (TerminalAckReason::AttemptEnded, "attempt_ended"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(wire));
        }
        // Neither "sealed" nor "lease_expired" is a member of the wizard
        // terminal-ack reason enum. Lease expiry is SERVER-OWNED: the sweep
        // moves the attempt to `expired` and the builder tears down locally on
        // `409 fenced`, never sending a terminal ack (an expired-lease ack
        // would itself be fenced).
        assert!(serde_json::from_value::<TerminalAckReason>(json!("sealed")).is_err());
        assert!(serde_json::from_value::<TerminalAckReason>(json!("lease_expired")).is_err());
        assert_eq!(JOB_KIND_INTERACTIVE_CAPTURE, "interactive_capture");
        assert_eq!(JOB_STATUS_HOLDING, "holding");
    }

    // ── §1 fencing transport ────────────────────────────────────────────────

    #[test]
    fn lease_token_header_has_the_canonical_spec_spelling() {
        // §1.1/D2: the token rides X-Ato-Lease-Token on ALL endpoints, GET and
        // POST (case-insensitive per HTTP).
        assert_eq!(LEASE_TOKEN_HEADER, "X-Ato-Lease-Token");
        assert!(LEASE_TOKEN_HEADER.eq_ignore_ascii_case("x-ato-lease-token"));
    }

    #[test]
    fn strict_bodies_reject_lease_token_and_job_id_keys() {
        // §1.1/D2 mandatory test: `lease_token` travels ONLY in the
        // X-Ato-Lease-Token header (its one JSON appearance is the claim
        // response that mints it), and `job_id` is path-only. A body (or the
        // control query) carrying either key fails strict-mode parsing on
        // EVERY builder message.
        fn with_key(mut v: Value, key: &str) -> Value {
            v[key] = json!("smuggled");
            v
        }
        for key in ["lease_token", "job_id"] {
            assert!(
                serde_json::from_value::<LeaseRenewRequest>(with_key(renew_json(), key)).is_err(),
                "renew body must reject {key}"
            );
            assert!(
                serde_json::from_value::<ControlQuery>(with_key(control_query_json(), key))
                    .is_err(),
                "control query must reject {key}"
            );
            assert!(
                serde_json::from_value::<ProgressRequest>(with_key(progress_json(), key)).is_err(),
                "progress body must reject {key}"
            );
            assert!(
                serde_json::from_value::<HoldReadyRequest>(with_key(hold_ready_json(), key))
                    .is_err(),
                "hold-ready body must reject {key}"
            );
            assert!(
                serde_json::from_value::<CandidateReportRequest>(with_key(
                    candidate_report_json(),
                    key
                ))
                .is_err(),
                "candidate report body must reject {key}"
            );
            assert!(
                serde_json::from_value::<CandidateAcceptanceRequest>(with_key(
                    acceptance_json(),
                    key
                ))
                .is_err(),
                "acceptance body must reject {key}"
            );
            assert!(
                serde_json::from_value::<WizardTerminalAck>(with_key(terminal_ack_json(), key))
                    .is_err(),
                "terminal ack body must reject {key}"
            );
        }
    }

    #[test]
    fn optional_fields_reject_explicit_null() {
        // §3 null policy mandatory test (mirrors the strict-body lease_token
        // one): absence is encoded by OMITTING the key. The api's zod
        // `.optional()` admits only an absent key, never null — a payload
        // carrying an explicit null for ANY optional field must fail parsing
        // here too, or the same payload would get two verdicts on the seam.

        // Terminal ack: failure_stage / failure_reason (the pre-fix §3.8
        // example carried exactly these nulls).
        for key in ["failure_stage", "failure_reason"] {
            let mut v = terminal_ack_json();
            v[key] = Value::Null;
            assert!(
                serde_json::from_value::<WizardTerminalAck>(v).is_err(),
                "terminal ack must reject explicit null {key}"
            );
        }

        // Candidate acceptance: acceptance_receipt / failure_reason.
        let mut accepted_null_receipt = acceptance_json();
        accepted_null_receipt["acceptance_receipt"] = Value::Null;
        assert!(
            serde_json::from_value::<CandidateAcceptanceRequest>(accepted_null_receipt).is_err(),
            "acceptance body must reject explicit null acceptance_receipt"
        );
        assert!(
            serde_json::from_value::<CandidateAcceptanceRequest>(json!({
                "submission_attempt_id": "subatt_01J1XY",
                "worker_claim_id": "claim_01J1XZ",
                "capture_epoch": 3,
                "status": "rejected",
                "failure_reason": null
            }))
            .is_err(),
            "acceptance body must reject explicit null failure_reason"
        );

        // Control response: candidate_id / hold_expires_at.
        assert!(
            serde_json::from_value::<ControlResponse>(json!({
                "directive": "capture",
                "server_capture_epoch": 3,
                "candidate_id": "cand_01J1Z0",
                "hold_expires_at": null,
                "pause_permitted": true
            }))
            .is_err(),
            "control response must reject explicit null hold_expires_at"
        );
        assert!(
            serde_json::from_value::<ControlResponse>(json!({
                "directive": "discard",
                "server_capture_epoch": 3,
                "candidate_id": null,
                "pause_permitted": false
            }))
            .is_err(),
            "control response must reject explicit null candidate_id"
        );

        // Omission stays green: the same shapes parse (and validate) with the
        // optional keys absent.
        let rejected_omitted: CandidateAcceptanceRequest = serde_json::from_value(json!({
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "capture_epoch": 3,
            "status": "rejected"
        }))
        .unwrap();
        rejected_omitted.validate().unwrap();
        let discard_omitted: ControlResponse = serde_json::from_value(json!({
            "directive": "discard",
            "server_capture_epoch": 3,
            "pause_permitted": false
        }))
        .unwrap();
        discard_omitted.validate().unwrap();
    }

    #[test]
    fn fenced_error_envelope_matches_the_spec_shape() {
        let e: ErrorEnvelope =
            serde_json::from_value(json!({ "error": "fenced", "message": "stale epoch" })).unwrap();
        assert_eq!(e.error, ERROR_CODE_FENCED);
    }

    // ── §1.3 mandatory epoch contract tests (B1) ────────────────────────────

    /// §1.3 (a) — SERVER RULE (§1.2 control poll): ACCEPT
    /// `observed <= server_epoch`. Server epoch advanced 0→1 while the builder
    /// still observes 0: the poll is served (not fenced), and the response
    /// schema MUST allow `server_capture_epoch` to differ from the request's
    /// `observed_capture_epoch` — that is how a stale observer catches up.
    #[test]
    fn epoch_test_a_stale_observer_is_served_the_new_capture_epoch() {
        let query: ControlQuery = serde_json::from_value(json!({
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "observed_capture_epoch": 0
        }))
        .unwrap();
        control_poll_epoch_rule(query.observed_capture_epoch, 1).unwrap();

        let resp: ControlResponse = serde_json::from_value(json!({
            "directive": "capture",
            "server_capture_epoch": 1,
            "candidate_id": "cand_01J1Z0",
            "pause_permitted": true
        }))
        .unwrap();
        resp.validate().unwrap();
        assert_ne!(resp.server_capture_epoch, query.observed_capture_epoch);
    }

    /// §1.3 (b) — SERVER RULE (§1.2 control poll): REJECT `409 fenced` only
    /// when `observed > server_epoch` — a builder cannot have observed the
    /// future (treat as corrupt/forged state).
    #[test]
    fn epoch_test_b_observer_ahead_of_the_server_is_fenced() {
        assert!(control_poll_epoch_rule(2, 1).is_err());
        // The boundary stays accepted: observing exactly the server epoch is
        // fine.
        control_poll_epoch_rule(1, 1).unwrap();
    }

    /// §1.3 (c) — SERVER RULE (§1.2 report/acceptance): a report with
    /// `capture_epoch: 0` against a candidate whose epoch is 1 is fenced
    /// (exact-match rule); additionally 0 is below the ≥ 1 schema floor on
    /// both the report and the acceptance body.
    #[test]
    fn epoch_test_c_report_epoch_zero_against_candidate_epoch_one_is_rejected() {
        assert!(candidate_epoch_rule(0, 1).is_err());
        let mut report: CandidateReportRequest =
            serde_json::from_value(candidate_report_json()).unwrap();
        report.capture_epoch = 0;
        assert!(report.validate().is_err());
        let mut acceptance: CandidateAcceptanceRequest =
            serde_json::from_value(acceptance_json()).unwrap();
        acceptance.capture_epoch = 0;
        assert!(acceptance.validate().is_err());
    }

    /// §1.3 (d) — SERVER RULE (§1.2 report/acceptance): a report with
    /// `capture_epoch: 1` against a candidate whose epoch is 1 is accepted
    /// (exact match).
    #[test]
    fn epoch_test_d_exact_epoch_match_is_accepted() {
        candidate_epoch_rule(1, 1).unwrap();
        let mut report: CandidateReportRequest =
            serde_json::from_value(candidate_report_json()).unwrap();
        report.capture_epoch = 1;
        report.validate().unwrap();
        // ...and a superseded (non-matching) epoch stays fenced.
        assert!(candidate_epoch_rule(1, 2).is_err());
    }

    // ── wire contract version ───────────────────────────────────────────────

    #[test]
    fn wire_contract_version_constant_is_pinned() {
        assert_eq!(WIRE_CONTRACT_VERSION, "ato.submission-wizard-wire/v1");
        assert_eq!(
            serde_json::to_value(WireContractVersion).unwrap(),
            json!("ato.submission-wizard-wire/v1")
        );
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
            "wire_contract_version": "ato.submission-wizard-wire/v1",
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "lease_token": "b64u-opaque-token",
            "lease_expires_at": "2026-07-22T09:15:00.000Z"
        })
    }

    #[test]
    fn claim_extension_parses_the_spec_example() {
        let ext: InteractiveCaptureClaimExt = serde_json::from_value(claim_job_example()).unwrap();
        assert_eq!(ext.wire_contract_version, WireContractVersion);
        assert_eq!(ext.submission_attempt_id, "subatt_01J1XY");
        assert_eq!(ext.worker_claim_id, "claim_01J1XZ");
        assert_eq!(ext.lease_token.expose(), "b64u-opaque-token");
        assert_eq!(ext.lease_expires_at, "2026-07-22T09:15:00.000Z");
        // Serialization carries exactly the five §3.1 extension fields, with
        // the version literal.
        let v = serde_json::to_value(&ext).unwrap();
        assert_eq!(
            sorted_keys(&v),
            vec![
                "lease_expires_at",
                "lease_token",
                "submission_attempt_id",
                "wire_contract_version",
                "worker_claim_id",
            ]
        );
        assert_eq!(v["wire_contract_version"], json!(WIRE_CONTRACT_VERSION));
    }

    #[test]
    fn lease_token_never_renders_through_debug() {
        // D2: the token must never reach a log line. The realistic leak is a
        // `{:?}` of a struct that happens to carry it — so the type itself
        // redacts, and every struct that embeds it inherits that.
        let ext: InteractiveCaptureClaimExt = serde_json::from_value(claim_job_example()).unwrap();
        assert!(!format!("{ext:?}").contains("b64u-opaque-token"));
        assert!(format!("{:?}", ext.lease_token).contains("<redacted>"));

        let fencing = Fencing4 {
            job_id: "job_x".to_string(),
            submission_attempt_id: ext.submission_attempt_id.clone(),
            worker_claim_id: ext.worker_claim_id.clone(),
            lease_token: ext.lease_token.clone(),
        };
        assert!(!format!("{fencing:?}").contains("b64u-opaque-token"));
        // …while the WIRE encoding is unchanged: the claim response (§3.1) is
        // still the one message carrying the bare token string.
        assert_eq!(
            serde_json::to_value(&ext).unwrap()["lease_token"],
            json!("b64u-opaque-token")
        );
    }

    #[test]
    fn wire_contract_version_mismatch_fails_closed_at_parse() {
        // §3.1: `wire_contract_version` is a REQUIRED literal — any other
        // value (or its absence) is a schema reject BEFORE any semantics run.
        let mut wrong_version = claim_job_example();
        wrong_version["wire_contract_version"] = json!("ato.submission-wizard-wire/v2");
        assert!(serde_json::from_value::<InteractiveCaptureClaimExt>(wrong_version).is_err());

        let mut missing = claim_job_example();
        missing
            .as_object_mut()
            .unwrap()
            .remove("wire_contract_version");
        assert!(serde_json::from_value::<InteractiveCaptureClaimExt>(missing).is_err());
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
    fn lease_renew_body_is_exactly_the_two_fencing_body_fields() {
        // FENCING-4 transport on renew: job_id in the path, token in the
        // header, and a strict body of exactly these two fields — the epoch
        // plays no role on renew (§1.2).
        let req: LeaseRenewRequest = serde_json::from_value(renew_json()).unwrap();
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(
            sorted_keys(&v),
            vec!["submission_attempt_id", "worker_claim_id"]
        );
        // capture_epoch is not a renew field at all (B1).
        let mut with_epoch = renew_json();
        with_epoch["capture_epoch"] = json!(1);
        assert!(serde_json::from_value::<LeaseRenewRequest>(with_epoch).is_err());

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
            "server_capture_epoch": 3,
            "candidate_id": "cand_01J1Z0",
            "hold_expires_at": "2026-07-22T09:45:00.000Z",
            "pause_permitted": true
        }))
        .unwrap();
        assert_eq!(resp.directive, ControlDirective::Capture);
        assert_eq!(resp.server_capture_epoch, 3);
        assert_eq!(resp.candidate_id.as_deref(), Some("cand_01J1Z0"));
        assert!(resp.pause_permitted);
        resp.validate().unwrap();
    }

    #[test]
    fn control_response_refinements_fail_closed() {
        // capture without a candidate id
        let no_candidate = ControlResponse {
            directive: ControlDirective::Capture,
            server_capture_epoch: 3,
            candidate_id: None,
            hold_expires_at: None,
            pause_permitted: true,
        };
        assert!(no_candidate.validate().is_err());
        // capture at epoch 0 (epoch 0 ⇔ "no capture ever requested")
        let epoch_zero = ControlResponse {
            server_capture_epoch: 0,
            candidate_id: Some("cand_x".into()),
            ..no_candidate.clone()
        };
        assert!(epoch_zero.validate().is_err());
        // hold without a hold deadline
        let hold_no_deadline = ControlResponse {
            directive: ControlDirective::Hold,
            server_capture_epoch: 0,
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
            server_capture_epoch: 3,
            candidate_id: None,
            hold_expires_at: None,
            pause_permitted: false,
        };
        discard.validate().unwrap();
    }

    #[test]
    fn control_query_carries_the_three_query_params() {
        let q: ControlQuery = serde_json::from_value(control_query_json()).unwrap();
        assert_eq!(q.observed_capture_epoch, 2);
        assert_eq!(
            sorted_keys(&serde_json::to_value(&q).unwrap()),
            vec![
                "observed_capture_epoch",
                "submission_attempt_id",
                "worker_claim_id"
            ]
        );
    }

    // ── §3.4 progress ───────────────────────────────────────────────────────

    #[test]
    fn progress_body_carries_the_fencing_body_fields_plus_stage() {
        let req: ProgressRequest = serde_json::from_value(progress_json()).unwrap();
        assert_eq!(req.stage, WizardStage::Deps);
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(
            sorted_keys(&v),
            vec!["stage", "submission_attempt_id", "worker_claim_id"]
        );
        let back: ProgressRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back, req);
    }

    // ── §3.5 hold-ready ─────────────────────────────────────────────────────

    fn hold_ready() -> HoldReadyRequest {
        serde_json::from_value(hold_ready_json()).unwrap()
    }

    #[test]
    fn hold_ready_carries_identity_tuple_and_no_upstream_url() {
        let v = serde_json::to_value(hold_ready()).unwrap();
        // ADR-004: the exact key set — no self-reported upstream URL/host/
        // address field exists to smuggle an SSRF target through, no token,
        // no job_id, no epoch.
        assert_eq!(
            sorted_keys(&v),
            vec![
                "builder_id",
                "guest_port",
                "session_id",
                "slot_id",
                "submission_attempt_id",
                "worker_claim_id",
            ]
        );
        hold_ready().validate().unwrap();
        // The strict body rejects a self-reported upstream outright.
        let mut with_upstream = hold_ready_json();
        with_upstream["upstream_url"] = json!("http://attacker.example");
        assert!(serde_json::from_value::<HoldReadyRequest>(with_upstream).is_err());
    }

    #[test]
    fn hold_ready_validation_bounds() {
        let mut bad_port = hold_ready();
        bad_port.guest_port = 0;
        assert!(bad_port.validate().is_err());
        // > 65535 is unrepresentable: u16 rejects it at parse.
        let mut v = hold_ready_json();
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
        let v = candidate_report_json();
        let req: CandidateReportRequest = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(req.capture_epoch, 3);
        assert_eq!(req.candidate_id, "cand_01J1Z0");
        assert!(!req.source_lost);
        req.validate().unwrap();
        // Round-trip preserves the exact key set (fencing body fields + epoch
        // + five payload fields).
        assert_eq!(
            sorted_keys(&serde_json::to_value(&req).unwrap()),
            sorted_keys(&v)
        );

        // Epoch 0 can never name a capture command (schema floor is >= 1).
        let mut epoch_zero = req.clone();
        epoch_zero.capture_epoch = 0;
        assert!(epoch_zero.validate().is_err());

        let resp: CandidateReportResponse =
            serde_json::from_value(json!({ "candidate_id": "cand_01J1Z0", "status": "reported" }))
                .unwrap();
        assert_eq!(resp.status, CandidateStatus::Reported);
    }

    // ── §3.7 candidate acceptance (B2 — not a job-terminal ack) ─────────────

    #[test]
    fn candidate_acceptance_matches_the_spec_example() {
        let v = acceptance_json();
        let req: CandidateAcceptanceRequest = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(req.capture_epoch, 3);
        assert_eq!(req.status, AcceptanceStatus::Accepted);
        req.validate().unwrap();
        // Round-trip preserves the exact key set.
        assert_eq!(
            sorted_keys(&serde_json::to_value(&req).unwrap()),
            sorted_keys(&v)
        );

        let resp: CandidateAcceptanceResponse =
            serde_json::from_value(json!({ "candidate_id": "cand_01J1Z0", "status": "accepted" }))
                .unwrap();
        assert_eq!(resp.status, AcceptanceStatus::Accepted);
    }

    #[test]
    fn acceptance_receipt_is_a_versioned_envelope_not_a_pinned_field_list() {
        // D3: BOTH sides validate ONLY the envelope — the literal plus "is an
        // object". No payload key is pinned (the 9-key pre-Gate-0 core is
        // superseded; its pinning tests are removed): an EMPTY payload object
        // is green.
        let empty_payload: AcceptanceReceipt = serde_json::from_value(json!({
            "receipt_schema": "ato.snapshot-acceptance/v1",
            "receipt": {}
        }))
        .unwrap();
        assert!(empty_payload.receipt.is_empty());
        assert_eq!(
            serde_json::to_value(&empty_payload).unwrap()["receipt_schema"],
            json!(ACCEPTANCE_RECEIPT_SCHEMA)
        );

        // Wrong version literal → schema reject (fail-closed).
        assert!(
            serde_json::from_value::<AcceptanceReceipt>(json!({
                "receipt_schema": "ato.snapshot-acceptance/v2",
                "receipt": {}
            }))
            .is_err()
        );
        // The payload must be an OBJECT — the envelope's one structural rule.
        for not_an_object in [json!("evidence"), json!([1, 2]), json!(7), json!(null)] {
            assert!(
                serde_json::from_value::<AcceptanceReceipt>(json!({
                    "receipt_schema": "ato.snapshot-acceptance/v1",
                    "receipt": not_an_object
                }))
                .is_err()
            );
        }
        // The envelope itself is required alongside `status: "accepted"`.
        let mut missing_envelope = acceptance_json();
        missing_envelope
            .as_object_mut()
            .unwrap()
            .remove("acceptance_receipt");
        let req: CandidateAcceptanceRequest = serde_json::from_value(missing_envelope).unwrap();
        assert!(req.validate().is_err());
    }

    #[test]
    fn acceptance_receipt_envelope_rejects_unknown_keys() {
        // The envelope is STRICT on both sides (api `.strict()`, tested there
        // as "unknown envelope keys"; `deny_unknown_fields` here): payload
        // keys live INSIDE `receipt`, never beside it. `deny_unknown_fields`
        // on the outer CandidateAcceptanceRequest does NOT propagate to
        // nested structs, so the envelope pins its own strictness — without
        // it {receipt_schema, receipt, execution_id} parses GREEN here and
        // RED on the api, a strict-body divergence inside a builder→api
        // request body.
        assert!(
            serde_json::from_value::<AcceptanceReceipt>(json!({
                "receipt_schema": "ato.snapshot-acceptance/v1",
                "receipt": { "execution_id": "exec_01J1Z1" },
                "execution_id": "exec_01J1Z1"
            }))
            .is_err(),
            "envelope must reject an unknown key beside receipt_schema/receipt"
        );
        // ...and the same divergence through the full acceptance body.
        let mut body = acceptance_json();
        body["acceptance_receipt"]["execution_id"] = json!("exec_01J1Z1");
        assert!(
            serde_json::from_value::<CandidateAcceptanceRequest>(body).is_err(),
            "acceptance body must reject an unknown envelope key"
        );
    }

    #[test]
    fn acceptance_refinements_split_accepted_and_rejected() {
        // Rejected: receipt must be ABSENT; failure_reason is optional.
        let rejected: CandidateAcceptanceRequest = serde_json::from_value(json!({
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "capture_epoch": 3,
            "status": "rejected",
            "failure_reason": "acceptance run failed readiness"
        }))
        .unwrap();
        rejected.validate().unwrap();

        let mut rejected_with_receipt = rejected.clone();
        rejected_with_receipt.acceptance_receipt = Some(AcceptanceReceipt {
            receipt_schema: AcceptanceReceiptSchema,
            receipt: serde_json::Map::new(),
        });
        assert!(rejected_with_receipt.validate().is_err());

        // Accepted: failure_reason is only legal with "rejected".
        let mut accepted: CandidateAcceptanceRequest =
            serde_json::from_value(acceptance_json()).unwrap();
        accepted.failure_reason = Some("but also failed?".into());
        assert!(accepted.validate().is_err());
    }

    // ── §3.8 wizard terminal ack (restricted) ───────────────────────────────

    #[test]
    fn wizard_terminal_ack_round_trips_the_spec_example() {
        let ack: WizardTerminalAck = serde_json::from_value(terminal_ack_json()).unwrap();
        assert_eq!(ack.agent_id, "builder-sugamo-1");
        assert_eq!(ack.reason, TerminalAckReason::AttemptEnded);
        assert_eq!(ack.failure_stage, None);
        assert_eq!(ack.failure_reason, None);
        ack.validate().unwrap();
        // Absent optionals are OMITTED on the wire, matching the spec example
        // (§3 null policy: this side never emits nulls, and an explicit null
        // is a parse reject — see optional_fields_reject_explicit_null).
        assert_eq!(
            sorted_keys(&serde_json::to_value(&ack).unwrap()),
            vec![
                "agent_id",
                "reason",
                "submission_attempt_id",
                "worker_claim_id",
            ]
        );
    }

    #[test]
    fn wizard_terminal_ack_rejects_the_legacy_sealed_ack_shape() {
        // §2/§3.8 (B2): `status: "sealed"` is NOT used by interactive_capture
        // jobs, and the terminal ack carries no accepted_candidate_id, no
        // receipt, and no epoch — candidate acceptance is §3.7's endpoint and
        // is not job-terminal. The strict body enforces the absence of every
        // legacy sealed-ack key.
        for (key, value) in [
            ("status", json!("sealed")),
            ("accepted_candidate_id", json!("cand_01J1Z0")),
            (
                "acceptance_receipt",
                json!({ "receipt_schema": ACCEPTANCE_RECEIPT_SCHEMA, "receipt": {} }),
            ),
            ("capture_epoch", json!(3)),
        ] {
            let mut v = terminal_ack_json();
            v[key] = value;
            assert!(
                serde_json::from_value::<WizardTerminalAck>(v).is_err(),
                "terminal ack must reject legacy key {key}"
            );
        }
    }

    #[test]
    fn wizard_terminal_ack_rejects_lease_expired_reason() {
        // §2/§3.8: lease expiry is SERVER-OWNED — the API sweep moves the
        // attempt to `expired` and the builder tears down locally on
        // `409 fenced`, without sending a terminal ack (an expired-lease ack
        // would itself be fenced). `"lease_expired"` is therefore absent from
        // the reason enum, so a terminal-ack body carrying it fails at parse.
        let mut v = terminal_ack_json();
        v["reason"] = json!("lease_expired");
        assert!(serde_json::from_value::<WizardTerminalAck>(v).is_err());
    }

    #[test]
    fn wizard_terminal_ack_failure_diagnostics_are_optional_refinements() {
        // failure_stage discriminates capture_seal vs acceptance as a
        // DIAGNOSTIC refinement of `reason` — optional, never required.
        let mut ack: WizardTerminalAck = serde_json::from_value(json!({
            "agent_id": "builder-sugamo-1",
            "submission_attempt_id": "subatt_01J1XY",
            "worker_claim_id": "claim_01J1XZ",
            "reason": "acceptance_failed_source_lost",
            "failure_stage": "acceptance",
            "failure_reason": "acceptance run failed after source loss"
        }))
        .unwrap();
        ack.validate().unwrap();
        assert_eq!(ack.failure_stage, Some(WizardFailureStage::Acceptance));

        ack.failure_stage = Some(WizardFailureStage::CaptureSeal);
        assert_eq!(
            serde_json::to_value(&ack).unwrap()["failure_stage"],
            json!("capture_seal")
        );
        // A build failure without diagnostics is a legal terminal ack.
        ack.reason = TerminalAckReason::BuildFailed;
        ack.failure_stage = None;
        ack.failure_reason = None;
        ack.validate().unwrap();
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

    #[test]
    fn quiesce_messages_reject_unknown_keys_per_shape() {
        // The api's three §5 schemas are each `.strict()`; the Rust mirror must
        // reject unknown keys per shape too (one verdict on both sides).
        // Previously the internally-tagged enum silently ignored them.

        // `unquiesce` carries no `inflight` — the api rejects this exact shape.
        assert!(
            serde_json::from_value::<QuiesceMessage>(json!({
                "type": "unquiesce", "epoch": 3, "inflight": 0
            }))
            .is_err()
        );
        // A stray key beside an otherwise-valid `quiesce` is rejected.
        assert!(
            serde_json::from_value::<QuiesceMessage>(json!({
                "type": "quiesce", "epoch": 3, "extra": "x"
            }))
            .is_err()
        );
        // `quiesced` likewise rejects an extra key.
        assert!(
            serde_json::from_value::<QuiesceMessage>(json!({
                "type": "quiesced", "epoch": 3, "inflight": 0, "extra": 1
            }))
            .is_err()
        );
        // An unknown discriminant is still rejected.
        assert!(
            serde_json::from_value::<QuiesceMessage>(json!({ "type": "pause", "epoch": 3 }))
                .is_err()
        );
        // Sanity: the three canonical shapes still parse round-trip.
        assert_eq!(
            serde_json::from_value::<QuiesceMessage>(json!({ "type": "quiesce", "epoch": 3 }))
                .unwrap(),
            QuiesceMessage::Quiesce { epoch: 3 }
        );
        assert_eq!(
            serde_json::from_value::<QuiesceMessage>(
                json!({ "type": "quiesced", "epoch": 3, "inflight": 0 })
            )
            .unwrap(),
            QuiesceMessage::Quiesced {
                epoch: 3,
                inflight: 0
            }
        );
        assert_eq!(
            serde_json::from_value::<QuiesceMessage>(json!({ "type": "unquiesce", "epoch": 3 }))
                .unwrap(),
            QuiesceMessage::Unquiesce { epoch: 3 }
        );
    }

    // ── §7 capsule.toml declarations (D1: guest-absolute paths) ─────────────

    /// The §7 valid example, verbatim from the spec (= design doc §1.2).
    const VALID_DECLS: &str = r#"
[cache.model]
path = "/var/cache/example-model"
capture = "include"

[cache.pip]
path = "/root/.venv"
capture = "exclude"

[state.data]
path = "/data"
snapshot = "exclude"
schema = "1"

[state.db]
path = "/var/lib/app/data/app.db"
snapshot = "exclude"
schema = "sqlite"
"#;

    #[test]
    fn valid_spec_example_produces_the_declared_path_set() {
        let declared = parse_capture_declarations(VALID_DECLS).unwrap();
        assert_eq!(
            declared.paths,
            BTreeSet::from([
                "/var/cache/example-model".to_string(),
                "/root/.venv".to_string(),
                "/data".to_string(),
                "/var/lib/app/data/app.db".to_string(),
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
                "[cache.pip]\npath = \"/root/.venv\"\ncapture = \"include\"\nextra = true\n",
            )
            .is_err()
        );
        assert!(
            parse_capture_declarations(
                "[state.db]\npath = \"/data/app.db\"\nsnapshot = \"exclude\"\nschema = \"sqlite\"\nextra = true\n",
            )
            .is_err()
        );
    }

    #[test]
    fn relative_path_is_rejected() {
        // D1: declared surfaces are guest-absolute; the pre-review draft had
        // this backwards.
        let err =
            parse_capture_declarations("[cache.rel]\npath = \".venv\"\ncapture = \"include\"\n")
                .unwrap_err();
        assert!(err.contains("guest-absolute"), "{err}");
        assert!(
            parse_capture_declarations("[cache.rel]\npath = \"models\"\ncapture = \"include\"\n")
                .is_err()
        );
    }

    #[test]
    fn leading_double_slash_is_rejected() {
        let err = parse_capture_declarations(
            "[cache.doubleslash]\npath = \"//var/cache\"\ncapture = \"include\"\n",
        )
        .unwrap_err();
        assert!(err.contains("SINGLE '/'"), "{err}");
    }

    #[test]
    fn scheme_prefix_is_rejected() {
        // "file:///models" carries a ':' before the first '/' (and would also
        // fail the leading-'/' rule).
        assert!(
            parse_capture_declarations(
                "[cache.scheme]\npath = \"file:///models\"\ncapture = \"include\"\n",
            )
            .is_err()
        );
        let err =
            parse_capture_declarations("[cache.r2]\npath = \"r2://x\"\ncapture = \"include\"\n")
                .unwrap_err();
        assert!(err.contains("scheme"), "{err}");
        // A ':' AFTER the first '/' is just a path character, not a scheme.
        parse_capture_declarations("[cache.colon]\npath = \"/data:v1\"\ncapture = \"include\"\n")
            .unwrap();
    }

    #[test]
    fn dot_and_dotdot_segments_are_rejected() {
        let err = parse_capture_declarations(
            "[cache.up]\npath = \"/var/../etc\"\ncapture = \"include\"\n",
        )
        .unwrap_err();
        assert!(err.contains("'.' or '..'"), "{err}");
        assert!(
            parse_capture_declarations(
                "[cache.dot]\npath = \"/var/./cache\"\ncapture = \"include\"\n",
            )
            .is_err()
        );
    }

    #[test]
    fn trailing_slash_and_empty_segment_paths_are_rejected() {
        // No normalization pass exists on either side: every input that would
        // need normalizing is rejected, so the collision/nesting verdicts run
        // on the absolute paths exactly as declared (one verdict on both
        // sides).
        let err =
            parse_capture_declarations("[cache.a]\npath = \"/data/\"\ncapture = \"include\"\n")
                .unwrap_err();
        assert!(err.contains("empty segment"), "{err}");
        assert!(
            parse_capture_declarations("[cache.a]\npath = \"/data//x\"\ncapture = \"include\"\n")
                .is_err()
        );
        // The seam finding's repro, now on absolute paths: { cache "/data/",
        // state "/data/app.db" } must be INVALID here for the same reason it
        // is invalid on the api.
        assert!(
            parse_capture_declarations(
                "[cache.a]\npath = \"/data/\"\ncapture = \"include\"\n\n[state.b]\npath = \"/data/app.db\"\nsnapshot = \"exclude\"\nschema = \"sqlite\"\n",
            )
            .is_err()
        );
    }

    #[test]
    fn bare_root_path_is_rejected() {
        let err = parse_capture_declarations("[cache.root]\npath = \"/\"\ncapture = \"include\"\n")
            .unwrap_err();
        assert!(err.contains("bare '/'"), "{err}");
    }

    #[test]
    fn backslash_path_is_rejected() {
        let err = parse_capture_declarations(
            "[cache.backslash]\npath = \"/var\\\\cache\"\ncapture = \"include\"\n",
        )
        .unwrap_err();
        assert!(err.contains("backslash"), "{err}");
    }

    #[test]
    fn capture_value_outside_include_exclude_fails_at_parse() {
        assert!(
            parse_capture_declarations(
                "[cache.maybe]\npath = \"/var/cache\"\ncapture = \"sometimes\"\n",
            )
            .is_err()
        );
    }

    #[test]
    fn state_is_never_snapshot_included() {
        assert!(
            parse_capture_declarations(
                "[state.data2]\npath = \"/data2\"\nsnapshot = \"include\"\nschema = \"sqlite\"\n",
            )
            .is_err()
        );
    }

    #[test]
    fn state_requires_a_schema() {
        assert!(
            parse_capture_declarations(
                "[state.nodecl]\npath = \"/srv/data\"\nsnapshot = \"exclude\"\n"
            )
            .is_err()
        );
    }

    #[test]
    fn identical_paths_across_sections_are_rejected() {
        // The spec's `[cache.dup]` case: "/data" is already [state.data]'s
        // path in the valid example.
        let toml_text =
            format!("{VALID_DECLS}\n[cache.dup]\npath = \"/data\"\ncapture = \"include\"\n");
        let err = parse_capture_declarations(&toml_text).unwrap_err();
        assert!(err.contains("identical path"), "{err}");
    }

    #[test]
    fn nesting_between_any_two_declarations_is_rejected() {
        // The spec's `[cache.nest]` case: a cache path nested under the state
        // path "/data".
        let err = parse_capture_declarations(
            "[state.data]\npath = \"/data\"\nsnapshot = \"exclude\"\nschema = \"1\"\n\n[cache.nest]\npath = \"/data/cache\"\ncapture = \"include\"\n",
        )
        .unwrap_err();
        assert!(err.contains("nest"), "{err}");
        // cache path containing a state path
        assert!(
            parse_capture_declarations(
                "[cache.data]\npath = \"/data\"\ncapture = \"include\"\n\n[state.db]\npath = \"/data/app.db\"\nsnapshot = \"exclude\"\nschema = \"sqlite\"\n",
            )
            .is_err()
        );
        // ALL nesting is forbidden now, not only across sections: two cache
        // declarations in an ancestor/descendant relation are rejected too
        // (round-2 fix — this case used to be accepted).
        assert!(
            parse_capture_declarations(
                "[cache.outer]\npath = \"/vendor\"\ncapture = \"include\"\n\n[cache.inner]\npath = \"/vendor/bin\"\ncapture = \"exclude\"\n",
            )
            .is_err()
        );
        // ...and two state declarations likewise (spec §7 invalid example
        // /var/cache + /var/cache/session).
        assert!(
            parse_capture_declarations(
                "[state.outer]\npath = \"/var/cache\"\nsnapshot = \"exclude\"\nschema = \"1\"\n\n[state.inner]\npath = \"/var/cache/session\"\nsnapshot = \"exclude\"\nschema = \"1\"\n",
            )
            .is_err()
        );
    }

    #[test]
    fn declaration_names_are_unique_across_cache_and_state() {
        let err = parse_capture_declarations(
            "[cache.db]\npath = \"/var/cache\"\ncapture = \"include\"\n\n[state.db]\npath = \"/data/app.db\"\nsnapshot = \"exclude\"\nschema = \"sqlite\"\n",
        )
        .unwrap_err();
        assert!(err.contains("unique"), "{err}");
    }

    #[test]
    fn declaration_name_charset_and_length_are_enforced() {
        assert!(
            parse_capture_declarations("[cache.\"UPPER\"]\npath = \"/x\"\ncapture = \"include\"\n")
                .is_err()
        );
        let long_name = "a".repeat(41);
        assert!(
            parse_capture_declarations(&format!(
                "[cache.{long_name}]\npath = \"/x\"\ncapture = \"include\"\n"
            ))
            .is_err()
        );
    }

    #[test]
    fn state_schema_charset_and_length_are_enforced() {
        assert!(
            parse_capture_declarations(
                "[state.db]\npath = \"/d\"\nsnapshot = \"exclude\"\nschema = \"NOT VALID\"\n",
            )
            .is_err()
        );
        let long_schema = "s".repeat(61);
        assert!(
            parse_capture_declarations(&format!(
                "[state.db]\npath = \"/d\"\nsnapshot = \"exclude\"\nschema = \"{long_schema}\"\n"
            ))
            .is_err()
        );
        // spec example ids stay valid
        parse_capture_declarations(
            "[state.kv]\npath = \"/d\"\nsnapshot = \"exclude\"\nschema = \"kv-dir\"\n",
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
        // "/database" is NOT inside "/data" — no false positive on a shared
        // string prefix.
        parse_capture_declarations(
            "[cache.data]\npath = \"/data\"\ncapture = \"include\"\n\n[state.other]\npath = \"/database\"\nsnapshot = \"exclude\"\nschema = \"sqlite\"\n",
        )
        .unwrap();
    }

    // ── seam finding 1: length bounds are UTF-16 code units ─────────────────

    #[test]
    fn string_length_bounds_count_utf16_code_units_not_scalars() {
        // The api's zod `.min()/.max()` count UTF-16 code units (`String.length`);
        // this side must agree or a string legal on one side is illegal on the
        // other. An astral scalar is 1 `char` but 2 UTF-16 code units.

        // failure_reason: max 2000. 1200 astral emoji = 1200 scalars = 2400
        // UTF-16 code units → REJECTED (it would be ACCEPTED if this counted
        // chars().count() == 1200 <= 2000).
        let astral_reason = "\u{1F600}".repeat(1200);
        assert_eq!(astral_reason.chars().count(), 1200);
        assert_eq!(astral_reason.encode_utf16().count(), 2400);
        let over_bound = WizardTerminalAck {
            agent_id: "b".into(),
            submission_attempt_id: "subatt_01J1XY".into(),
            worker_claim_id: "claim_01J1XZ".into(),
            reason: TerminalAckReason::BuildFailed,
            failure_stage: Some(WizardFailureStage::CaptureSeal),
            failure_reason: Some(astral_reason.clone()),
        };
        assert!(over_bound.validate().is_err());
        // A just-at-bound BMP string (each scalar = 1 UTF-16 code unit) passes.
        let at_bound = WizardTerminalAck {
            failure_reason: Some("x".repeat(2000)),
            ..over_bound.clone()
        };
        assert!(at_bound.validate().is_ok());
        // The same shared bound applies to the acceptance failure_reason.
        let acceptance_over_bound = CandidateAcceptanceRequest {
            submission_attempt_id: "subatt_01J1XY".into(),
            worker_claim_id: "claim_01J1XZ".into(),
            capture_epoch: 3,
            status: AcceptanceStatus::Rejected,
            acceptance_receipt: None,
            failure_reason: Some(astral_reason),
        };
        assert!(acceptance_over_bound.validate().is_err());

        // Declared path: max 200, guest-absolute. A path may hold astral
        // scalars (charset is unrestricted apart from the segment rules), so
        // this bound is the most load-bearing. "/" + 150 astral scalars = 301
        // UTF-16 code units → REJECTED.
        let astral_path = format!("/{}", "\u{1F600}".repeat(150));
        assert_eq!(astral_path.chars().count(), 151);
        assert_eq!(astral_path.encode_utf16().count(), 301);
        assert!(
            parse_capture_declarations(&format!(
                "[cache.emoji]\npath = \"{astral_path}\"\ncapture = \"include\"\n"
            ))
            .is_err()
        );
        // "/" + 199 BMP chars = 200 UTF-16 code units = at bound → accepted.
        let bmp_path = format!("/{}", "a".repeat(199));
        assert_eq!(bmp_path.encode_utf16().count(), 200);
        parse_capture_declarations(&format!(
            "[cache.bmp]\npath = \"{bmp_path}\"\ncapture = \"include\"\n"
        ))
        .unwrap();
    }

    #[test]
    fn candidate_report_string_bounds_count_utf16_code_units() {
        // execution_id/snapshot_id ≤ 200, artifact_location ≤ 500 UTF-16 code
        // units (the api's zod). An astral scalar is 1 `char` but 2 code units,
        // so counting `chars().count()` would admit a body the api 400s.
        let base = CandidateReportRequest {
            submission_attempt_id: "subatt_01J1XY".into(),
            worker_claim_id: "claim_01J1XZ".into(),
            capture_epoch: 3,
            candidate_id: "cand_01J1Z0".into(),
            execution_id: "exec_01J1Z1".into(),
            snapshot_id: "snap_01J1Z2".into(),
            artifact_location: "r2://snapshots/cand_01J1Z0/seal".into(),
            source_lost: false,
        };
        base.validate().unwrap();

        // execution_id: "e" + 100 astral scalars = 201 code units → REJECTED
        // (would be ACCEPTED at chars().count() == 101 <= 200).
        let astral_exec = format!("e{}", "\u{1F600}".repeat(100));
        assert_eq!(astral_exec.chars().count(), 101);
        assert_eq!(astral_exec.encode_utf16().count(), 201);
        assert!(
            CandidateReportRequest {
                execution_id: astral_exec,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        // artifact_location: "a" + 250 astral scalars = 501 code units → REJECTED.
        let astral_loc = format!("a{}", "\u{1F600}".repeat(250));
        assert_eq!(astral_loc.encode_utf16().count(), 501);
        assert!(
            CandidateReportRequest {
                artifact_location: astral_loc,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        // Empty execution_id (min 1) → REJECTED.
        assert!(
            CandidateReportRequest {
                execution_id: String::new(),
                ..base.clone()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn terminal_ack_agent_id_bound_counts_utf16_code_units() {
        // agent_id is 1..120 UTF-16 code units (the api's builderLocalIdSchema).
        let base = WizardTerminalAck {
            agent_id: "builder-sugamo-1".into(),
            submission_attempt_id: "subatt_01J1XY".into(),
            worker_claim_id: "claim_01J1XZ".into(),
            reason: TerminalAckReason::AttemptEnded,
            failure_stage: None,
            failure_reason: None,
        };
        base.validate().unwrap();

        // "a" + 60 astral scalars = 121 code units → REJECTED (would be ACCEPTED
        // at chars().count() == 61 <= 120).
        let astral_agent = format!("a{}", "\u{1F600}".repeat(60));
        assert_eq!(astral_agent.chars().count(), 61);
        assert_eq!(astral_agent.encode_utf16().count(), 121);
        assert!(
            WizardTerminalAck {
                agent_id: astral_agent,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        // Empty agent_id (min 1) → REJECTED.
        assert!(
            WizardTerminalAck {
                agent_id: String::new(),
                ..base.clone()
            }
            .validate()
            .is_err()
        );
    }
}
