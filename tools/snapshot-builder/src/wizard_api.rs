//! Submission Wizard **builder-lane api client** (ato-wizard PR-2, slice 2) —
//! the transport half of the `interactive_capture` lane.
//!
//! Everything the builder sends to ato-api after claiming a wizard job lives
//! here: lease renew (§3.2), control poll (§3.3), progress (§3.4), hold-ready
//! (§3.5), candidate report (§3.6), candidate acceptance (§3.7) and the
//! RESTRICTED wizard terminal ack (§3.8) of
//! `docs/contracts/SUBMISSION_WIZARD_WIRE_V1.md`. The wire shapes are NOT
//! redeclared — every request/response body is the corresponding
//! [`crate::wizard_wire`] type, which already pins the strict field sets, the
//! null policy and the refinement rules against the api's zod mirror.
//!
//! **Why a seam at all.** Every other api call in this daemon is a bare free
//! function calling `ureq::` inline with a hand-built bearer header, no timeout
//! and no test seam, so no HTTP path in this crate is testable today. This
//! module adds the two seams that fixes:
//!
//! - [`WizardApi`] — the SEMANTIC seam (one method per wire section). The hold
//!   loop's [`ApiControlSource`] and [`LeaseRenewDriver`] are written against
//!   it, so they can be driven by a scripted fake with no sockets.
//! - [`HttpTransport`] — the BYTE seam under [`HttpWizardApi`], mirroring
//!   `upload.rs`'s injected `ImportCommandRunner`. It is what makes the FENCING-4
//!   split assertable: a test can look at the actual request line, headers and
//!   body of every call and prove the lease token appears in exactly one of them.
//!
//! **FENCING-4 transport (§1.1, D2), enforced by construction:**
//!
//! - `job_id` → URL path only. Never repeated in a body.
//! - `lease_token` → the [`LEASE_TOKEN_HEADER`] header only. Never a query
//!   param, never a body field — the strict body types have no such key, and the
//!   token's own type ([`LeaseToken`]) redacts itself in `Debug` and has no
//!   `Display`, so it cannot reach a log line by accident either.
//! - `submission_attempt_id` + `worker_claim_id` → body on POSTs, query on the
//!   control GET. This client STAMPS both from the [`Fencing4`] tuple over
//!   whatever the caller passed, so a request body can never disagree with the
//!   identity the request is fenced under.
//!
//! **Epoch handling** reuses the pinned rules rather than restating them:
//! [`control_poll_epoch_rule`] on every control response (observed `<=` server is
//! accepted — a stale observer is *behind*, not impostored; only observed `>`
//! server is a fault) and [`candidate_epoch_rule`] as a pre-flight on report /
//! acceptance (message epoch must equal the epoch the control channel delivered
//! with the candidate). Exact-equality fencing on the control poll would deadlock
//! the first capture and is never applied here.
//!
//! **Turned on by CONFIGURATION, not by this module.** Everything here runs
//! against a real hold once a builder is configured with a slot
//! (`--builder-id`/`--slot-id`/`--hold-proxy-listen`), which is what adds
//! `interactive_capture` to the claim's `supported_kinds`. Without one the api
//! never hands this daemon such a job, and the only consumer left is the §3.8
//! terminal ack.
//!
//! **Dead-code allow (module-scoped, same rationale as `hold_phase`):** this is a
//! binary crate, so `pub` items are dead unless `fn main` reaches them; most of
//! this module is reached only by its own tests until the live session lands.
#![allow(dead_code)]

use std::fmt;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::hold_phase::{ControlFault, ControlSource, HoldTermination, LeaseKeepalive};
use crate::wizard_wire::{
    CandidateAcceptanceRequest, CandidateAcceptanceResponse, CandidateReportRequest,
    CandidateReportResponse, ControlDirective, ControlQuery, ControlResponse, ERROR_CODE_FENCED,
    ErrorEnvelope, Fencing4, HoldReadyRequest, LEASE_TOKEN_HEADER, LeaseRenewRequest,
    LeaseRenewResponse, ProgressRequest, TerminalAckReason, WizardFailureStage, WizardStage,
    WizardTerminalAck, candidate_epoch_rule, control_poll_epoch_rule,
};

/// Connect timeout for one builder-lane request. The existing bare `ureq::`
/// calls in `main.rs` set NO timeout at all, which on this lane is a lease bug
/// rather than a slow request: a hung renew would silently burn the whole lease
/// window. Both budgets below are kept well under one renew interval (a third of
/// the lease TTL) so a stalled call still leaves a retry inside the window.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Overall per-request timeout (connect + transfer).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How many renew opportunities fit inside one lease window: the driver renews
/// after a THIRD of the observed lease TTL, so two consecutive failed renews
/// still leave a third attempt before the lease dies. The cadence is derived
/// from the TTL the server itself reported (§3.1/§3.2 `lease_expires_at`) — this
/// side never hardcodes the api's `LEASE_TTL_SEC`.
const LEASE_RENEW_DIVISOR: u32 = 3;
/// A failed renew retries after a third of a renew interval (i.e. three tries
/// per interval, nine per lease window).
const LEASE_RENEW_RETRY_DIVISOR: u32 = 3;
/// Floor on the retry backoff, so a pathologically short lease can never turn
/// the retry path into a busy loop against the api.
const LEASE_RENEW_MIN_RETRY: Duration = Duration::from_secs(1);

/// Backoff between control-poll retries ([`ApiControlSource::poll`]).
///
/// A single `502` from the edge — or one reset connection — must NOT destroy a
/// live author session: by the time the poll runs the lease has just been
/// renewed in the same call, so it is provably alive, while the author has spent
/// minutes in Step 4 bringing the app to the state being captured and a torn
/// down hold makes them redo the whole build + setup. This mirrors
/// [`LeaseRenewDriver::tick`], which already retries a non-fenced renew inside
/// the window rather than failing on the first blip; what BOUNDS both is the
/// same thing — the lease deadline, re-checked before every attempt, so a retry
/// only ever proceeds while the lease is provably alive.
///
/// It is a DEADLINE and not an attempt count on purpose. An api that is being
/// redeployed refuses connections for seconds, and each refusal comes back in
/// about a millisecond: a count of three would burn the whole budget in
/// milliseconds and tear down a hold whose lease still had minutes on it — the
/// exact failure this retry exists to prevent. The backoff is what keeps the
/// deadline bound from becoming a spin; it is slept through the [`WallClock`]
/// seam, so the fake clock drives it in tests without real time passing.
const CONTROL_POLL_RETRY_BACKOFF: Duration = Duration::from_secs(2);

/// How long a run of TRANSIENT control-poll blips is retried, captured ONCE at
/// the top of [`ApiControlSource::poll`] as `now + min(this, lease_remaining())`.
///
/// It CANNOT be the live lease deadline. The renew rides the same poll (§3.3),
/// and every successful renew pushes the lease deadline out — so a retry bounded
/// by that deadline is bounded by a thing the loop keeps extending. The one case
/// that matters is precisely when `/lease/renew` stays healthy while `/control`
/// does not (a builder/api version skew, or a control route the deployed api
/// answers with a persistent 5xx): the lease renews indefinitely and a
/// deadline-bounded retry never returns. Capturing the window on ENTRY makes the
/// bound immune to the renew.
///
/// The `min(_, lease_remaining())` keeps the original guarantee — a retry never
/// outlives the lease it rides on — while this fixed ceiling caps the OTHER
/// direction: a control channel unhealthy for a full minute while the lease
/// keeps renewing is not a blip, so the hold ends and releases the held guest
/// and the builder slot instead of holding them for the whole (renewing) lease.
/// A minute is many `CONTROL_POLL_RETRY_BACKOFF`s — ample to ride out an api
/// redeploy, which refuses connections for seconds — and well under a lease TTL.
const CONTROL_POLL_RETRY_WINDOW: Duration = Duration::from_secs(60);

/// The builder's own failure-reason budget: 1800 **UTF-16 code units** against
/// the api's 2000 — the same unit on both sides, which is the whole point (see
/// [`truncate`]). This is THE budget for the whole daemon — the legacy failed /
/// source-materialize acks in `main.rs` truncate through
/// [`truncate_failure_reason`] rather than re-spelling `1800` inline, so the
/// bound cannot drift on one lane and not another.
const FAILURE_REASON_BUDGET: usize = 1800;

/// Error bodies are non-secret server output, but they are still unbounded
/// remote input — cap what is carried into a local error.
const ERROR_BODY_BUDGET: usize = 1800;

// ─────────────────────────────────────────────────────────────────────────────
// The byte seam: one HTTP request/response, injectable
// ─────────────────────────────────────────────────────────────────────────────

/// One outbound builder-lane request, as bytes-on-the-wire facts. Deliberately
/// dumb: the whole point is that a test can assert on exactly what left the
/// process (method, full URL including query, headers, body) rather than on the
/// intent behind it.
#[derive(Clone)]
pub struct HttpRequest {
    pub method: &'static str,
    /// Absolute URL, query string included.
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// JSON body for POSTs; `None` for the control GET.
    pub body: Option<String>,
}

/// Redacted by hand, never derived: the headers of EVERY request on this lane
/// carry two secrets (the builder's bearer token and the lease token), and a
/// `{:?}` of a request while debugging a 409 is exactly how they would reach a
/// log.
impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), redacted_header_value(name, value)))
            .collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body", &self.body)
            .finish()
    }
}

/// The two header names this client sets that carry a secret. Compared
/// case-insensitively because HTTP header names are (and HTTP/2 lowercases them
/// on the wire).
fn redacted_header_value<'v>(name: &str, value: &'v str) -> &'v str {
    if name.eq_ignore_ascii_case(LEASE_TOKEN_HEADER) || name.eq_ignore_ascii_case("authorization") {
        "<redacted>"
    } else {
        value
    }
}

/// One inbound response. A non-2xx status is a RESPONSE here, not a transport
/// error: the §1 fencing rejection is a `409` whose body must be parsed to tell
/// `fenced` from any other 409.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// The injectable HTTP seam (production: [`UreqTransport`]). Mirrors
/// `upload.rs`'s `ImportCommandRunner` — the crate's established way to make a
/// side effect assertable without a mocking framework.
pub trait HttpTransport {
    /// `Err` is a TRANSPORT failure (DNS, connect, timeout, malformed
    /// response) — a status code is never an `Err`.
    fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, String>;
}

/// Production transport: one `ureq::Agent` with connect + overall timeouts.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        UreqTransport {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(CONNECT_TIMEOUT)
                .timeout(REQUEST_TIMEOUT)
                .build(),
        }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        UreqTransport::new()
    }
}

impl HttpTransport for UreqTransport {
    fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
        let mut call = match request.method {
            "GET" => self.agent.get(&request.url),
            "POST" => self.agent.post(&request.url),
            other => return Err(format!("unsupported method {other}")),
        };
        for (name, value) in &request.headers {
            call = call.set(name, value);
        }
        let sent = match &request.body {
            Some(body) => call
                .set("content-type", "application/json")
                .send_string(body),
            None => call.call(),
        };
        match sent {
            Ok(response) => Ok(HttpResponse {
                status: response.status(),
                body: response.into_string().unwrap_or_default(),
            }),
            // A status error still carries the body the §1 envelope needs.
            Err(ureq::Error::Status(status, response)) => Ok(HttpResponse {
                status,
                body: response.into_string().unwrap_or_default(),
            }),
            Err(err) => Err(err.to_string()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// A builder-lane api failure. No variant can carry the lease token: the only
/// inputs are the endpoint name, the server's own status/body, and locally
/// produced contract messages — none of which this client ever writes the token
/// into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardApiError {
    /// `409 { "error": "fenced", … }` (§1): the claim/lease is dead or an epoch
    /// rule was violated. The request had NO side effects server-side.
    Fenced { endpoint: String, message: String },
    /// Any other non-2xx.
    Status {
        endpoint: String,
        code: u16,
        body: String,
    },
    /// DNS/connect/timeout/etc — the request may or may not have been seen.
    Transport { endpoint: String, message: String },
    /// The exchange violated the wire contract on THIS side: a body that would
    /// be rejected server-side, a response that failed its own validation, or an
    /// epoch rule breach. Raised BEFORE a bad request leaves where possible.
    Contract { endpoint: String, message: String },
}

impl WizardApiError {
    /// `true` ⇒ the claim is fenced out. Fail closed: stop holding, tear down
    /// locally, and send NO terminal ack (§3.8 — lease expiry is server-owned).
    pub fn is_fenced(&self) -> bool {
        matches!(self, WizardApiError::Fenced { .. })
    }

    /// `true` ⇒ waiting and re-issuing the SAME request could plausibly succeed,
    /// so the builder-lane retry loops ([`ApiControlSource::poll`],
    /// [`LeaseRenewDriver::tick`]) may back off and try again inside their
    /// window. `false` ⇒ the failure is DETERMINISTIC — the next identical
    /// request fails identically — so a retry only burns the lease window and
    /// floods the log; the caller must fail closed at once instead.
    ///
    /// Only two shapes are genuinely transient:
    /// - [`WizardApiError::Transport`] — DNS/connect/reset/timeout, e.g. an api
    ///   mid-redeploy refusing connections for a few seconds.
    /// - a `5xx` [`WizardApiError::Status`] — a server-side error the edge or api
    ///   may recover from.
    ///
    /// Everything else is deterministic:
    /// - [`WizardApiError::Fenced`] — the claim is dead server-side (also caught
    ///   earlier by [`Self::is_fenced`]); a retry is another `409`.
    /// - [`WizardApiError::Contract`] — a body/response that breaks the wire
    ///   contract on THIS side (§1): builder ↔ api version skew, not a blip.
    /// - a `4xx` [`WizardApiError::Status`] — the deployed api does not have this
    ///   route (404/405) or rejected the request (4xx). `429` is the one 4xx that
    ///   is conventionally retryable, but a single-caller leased claim never
    ///   receives it; folding it in with the other 4xx is the fail-closed-safe
    ///   choice (the hold tears down and the server sweep owns the outcome),
    ///   whereas treating an unknown 4xx as retryable is what risks the loop
    ///   never terminating.
    pub fn is_retryable(&self) -> bool {
        match self {
            WizardApiError::Transport { .. } => true,
            WizardApiError::Status { code, .. } => (500..600).contains(code),
            WizardApiError::Fenced { .. } | WizardApiError::Contract { .. } => false,
        }
    }

    pub fn endpoint(&self) -> &str {
        match self {
            WizardApiError::Fenced { endpoint, .. }
            | WizardApiError::Status { endpoint, .. }
            | WizardApiError::Transport { endpoint, .. }
            | WizardApiError::Contract { endpoint, .. } => endpoint,
        }
    }
}

impl fmt::Display for WizardApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WizardApiError::Fenced { endpoint, message } => {
                write!(f, "{endpoint}: 409 fenced: {message}")
            }
            WizardApiError::Status {
                endpoint,
                code,
                body,
            } => write!(f, "{endpoint}: status {code}: {body}"),
            WizardApiError::Transport { endpoint, message } => {
                write!(f, "{endpoint}: transport: {message}")
            }
            WizardApiError::Contract { endpoint, message } => {
                write!(f, "{endpoint}: wire contract: {message}")
            }
        }
    }
}

impl std::error::Error for WizardApiError {}

impl From<WizardApiError> for ControlFault {
    fn from(err: WizardApiError) -> ControlFault {
        ControlFault {
            message: err.to_string(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The semantic seam
// ─────────────────────────────────────────────────────────────────────────────

/// The candidate command the control channel delivered for one epoch (§3.3):
/// `capture` responses carry a pre-minted `candidate_id`, and epoch ↔ candidate
/// is 1:1. Kept as the builder's memo of that pairing so the §1.2 candidate
/// epoch rule can be checked BEFORE a report or acceptance leaves — a report for
/// a superseded epoch is a `409 fenced` server-side, and there is no reason to
/// spend the round trip to learn that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureCommand {
    pub candidate_id: String,
    pub capture_epoch: u64,
}

impl CaptureCommand {
    /// `Some` only for a `capture` directive that carries its candidate — i.e.
    /// only for a response that already passed [`ControlResponse::validate`].
    pub fn from_control(response: &ControlResponse) -> Option<CaptureCommand> {
        match (response.directive, response.candidate_id.as_ref()) {
            (ControlDirective::Capture, Some(candidate_id)) => Some(CaptureCommand {
                candidate_id: candidate_id.clone(),
                capture_epoch: response.server_capture_epoch,
            }),
            _ => None,
        }
    }
}

/// The builder-lane api, one method per wire section. Every method takes the
/// [`Fencing4`] tuple: the identity is not an implementation detail of a body,
/// it is what the request is fenced under, and the client derives the body/query
/// ids from it.
pub trait WizardApi {
    /// §3.2 `POST /jobs/:job_id/lease/renew`.
    fn renew_lease(&self, fencing: &Fencing4) -> Result<LeaseRenewResponse, WizardApiError>;

    /// §3.3 `GET /jobs/:job_id/control`. The response is validated
    /// ([`ControlResponse::validate`]) and epoch-checked
    /// ([`control_poll_epoch_rule`]) before it is returned.
    fn poll_control(
        &self,
        fencing: &Fencing4,
        observed_capture_epoch: u64,
    ) -> Result<ControlResponse, WizardApiError>;

    /// §3.4 `POST /jobs/:job_id/progress`.
    fn report_progress(&self, fencing: &Fencing4, stage: WizardStage)
    -> Result<(), WizardApiError>;

    /// §3.5 `POST /jobs/:job_id/hold-ready`.
    fn report_hold_ready(
        &self,
        fencing: &Fencing4,
        request: &HoldReadyRequest,
    ) -> Result<(), WizardApiError>;

    /// §3.6 `POST /jobs/:job_id/candidates`. `command` is the control-channel
    /// pairing the report is cross-checked against (§1.2).
    fn report_candidate(
        &self,
        fencing: &Fencing4,
        command: &CaptureCommand,
        request: &CandidateReportRequest,
    ) -> Result<CandidateReportResponse, WizardApiError>;

    /// §3.7 `POST /jobs/:job_id/candidates/:candidate_id/acceptance`. NOT a
    /// job-terminal ack.
    fn report_candidate_acceptance(
        &self,
        fencing: &Fencing4,
        command: &CaptureCommand,
        request: &CandidateAcceptanceRequest,
    ) -> Result<CandidateAcceptanceResponse, WizardApiError>;

    /// §3.8 `POST /jobs/:job_id/ack` with the RESTRICTED wizard payload (no
    /// `status` member — the legacy sealed/failed ack body is a schema reject
    /// for this job kind).
    fn wizard_terminal_ack(
        &self,
        fencing: &Fencing4,
        ack: &WizardTerminalAck,
    ) -> Result<(), WizardApiError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// The production client
// ─────────────────────────────────────────────────────────────────────────────

/// The ureq-backed [`WizardApi`], generic over the byte seam so its request
/// shapes are unit-testable.
pub struct HttpWizardApi<T: HttpTransport> {
    /// Base api URL, trailing `/` normalized away.
    api_url: String,
    /// The builder's bearer credential — a SECOND secret, distinct from the
    /// lease token: the bearer proves "a builder", the lease token proves "this
    /// claim". The api requires both on every builder-lane route.
    agent_token: String,
    transport: T,
}

/// Never derived — see [`ArtifactStore`](crate::upload::ArtifactStore)'s Debug
/// for the same reasoning: this struct holds a live credential.
impl<T: HttpTransport> fmt::Debug for HttpWizardApi<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpWizardApi")
            .field("api_url", &self.api_url)
            .field("agent_token", &"<redacted>")
            .finish()
    }
}

impl<T: HttpTransport> HttpWizardApi<T> {
    pub fn new(api_url: String, agent_token: String, transport: T) -> Self {
        HttpWizardApi {
            api_url: api_url.trim_end_matches('/').to_string(),
            agent_token,
            transport,
        }
    }

    /// The injected byte seam, for the tests that assert on what left the
    /// process (including `main`'s, which drive the real client). Production
    /// code never reaches past the semantic seam.
    #[cfg(test)]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// `<api>/v1/capsule-snapshots/jobs/<job_id><suffix>` — `job_id` rides the
    /// PATH and nothing else (§1.1). `endpoint` is the WIRE-SECTION name for the
    /// error, never the path fragment: a rejected `job_id` is a fact about the
    /// call being made, and labelling it `/control` would name a thing no
    /// operator can look up.
    fn job_url(
        &self,
        endpoint: &str,
        fencing: &Fencing4,
        suffix: &str,
    ) -> Result<String, WizardApiError> {
        url_component_safe(endpoint, "job_id", &fencing.job_id)?;
        Ok(format!(
            "{}/v1/capsule-snapshots/jobs/{}{suffix}",
            self.api_url, fencing.job_id
        ))
    }

    /// The two credential headers. The lease token is written here and NOWHERE
    /// else in this module — [`LeaseToken::expose`](crate::wizard_wire::LeaseToken::expose)
    /// has exactly this one call site.
    fn headers(&self, fencing: &Fencing4) -> Vec<(String, String)> {
        vec![
            (
                "authorization".to_string(),
                format!("Bearer {}", self.agent_token),
            ),
            (
                LEASE_TOKEN_HEADER.to_string(),
                fencing.lease_token.expose().to_string(),
            ),
        ]
    }

    fn send(&self, endpoint: &str, request: &HttpRequest) -> Result<HttpResponse, WizardApiError> {
        self.transport
            .execute(request)
            .map_err(|message| WizardApiError::Transport {
                endpoint: endpoint.to_string(),
                message,
            })
    }

    /// POST a strict wire body and parse the response.
    fn post<B: Serialize, R: DeserializeOwned>(
        &self,
        endpoint: &str,
        fencing: &Fencing4,
        suffix: &str,
        body: &B,
    ) -> Result<R, WizardApiError> {
        let response = self.post_raw(endpoint, fencing, suffix, body)?;
        parse_ok(endpoint, response)
    }

    /// POST a strict wire body whose success response carries nothing this side
    /// consumes (`200 {}`).
    fn post_discarding_response<B: Serialize>(
        &self,
        endpoint: &str,
        fencing: &Fencing4,
        suffix: &str,
        body: &B,
    ) -> Result<(), WizardApiError> {
        let response = self.post_raw(endpoint, fencing, suffix, body)?;
        classify(endpoint, &response)?;
        Ok(())
    }

    fn post_raw<B: Serialize>(
        &self,
        endpoint: &str,
        fencing: &Fencing4,
        suffix: &str,
        body: &B,
    ) -> Result<HttpResponse, WizardApiError> {
        let encoded = serde_json::to_string(body).map_err(|e| WizardApiError::Contract {
            endpoint: endpoint.to_string(),
            message: format!("request body could not be encoded: {e}"),
        })?;
        let request = HttpRequest {
            method: "POST",
            url: self.job_url(endpoint, fencing, suffix)?,
            headers: self.headers(fencing),
            body: Some(encoded),
        };
        self.send(endpoint, &request)
    }
}

/// Classify a response: 2xx passes through, `409 {"error":"fenced"}` becomes
/// [`WizardApiError::Fenced`], everything else a [`WizardApiError::Status`].
fn classify(endpoint: &str, response: &HttpResponse) -> Result<(), WizardApiError> {
    if (200..300).contains(&response.status) {
        return Ok(());
    }
    // The §1 envelope is what distinguishes a FENCED 409 (claim dead — fail
    // closed, no ack) from any other 409 (e.g. a state conflict). Parse it
    // rather than assuming every 409 is a fence.
    if response.status == 409
        && let Ok(envelope) = serde_json::from_str::<ErrorEnvelope>(&response.body)
        && envelope.error == ERROR_CODE_FENCED
    {
        return Err(WizardApiError::Fenced {
            endpoint: endpoint.to_string(),
            message: truncate(&envelope.message, ERROR_BODY_BUDGET),
        });
    }
    Err(WizardApiError::Status {
        endpoint: endpoint.to_string(),
        code: response.status,
        body: truncate(&response.body, ERROR_BODY_BUDGET),
    })
}

fn parse_ok<R: DeserializeOwned>(
    endpoint: &str,
    response: HttpResponse,
) -> Result<R, WizardApiError> {
    classify(endpoint, &response)?;
    serde_json::from_str::<R>(&response.body).map_err(|e| WizardApiError::Contract {
        endpoint: endpoint.to_string(),
        message: format!("response did not match the wire contract: {e}"),
    })
}

/// The ONE place a fencing id may ride a URL is the control poll's query, plus
/// `job_id` in every path (§1.1). Neither is percent-encoded here: an id
/// carrying anything outside the RFC-3986 unreserved set FAILS CLOSED instead.
/// Server-minted ids are `<prefix><ULID>` (Crockford base32), so this never
/// rejects a real id; what it rejects is a skewed or hostile id smuggling `&`,
/// `?`, `#`, `/` or whitespace into a request line — which no guessed encoding
/// should paper over.
fn url_component_safe(endpoint: &str, name: &str, value: &str) -> Result<(), WizardApiError> {
    let unreserved = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~');
    if value.is_empty() || !value.chars().all(unreserved) {
        return Err(WizardApiError::Contract {
            endpoint: endpoint.to_string(),
            message: format!(
                "{name} is not URL-safe (expected a non-empty RFC-3986 unreserved string)"
            ),
        });
    }
    Ok(())
}

/// Truncate to at most `budget` **UTF-16 code units**, never splitting a scalar.
///
/// The unit matters: every length bound on this wire is counted in UTF-16 code
/// units (zod's `.max()` measures `String.length`), so a truncator counting
/// `chars()` measures a DIFFERENT quantity than the validator that follows it. A
/// diagnostic whose first 1800 scalars include 201 astral ones (emoji in a
/// Node/Vite log) is 2001+ code units after a `chars()` truncation — over the
/// api's 2000 bound, so `validate()` refuses the body and the terminal ack is
/// never sent AT ALL, leaving the job to the server sweep with no diagnostic.
/// Truncating in the validator's own unit closes that gap; a scalar is never cut
/// in half, so the result is always valid UTF-8 and at most `budget` units.
fn truncate(value: &str, budget: usize) -> String {
    // Fast path: pure ASCII (every build diagnostic that ever hit this before)
    // has one code unit per byte, so no per-char walk is needed to know it fits.
    if value.len() <= budget && value.is_ascii() {
        return value.to_string();
    }
    let mut truncated = String::new();
    let mut units = 0usize;
    for scalar in value.chars() {
        let width = scalar.len_utf16();
        if units + width > budget {
            break;
        }
        units += width;
        truncated.push(scalar);
    }
    truncated
}

/// Truncate a diagnostic to the builder's [`FAILURE_REASON_BUDGET`]. Shared with
/// the legacy (non-wizard) acks in `main.rs`: one budget, one call site shape.
pub fn truncate_failure_reason(value: &str) -> String {
    truncate(value, FAILURE_REASON_BUDGET)
}

impl<T: HttpTransport> WizardApi for HttpWizardApi<T> {
    fn renew_lease(&self, fencing: &Fencing4) -> Result<LeaseRenewResponse, WizardApiError> {
        let body = LeaseRenewRequest {
            submission_attempt_id: fencing.submission_attempt_id.clone(),
            worker_claim_id: fencing.worker_claim_id.clone(),
        };
        self.post("lease renew", fencing, "/lease/renew", &body)
    }

    fn poll_control(
        &self,
        fencing: &Fencing4,
        observed_capture_epoch: u64,
    ) -> Result<ControlResponse, WizardApiError> {
        const ENDPOINT: &str = "control poll";
        let query = ControlQuery {
            submission_attempt_id: fencing.submission_attempt_id.clone(),
            worker_claim_id: fencing.worker_claim_id.clone(),
            observed_capture_epoch,
        };
        // Build the query string from the STRICT wire struct's own serde
        // encoding, so the param names can never drift from `ControlQuery`
        // (§3.3) — and so the token cannot appear here even by a typo: the type
        // has no such field.
        let encoded = serde_json::to_value(&query).map_err(|e| WizardApiError::Contract {
            endpoint: ENDPOINT.to_string(),
            message: format!("control query could not be encoded: {e}"),
        })?;
        let fields = encoded
            .as_object()
            .ok_or_else(|| WizardApiError::Contract {
                endpoint: ENDPOINT.to_string(),
                message: "control query did not encode as an object".to_string(),
            })?;
        let mut params: Vec<String> = Vec::new();
        for (name, value) in fields {
            let rendered = match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            url_component_safe(ENDPOINT, name, &rendered)?;
            params.push(format!("{name}={rendered}"));
        }
        let request = HttpRequest {
            method: "GET",
            url: format!(
                "{}?{}",
                self.job_url(ENDPOINT, fencing, "/control")?,
                params.join("&")
            ),
            headers: self.headers(fencing),
            body: None,
        };
        let response: ControlResponse = parse_ok(ENDPOINT, self.send(ENDPOINT, &request)?)?;
        // Structural refinements the schema cannot express — without this a
        // `capture` directive with no `candidate_id` would reach the capture
        // seam and be captured against nothing.
        response
            .validate()
            .map_err(|message| WizardApiError::Contract {
                endpoint: ENDPOINT.to_string(),
                message,
            })?;
        // §1.2: observed <= server is ACCEPTED (that is how a lagging builder
        // catches up); only a server epoch BEHIND what we already observed is
        // corrupt state. Never exact equality — that would deadlock the first
        // capture, which necessarily arrives with server 1 > observed 0.
        control_poll_epoch_rule(observed_capture_epoch, response.server_capture_epoch).map_err(
            |message| WizardApiError::Contract {
                endpoint: ENDPOINT.to_string(),
                message,
            },
        )?;
        Ok(response)
    }

    fn report_progress(
        &self,
        fencing: &Fencing4,
        stage: WizardStage,
    ) -> Result<(), WizardApiError> {
        let body = ProgressRequest {
            submission_attempt_id: fencing.submission_attempt_id.clone(),
            worker_claim_id: fencing.worker_claim_id.clone(),
            stage,
        };
        self.post_discarding_response("progress", fencing, "/progress", &body)
    }

    fn report_hold_ready(
        &self,
        fencing: &Fencing4,
        request: &HoldReadyRequest,
    ) -> Result<(), WizardApiError> {
        const ENDPOINT: &str = "hold-ready";
        // The fencing tuple is the single source of truth for the identity
        // fields: a caller-supplied body can never disagree with the identity
        // the request is fenced under.
        let body = HoldReadyRequest {
            submission_attempt_id: fencing.submission_attempt_id.clone(),
            worker_claim_id: fencing.worker_claim_id.clone(),
            ..request.clone()
        };
        body.validate()
            .map_err(|message| WizardApiError::Contract {
                endpoint: ENDPOINT.to_string(),
                message,
            })?;
        self.post_discarding_response(ENDPOINT, fencing, "/hold-ready", &body)
    }

    fn report_candidate(
        &self,
        fencing: &Fencing4,
        command: &CaptureCommand,
        request: &CandidateReportRequest,
    ) -> Result<CandidateReportResponse, WizardApiError> {
        const ENDPOINT: &str = "candidate report";
        let body = CandidateReportRequest {
            submission_attempt_id: fencing.submission_attempt_id.clone(),
            worker_claim_id: fencing.worker_claim_id.clone(),
            ..request.clone()
        };
        body.validate()
            .map_err(|message| WizardApiError::Contract {
                endpoint: ENDPOINT.to_string(),
                message,
            })?;
        check_candidate_pairing(ENDPOINT, command, &body.candidate_id, body.capture_epoch)?;
        self.post(ENDPOINT, fencing, "/candidates", &body)
    }

    fn report_candidate_acceptance(
        &self,
        fencing: &Fencing4,
        command: &CaptureCommand,
        request: &CandidateAcceptanceRequest,
    ) -> Result<CandidateAcceptanceResponse, WizardApiError> {
        const ENDPOINT: &str = "candidate acceptance";
        let body = CandidateAcceptanceRequest {
            submission_attempt_id: fencing.submission_attempt_id.clone(),
            worker_claim_id: fencing.worker_claim_id.clone(),
            // §3.7 bounds `failure_reason` exactly as §3.8 does (≤ 2000 UTF-16
            // code units, builder truncates at 1800), so it is truncated here for
            // the same reason the §3.8 ack builders truncate: a rejection
            // diagnostic is unbounded acceptance output, and letting it overrun
            // turns the report into a LOCAL Contract error — the api never hears
            // that the candidate was rejected at all.
            failure_reason: request
                .failure_reason
                .as_deref()
                .map(truncate_failure_reason),
            ..request.clone()
        };
        body.validate()
            .map_err(|message| WizardApiError::Contract {
                endpoint: ENDPOINT.to_string(),
                message,
            })?;
        // The candidate in the PATH comes from the control command, never from
        // caller-supplied data, so there is no id to CROSS-check here (checking
        // the command against itself would read like a guard while proving
        // nothing). What is caller-supplied is the epoch, and it must still name
        // the pairing the control channel delivered (§1.2).
        candidate_epoch_rule(body.capture_epoch, command.capture_epoch).map_err(|message| {
            WizardApiError::Contract {
                endpoint: ENDPOINT.to_string(),
                message,
            }
        })?;
        url_component_safe(ENDPOINT, "candidate_id", &command.candidate_id)?;
        let suffix = format!("/candidates/{}/acceptance", command.candidate_id);
        self.post(ENDPOINT, fencing, &suffix, &body)
    }

    fn wizard_terminal_ack(
        &self,
        fencing: &Fencing4,
        ack: &WizardTerminalAck,
    ) -> Result<(), WizardApiError> {
        const ENDPOINT: &str = "wizard terminal ack";
        let body = WizardTerminalAck {
            submission_attempt_id: fencing.submission_attempt_id.clone(),
            worker_claim_id: fencing.worker_claim_id.clone(),
            ..ack.clone()
        };
        body.validate()
            .map_err(|message| WizardApiError::Contract {
                endpoint: ENDPOINT.to_string(),
                message,
            })?;
        self.post_discarding_response(ENDPOINT, fencing, "/ack", &body)
    }
}

/// §1.2 pre-flight for §3.6/§3.7: the message must name the candidate the
/// control channel delivered, at that candidate's epoch (epoch ↔ candidate is
/// 1:1). Both would be a `409 fenced` server-side with no side effects; failing
/// here keeps a superseded report from being spent as a round trip.
fn check_candidate_pairing(
    endpoint: &str,
    command: &CaptureCommand,
    candidate_id: &str,
    capture_epoch: u64,
) -> Result<(), WizardApiError> {
    if candidate_id != command.candidate_id {
        return Err(WizardApiError::Contract {
            endpoint: endpoint.to_string(),
            message: format!(
                "candidate_id {candidate_id} is not the candidate the control channel delivered for epoch {}",
                command.capture_epoch
            ),
        });
    }
    candidate_epoch_rule(capture_epoch, command.capture_epoch).map_err(|message| {
        WizardApiError::Contract {
            endpoint: endpoint.to_string(),
            message,
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// §3.8 terminal-ack helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Map a builder-lane failure stage string onto the §2 `failure_stage` enum by
/// PARSING IT AS THE ENUM — never a hand-written table, which could drift from
/// the wire strings. A builder stage with no wizard counterpart (`source`,
/// `artifact_metadata`, `claim_kind`, …) simply omits the optional refinement:
/// §3.8 makes it a refinement of `reason`, never a substitute for it.
pub fn wizard_failure_stage(stage: &str) -> Option<WizardFailureStage> {
    serde_json::from_value(serde_json::Value::String(stage.to_string())).ok()
}

/// §3.8 — ack an `interactive_capture` job that failed before ever reaching
/// `holding`. That is exactly `reason: "build_failed"`.
///
/// This is the RESTRICTED payload, not the legacy failed ack: the legacy body
/// carries `status` (a strict-mode reject for this job kind) and none of
/// FENCING-4 (a `409` even if the schema passed), so the legacy call would never
/// have landed against the deployed api.
pub fn ack_interactive_build_failure(
    api: &dyn WizardApi,
    agent_id: &str,
    fencing: &Fencing4,
    stage: &str,
    reason: &str,
) -> Result<(), WizardApiError> {
    api.wizard_terminal_ack(
        fencing,
        &WizardTerminalAck {
            agent_id: agent_id.to_string(),
            submission_attempt_id: fencing.submission_attempt_id.clone(),
            worker_claim_id: fencing.worker_claim_id.clone(),
            reason: TerminalAckReason::BuildFailed,
            failure_stage: wizard_failure_stage(stage),
            failure_reason: Some(truncate(reason, FAILURE_REASON_BUDGET)),
        },
    )
}

/// §3.8 — ack the terminal outcome of a completed hold, or send NOTHING when the
/// hold projects to no legal ack ([`HoldTermination::TornDownWithoutAck`]: the
/// lease is dead or in doubt, and expiry is server-owned).
pub fn ack_hold_termination(
    api: &dyn WizardApi,
    agent_id: &str,
    fencing: &Fencing4,
    termination: &HoldTermination,
) -> Result<(), WizardApiError> {
    let Some(reason) = termination.terminal_ack_reason() else {
        return Ok(());
    };
    // EXHAUSTIVE, not `_ => None`. The wildcard it replaces silently dropped the
    // diagnostic of any variant added after it, and `failure_reason` is the only
    // field of the ack that reaches a human — it is what the api stores as
    // `error_summary` and what the wizard shows the author. A new failure whose
    // reason vanishes would present as a bare `build_failed` with nothing to act
    // on, so the compiler is made to ask about each one.
    let failure_reason = match termination {
        HoldTermination::AcceptanceFailedSourceLost { failure_reason }
        | HoldTermination::FailedClosed { failure_reason } => {
            Some(truncate(failure_reason, FAILURE_REASON_BUDGET))
        }
        // #1160 — the count is part of the diagnosis ("it tried three times"),
        // so it leads the reason rather than living only in a builder log.
        HoldTermination::CaptureBudgetExhausted {
            attempts,
            failure_reason,
        } => Some(truncate(
            &format!("after {attempts} capture attempt(s): {failure_reason}"),
            FAILURE_REASON_BUDGET,
        )),
        // No failure to describe: these ended cleanly.
        HoldTermination::Accepted { .. }
        | HoldTermination::Discarded
        | HoldTermination::AttemptEnded => None,
        // Unreachable: `terminal_ack_reason()` is `None` for it, so the early
        // return above already sent nothing. Named rather than wildcarded so the
        // next variant still has to be decided here.
        HoldTermination::TornDownWithoutAck { .. } => None,
    };
    api.wizard_terminal_ack(
        fencing,
        &WizardTerminalAck {
            agent_id: agent_id.to_string(),
            submission_attempt_id: fencing.submission_attempt_id.clone(),
            worker_claim_id: fencing.worker_claim_id.clone(),
            reason,
            failure_stage: termination.failure_stage(),
            failure_reason,
        },
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Lease renew driver (§3.2)
// ─────────────────────────────────────────────────────────────────────────────

/// The wall clock `lease_expires_at` is measured against.
///
/// Deliberately NOT `snapshot::acceptance::MonotonicClock`: the lease deadline
/// is a SERVER wall-clock instant (ISO-8601 UTC), and comparing it to a
/// process-local `Instant` — which has no relationship to any clock the api can
/// name — is a category error. The hold loop keeps using the monotonic clock for
/// its own TTL; this driver uses the clock the deadline is expressed in.
pub trait WallClock {
    fn now_utc(&self) -> SystemTime;

    /// Wait `duration` before the caller looks at [`Self::now_utc`] again.
    ///
    /// Waiting belongs on the clock seam rather than on a bare
    /// `std::thread::sleep`: a backoff is a statement about the SAME time the
    /// deadline is expressed in, and a fake clock that advances instead of
    /// sleeping is what lets the deadline-bounded retry loops below be tested
    /// without spending real minutes.
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now_utc(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Parse an ISO-8601 / RFC-3339 UTC instant (`lease_expires_at`, §3.1/§3.2).
/// Fail-closed: an unparseable deadline is an error, never "assume far away".
fn parse_utc_instant(value: &str) -> Result<SystemTime, String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value.trim())
        .map_err(|e| format!("lease_expires_at {value:?} is not an ISO-8601 UTC instant: {e}"))?;
    let millis = parsed.timestamp_millis();
    if millis < 0 {
        return Err(format!("lease_expires_at {value:?} predates the epoch"));
    }
    Ok(SystemTime::UNIX_EPOCH + Duration::from_millis(millis as u64))
}

/// The renew cadence for an observed lease TTL — a third of the window, so two
/// consecutive failures still leave a try.
pub fn renew_interval(lease_ttl: Duration) -> Duration {
    lease_ttl / LEASE_RENEW_DIVISOR
}

/// Why the hold stopped holding, from the lease's point of view. **Every variant
/// is fail-closed and sends NO terminal ack** (§3.8): `Fenced`/`Expired` are the
/// server-owned expiry path by definition, and a `Contract` fault leaves the
/// lease unproven — a builder that cannot show its lease is alive must not
/// assert a job-terminal state either. The server sweep owns the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseLost {
    /// The api answered `409 fenced` — the claim is already dead.
    Fenced { message: String },
    /// The observed lease deadline passed without a successful renew.
    Expired { message: String },
    /// The lease deadline could not be established (unparseable
    /// `lease_expires_at`).
    Contract { message: String },
}

impl LeaseLost {
    pub fn message(&self) -> &str {
        match self {
            LeaseLost::Fenced { message }
            | LeaseLost::Expired { message }
            | LeaseLost::Contract { message } => message,
        }
    }
}

impl fmt::Display for LeaseLost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LeaseLost::Fenced { message } => write!(f, "lease fenced: {message}"),
            LeaseLost::Expired { message } => write!(f, "lease expired: {message}"),
            LeaseLost::Contract { message } => write!(f, "lease contract: {message}"),
        }
    }
}

impl From<LeaseLost> for ControlFault {
    fn from(lost: LeaseLost) -> ControlFault {
        ControlFault {
            message: lost.to_string(),
        }
    }
}

/// Keeps a claimed interactive lease alive across a hold that may run far longer
/// than one lease window (the hold TTL is 30 minutes; the api's lease TTL is
/// minutes). Without this a real hold is fenced out mid-session and every
/// subsequent call — including its terminal ack — is a `409`.
///
/// The driver renews only when [`Self::tick`] is called, so what makes the claim
/// above TRUE is that every long-running step of the hold drives a tick, not
/// just the control poll. [`ApiControlSource`] ticks on each poll AND exposes
/// the same tick as [`ControlSource::keepalive`], which
/// [`crate::hold_phase::HoldPhase`] hands to the capture action and to every
/// productive phase of the acceptance run — the two stretches (a pause + seal +
/// upload, and a disposable restore + verify) that each run for minutes without
/// a poll in between and would otherwise outlive the lease.
pub struct LeaseRenewDriver<'a, C: WallClock> {
    api: &'a dyn WizardApi,
    clock: &'a C,
    /// The server's own deadline for the CURRENT lease.
    lease_deadline: SystemTime,
    /// When [`Self::tick`] will next attempt a renew.
    next_renew_at: SystemTime,
    /// Derived from the observed TTL — never a hardcoded api constant.
    renew_interval: Duration,
    renews: u64,
}

/// Hand-written because the driver holds a `&dyn WizardApi` (not `Debug`) — and
/// because the useful facts are the two deadlines, which are non-secret.
impl<C: WallClock> fmt::Debug for LeaseRenewDriver<'_, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseRenewDriver")
            .field("lease_deadline", &self.lease_deadline)
            .field("next_renew_at", &self.next_renew_at)
            .field("renew_interval", &self.renew_interval)
            .field("renews", &self.renews)
            .finish()
    }
}

impl<'a, C: WallClock> LeaseRenewDriver<'a, C> {
    /// Adopt the lease minted by the §3.1 claim (`lease_expires_at`). Fails
    /// closed on a deadline that is unparseable or already past.
    pub fn new(
        api: &'a dyn WizardApi,
        clock: &'a C,
        lease_expires_at: &str,
    ) -> Result<Self, LeaseLost> {
        let mut driver = LeaseRenewDriver {
            api,
            clock,
            lease_deadline: SystemTime::UNIX_EPOCH,
            next_renew_at: SystemTime::UNIX_EPOCH,
            renew_interval: Duration::ZERO,
            renews: 0,
        };
        driver.adopt(lease_expires_at)?;
        Ok(driver)
    }

    /// Successful renews so far (diagnostics + tests).
    pub fn renews(&self) -> u64 {
        self.renews
    }

    /// How long the observed lease still has, saturating to zero once the
    /// deadline has passed (at which point the next [`Self::tick`] fails closed).
    ///
    /// This is the ONE bound a retry on this lane is allowed to use: the lease is
    /// the only thing that makes a further api call legitimate.
    pub fn lease_remaining(&self) -> Duration {
        self.lease_deadline
            .duration_since(self.clock.now_utc())
            .unwrap_or(Duration::ZERO)
    }

    /// The wall clock this driver measures the deadline against — shared with
    /// [`ApiControlSource`] so a retry backoff waits on the same clock the
    /// deadline is expressed in rather than inventing a second notion of time.
    pub fn clock(&self) -> &C {
        self.clock
    }

    fn adopt(&mut self, lease_expires_at: &str) -> Result<(), LeaseLost> {
        let now = self.clock.now_utc();
        let deadline = parse_utc_instant(lease_expires_at)
            .map_err(|message| LeaseLost::Contract { message })?;
        // `duration_since` errors exactly when the deadline is already behind
        // us — which is a dead lease, not a negative duration to clamp.
        let ttl = deadline
            .duration_since(now)
            .map_err(|_| LeaseLost::Expired {
                message: format!("lease deadline {lease_expires_at} is already in the past"),
            })?;
        self.lease_deadline = deadline;
        self.renew_interval = renew_interval(ttl);
        self.next_renew_at = now + self.renew_interval;
        Ok(())
    }

    /// Drive the lease one step. Call it on every hold-loop iteration: it renews
    /// only when the cadence says to, and it is the loop's fail-closed gate —
    /// `Err` means STOP HOLDING (and send no ack).
    ///
    /// A transport failure is NOT immediately terminal: it defers to a shorter
    /// retry inside the same lease window. What bounds that retry is the
    /// deadline check at the top — once the observed deadline passes with no
    /// successful renew, the lease is gone whatever the network said.
    pub fn tick(&mut self, fencing: &Fencing4) -> Result<(), LeaseLost> {
        let now = self.clock.now_utc();
        if now >= self.lease_deadline {
            return Err(LeaseLost::Expired {
                message: "the observed lease deadline passed without a successful renew"
                    .to_string(),
            });
        }
        if now < self.next_renew_at {
            return Ok(());
        }
        match self.api.renew_lease(fencing) {
            Ok(renewed) => {
                self.renews += 1;
                self.adopt(&renewed.lease_expires_at)
            }
            // A fenced renew is definitive: the claim is dead server-side.
            Err(err) if err.is_fenced() => Err(LeaseLost::Fenced {
                message: err.to_string(),
            }),
            // A non-fenced but DETERMINISTIC renew failure — a `200` whose body
            // does not match the §3.2 wire contract, or a 4xx from a skewed api —
            // cannot be cleared by waiting: deferring it to a backoff just burns
            // the rest of the lease window on a call that answers identically. The
            // lease is then unprovable, so it is lost NOW, fail closed with no ack
            // (§3.8) — `LeaseLost::Contract` is exactly "the lease could not be
            // kept because the exchange broke the contract".
            Err(err) if !err.is_retryable() => Err(LeaseLost::Contract {
                message: err.to_string(),
            }),
            // A transient blip (5xx / transport): defer to a shorter retry inside
            // the window. The deadline check at the top is what bounds it.
            Err(err) => {
                let backoff =
                    (self.renew_interval / LEASE_RENEW_RETRY_DIVISOR).max(LEASE_RENEW_MIN_RETRY);
                self.next_renew_at = now + backoff;
                eprintln!(
                    "[builder] wizard lease renew failed, retrying before the deadline: {err}"
                );
                Ok(())
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Production ControlSource (§3.3)
// ─────────────────────────────────────────────────────────────────────────────

/// The production [`ControlSource`]: the hold loop's control poll, plus the
/// lease renew riding on it.
///
/// The renew is driven from the poll on purpose. The poll IS the hold loop's
/// heartbeat, so renewing here keeps the whole hold single-threaded (no
/// background thread racing a pause/capture) and makes the safety property
/// automatic: a builder that has stopped polling has also stopped renewing, so a
/// wedged builder loses its lease instead of holding a preview binding open.
///
/// The two stretches that run for minutes WITHOUT a poll — the capture itself
/// and the acceptance run — drive the same renew through
/// [`ControlSource::keepalive`] instead. Same driver, same deadline, same
/// fail-closed verdict; the only difference is that no directive is consumed.
pub struct ApiControlSource<'a, C: WallClock> {
    api: &'a dyn WizardApi,
    fencing: &'a Fencing4,
    lease: LeaseRenewDriver<'a, C>,
    /// Pacing between polls; `ZERO` in tests.
    poll_interval: Duration,
    polls: u64,
    last_capture_command: Option<CaptureCommand>,
}

impl<'a, C: WallClock> ApiControlSource<'a, C> {
    pub fn new(
        api: &'a dyn WizardApi,
        fencing: &'a Fencing4,
        lease: LeaseRenewDriver<'a, C>,
        poll_interval: Duration,
    ) -> Self {
        ApiControlSource {
            api,
            fencing,
            lease,
            poll_interval,
            polls: 0,
            last_capture_command: None,
        }
    }

    pub fn polls(&self) -> u64 {
        self.polls
    }

    /// The most recent `capture` command, for the §1.2 pre-flight on the
    /// candidate report / acceptance. A `hold` or `discard` never clears it: the
    /// pairing stays valid until a NEWER capture command supersedes it.
    pub fn last_capture_command(&self) -> Option<&CaptureCommand> {
        self.last_capture_command.as_ref()
    }
}

impl<'a, C: WallClock> ApiControlSource<'a, C> {
    /// Make ONE builder-lane call under the hold's two fail-closed bounds, and
    /// retry it only while both hold.
    ///
    /// Every call this source makes — the control poll and the two candidate
    /// reports — has the same shape: it is legitimate only while the lease is,
    /// and a transient edge blip must not cost the author their live session.
    /// Sharing the loop is not just brevity: three copies would be three chances
    /// for one of them to drift into retrying past the lease.
    ///
    /// `what` names the call in the retry log. It is a builder-local label, not
    /// a wire value.
    fn under_lease<T>(
        &mut self,
        what: &str,
        call: impl Fn(&dyn WizardApi, &Fencing4) -> Result<T, WizardApiError>,
    ) -> Result<T, ControlFault> {
        // Copied out so the closure below borrows neither `self` nor the lease
        // it drives.
        let api = self.api;
        let fencing = self.fencing;
        // The retry window for a TRANSIENT blip, captured ONCE here — before the
        // loop can renew the lease. It must not be the live lease deadline: the
        // `tick` below renews on cadence, and every success pushes that deadline
        // out, so a retry bounded by it is bounded by a thing the loop keeps
        // extending — and a route that fails deterministically while
        // `/lease/renew` stays healthy would then spin forever. `min` with the
        // lease keeps the "never retry past the lease" guarantee; the fixed
        // ceiling ([`CONTROL_POLL_RETRY_WINDOW`]) caps a persistently-unhealthy
        // channel so the held guest and builder slot are released.
        let retry_deadline = self.lease.clock().now_utc()
            + CONTROL_POLL_RETRY_WINDOW.min(self.lease.lease_remaining());
        let mut attempt: u32 = 1;
        loop {
            // Lease first — and again before every retry: a dead lease makes the
            // call itself a 409, and the hold must end on the lease's terms (no
            // ack), not on whatever the route would have answered. `tick` renews
            // on cadence and fails closed the moment the observed deadline passes
            // with no successful renew — the bound for the case where
            // `/lease/renew` ALSO fails.
            self.lease.tick(fencing)?;
            match call(api, fencing) {
                Ok(value) => return Ok(value),
                // A fenced answer is definitive: the claim is already dead
                // server-side, so there is nothing to retry into.
                Err(err) if err.is_fenced() => return Err(err.into()),
                // A DETERMINISTIC non-fenced fault — a body this builder cannot
                // parse or that fails its refinements (`Contract`, §1.2/§3.3),
                // or a 4xx from an api that does not have this route (version
                // skew) — answers identically on every retry while the lease
                // renews on, so a retry loop never exits. Fail closed at once,
                // per `ControlFault`'s contract: it leaves the lease in doubt —
                // tear down locally, ack nothing.
                Err(err) if !err.is_retryable() => return Err(err.into()),
                // A transient blip — a 5xx from the edge, a reset connection, a
                // timeout — must not cost the author their live session. There is
                // deliberately NO attempt count: an api mid-redeploy refuses
                // connections for seconds and answers each attempt in a
                // millisecond, so a count would be spent before the outage was.
                // The bound is the entry-captured `retry_deadline` (which no renew
                // can extend); the backoff keeps it from being a spin.
                Err(err) => {
                    let now = self.lease.clock().now_utc();
                    if now >= retry_deadline {
                        // The window is spent — a control channel unhealthy this
                        // whole time while the lease kept renewing is not a blip.
                        // Fail closed on it rather than renew the lease forever.
                        return Err(err.into());
                    }
                    eprintln!(
                        "[builder] wizard {what} failed (attempt {attempt}), retrying \
                         inside the lease window: {err}"
                    );
                    attempt += 1;
                    // Never wait PAST either bound — the lease (`tick`, above) or
                    // the entry-captured retry window: sleeping the full backoff
                    // on a window with 200ms left would just delay the fail-closed
                    // return by the difference.
                    let backoff = CONTROL_POLL_RETRY_BACKOFF
                        .min(self.lease.lease_remaining())
                        .min(retry_deadline.duration_since(now).unwrap_or(Duration::ZERO));
                    self.lease.clock().sleep(backoff);
                }
            }
        }
    }

    /// The control-channel pairing a report must be cross-checked against
    /// (§1.2), or a fail-closed refusal.
    ///
    /// The memo is the one this source itself recorded from a `capture`
    /// directive. Reporting against anything else — an epoch the caller
    /// restated, say — would make the builder the author of the pairing the
    /// server is meant to be checking it against.
    fn capture_command_for(&self, capture_epoch: u64) -> Result<CaptureCommand, ControlFault> {
        let command = self.last_capture_command.clone().ok_or(ControlFault {
            message: "no capture command was delivered on this control channel, so there is \
                      no candidate to report against"
                .to_string(),
        })?;
        if command.capture_epoch != capture_epoch {
            return Err(ControlFault {
                message: format!(
                    "report is for epoch {capture_epoch} but the control channel's last \
                     capture command was epoch {}",
                    command.capture_epoch
                ),
            });
        }
        Ok(command)
    }
}

impl<C: WallClock> ControlSource for ApiControlSource<'_, C> {
    fn poll(&mut self, observed_capture_epoch: u64) -> Result<ControlResponse, ControlFault> {
        if self.polls > 0 && !self.poll_interval.is_zero() {
            self.lease.clock().sleep(self.poll_interval);
        }
        self.polls += 1;
        let response = self.under_lease("control poll", |api, fencing| {
            api.poll_control(fencing, observed_capture_epoch)
        })?;
        if let Some(command) = CaptureCommand::from_control(&response) {
            self.last_capture_command = Some(command);
        }
        Ok(response)
    }

    /// §3.6 over the wire, cross-checked against the pairing this source
    /// recorded (§1.2) before the round trip is spent.
    ///
    /// **A retry can lose a report that landed.** `is_retryable` covers the
    /// transport, and a request whose RESPONSE was lost has been applied
    /// server-side; the retry then answers `409 fenced` ("candidate is not
    /// awaiting a report"), and this ends the hold with no ack. That is the
    /// fail-closed side of the trade: the server owns a candidate it did report,
    /// the sweep resolves the attempt, and nothing is double-published. The
    /// alternative — no retry at all — would throw away good candidates for
    /// every ordinary edge blip, which is the far more frequent event.
    fn report_candidate(&mut self, report: &CandidateReportRequest) -> Result<(), ControlFault> {
        report
            .validate()
            .map_err(|message| ControlFault { message })?;
        let command = self.capture_command_for(report.capture_epoch)?;
        self.under_lease("candidate report", |api, fencing| {
            api.report_candidate(fencing, &command, report).map(|_| ())
        })
    }

    /// §3.7 over the wire. Same pairing check: the candidate id rides the PATH,
    /// and it comes from the control channel's memo rather than from the caller.
    fn report_acceptance(
        &mut self,
        request: &CandidateAcceptanceRequest,
    ) -> Result<(), ControlFault> {
        request
            .validate()
            .map_err(|message| ControlFault { message })?;
        let command = self.capture_command_for(request.capture_epoch)?;
        self.under_lease("candidate acceptance", |api, fencing| {
            api.report_candidate_acceptance(fencing, &command, request)
                .map(|_| ())
        })
    }
}

/// Drive the lease WITHOUT polling the control channel — see [`LeaseKeepalive`]
/// for why the hold needs one.
///
/// This is exactly the tick the poll already does, exposed on its own so the
/// stretches that run for minutes between polls (capture, acceptance) keep the
/// same lease alive through the same driver, with the same fail-closed verdict.
/// It deliberately does NOT poll: a keepalive must not consume a directive that
/// the hold loop is not in a position to act on.
impl<C: WallClock> LeaseKeepalive for ApiControlSource<'_, C> {
    fn keepalive(&mut self) -> Result<(), ControlFault> {
        self.lease.tick(self.fencing).map_err(Into::into)
    }
}

/// Test doubles for the byte seam, shared with `main`'s tests.
///
/// The §3.8 ack ROUTING (which body is sent for an `interactive_capture`
/// outcome, and when none may be sent) lives in `main::run_once`, not here, and
/// pinning it against a second hand-rolled fake would prove only that the fake
/// agrees with itself. It is asserted against the REAL [`HttpWizardApi`] driving
/// this transport instead, so the request bytes it produces are the ones under
/// test.
#[cfg(test)]
pub mod testing {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::{HttpRequest, HttpResponse, HttpTransport};

    /// Records every request that left the client and replays scripted
    /// responses — the byte seam this crate previously lacked entirely.
    pub struct RecordingTransport {
        script: Mutex<VecDeque<Result<HttpResponse, String>>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl RecordingTransport {
        pub fn new(script: Vec<Result<HttpResponse, String>>) -> Self {
            RecordingTransport {
                script: Mutex::new(script.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        /// A transport that answers every call with the same 200 body.
        pub fn always_ok(body: serde_json::Value, calls: usize) -> Self {
            RecordingTransport::new(
                (0..calls)
                    .map(|_| {
                        Ok(HttpResponse {
                            status: 200,
                            body: body.to_string(),
                        })
                    })
                    .collect(),
            )
        }

        pub fn requests(&self) -> Vec<HttpRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl HttpTransport for RecordingTransport {
        fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
            self.requests.lock().unwrap().push(request.clone());
            match self.script.lock().unwrap().pop_front() {
                Some(scripted) => scripted,
                None => panic!("unscripted request: {} {}", request.method, request.url),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    use serde_json::{Value, json};

    use super::testing::RecordingTransport;
    use super::*;
    use crate::wizard_wire::{
        ACCEPTANCE_RECEIPT_SCHEMA, AcceptanceReceipt, AcceptanceReceiptSchema, AcceptanceStatus,
        CandidateStatus, LeaseToken,
    };

    // ── fixtures ────────────────────────────────────────────────────────────

    const API: &str = "https://api.example/";
    const AGENT_TOKEN: &str = "agent-bearer-secret";
    const LEASE_SECRET: &str = "b64u-opaque-lease-token";

    fn fencing() -> Fencing4 {
        Fencing4 {
            job_id: "job_01J1XY".to_string(),
            submission_attempt_id: "subatt_01J1XY".to_string(),
            worker_claim_id: "claim_01J1XZ".to_string(),
            lease_token: LeaseToken::new(LEASE_SECRET.to_string()),
        }
    }

    fn client(transport: RecordingTransport) -> HttpWizardApi<RecordingTransport> {
        HttpWizardApi::new(API.to_string(), AGENT_TOKEN.to_string(), transport)
    }

    fn header<'r>(request: &'r HttpRequest, name: &str) -> Option<&'r str> {
        request
            .headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn body_json(request: &HttpRequest) -> Value {
        serde_json::from_str(request.body.as_ref().expect("POST carries a body"))
            .expect("body is JSON")
    }

    fn hold_ready() -> HoldReadyRequest {
        HoldReadyRequest {
            submission_attempt_id: "wrong".to_string(),
            worker_claim_id: "wrong".to_string(),
            builder_id: "builder-sugamo-1".to_string(),
            slot_id: "slot-3".to_string(),
            session_id: "sess_01J1Y9".to_string(),
            guest_port: 8000,
        }
    }

    fn candidate_report() -> CandidateReportRequest {
        CandidateReportRequest {
            submission_attempt_id: "wrong".to_string(),
            worker_claim_id: "wrong".to_string(),
            capture_epoch: 3,
            candidate_id: "cand_01J1Z0".to_string(),
            execution_id: format!("blake3:{}", "a".repeat(64)),
            snapshot_id: format!("blake3:{}", "d".repeat(64)),
            artifact_location: "r2://snapshots/cand_01J1Z0/seal".to_string(),
            source_lost: false,
        }
    }

    fn acceptance() -> CandidateAcceptanceRequest {
        CandidateAcceptanceRequest {
            submission_attempt_id: "wrong".to_string(),
            worker_claim_id: "wrong".to_string(),
            capture_epoch: 3,
            status: AcceptanceStatus::Accepted,
            acceptance_receipt: Some(AcceptanceReceipt {
                receipt_schema: AcceptanceReceiptSchema,
                receipt: serde_json::Map::new(),
            }),
            failure_reason: None,
        }
    }

    fn terminal_ack() -> WizardTerminalAck {
        WizardTerminalAck {
            agent_id: "builder-sugamo-1".to_string(),
            submission_attempt_id: "wrong".to_string(),
            worker_claim_id: "wrong".to_string(),
            reason: TerminalAckReason::AttemptEnded,
            failure_stage: None,
            failure_reason: None,
        }
    }

    fn command() -> CaptureCommand {
        CaptureCommand {
            candidate_id: "cand_01J1Z0".to_string(),
            capture_epoch: 3,
        }
    }

    fn capture_control(epoch: u64) -> Value {
        json!({
            "directive": "capture",
            "server_capture_epoch": epoch,
            "candidate_id": "cand_01J1Z0",
            "pause_permitted": true
        })
    }

    /// Every builder-lane section paired with its WIRE-SECTION name — the name
    /// its errors must carry. Used by the checks that must hold for ALL of them
    /// (the `job_id` path gate), so a new section cannot quietly opt out.
    #[allow(clippy::type_complexity)]
    fn interactive_calls() -> Vec<(
        &'static str,
        Box<dyn Fn(&HttpWizardApi<RecordingTransport>, &Fencing4) -> Result<(), WizardApiError>>,
    )> {
        vec![
            (
                "lease renew",
                Box::new(|api, f| api.renew_lease(f).map(|_| ())),
            ),
            (
                "control poll",
                Box::new(|api, f| api.poll_control(f, 0).map(|_| ())),
            ),
            (
                "progress",
                Box::new(|api, f| api.report_progress(f, WizardStage::Holding)),
            ),
            (
                "hold-ready",
                Box::new(|api, f| api.report_hold_ready(f, &hold_ready())),
            ),
            (
                "candidate report",
                Box::new(|api, f| {
                    api.report_candidate(f, &command(), &candidate_report())
                        .map(|_| ())
                }),
            ),
            (
                "candidate acceptance",
                Box::new(|api, f| {
                    api.report_candidate_acceptance(f, &command(), &acceptance())
                        .map(|_| ())
                }),
            ),
            (
                "wizard terminal ack",
                Box::new(|api, f| api.wizard_terminal_ack(f, &terminal_ack())),
            ),
        ]
    }

    /// Drive every builder-lane endpoint once against one recording transport.
    fn drive_every_endpoint() -> RecordingTransport {
        let transport = RecordingTransport::new(vec![
            Ok(HttpResponse {
                status: 200,
                body: json!({ "lease_expires_at": "2026-07-22T09:20:00.000Z" }).to_string(),
            }),
            Ok(HttpResponse {
                status: 200,
                body: capture_control(3).to_string(),
            }),
            Ok(HttpResponse {
                status: 200,
                body: json!({}).to_string(),
            }),
            Ok(HttpResponse {
                status: 200,
                body: json!({}).to_string(),
            }),
            Ok(HttpResponse {
                status: 200,
                body: json!({ "candidate_id": "cand_01J1Z0", "status": "reported" }).to_string(),
            }),
            Ok(HttpResponse {
                status: 200,
                body: json!({ "candidate_id": "cand_01J1Z0", "status": "accepted" }).to_string(),
            }),
            Ok(HttpResponse {
                status: 200,
                body: json!({}).to_string(),
            }),
        ]);
        let api = client(transport);
        let f = fencing();
        api.renew_lease(&f).expect("renew");
        api.poll_control(&f, 0).expect("control");
        api.report_progress(&f, WizardStage::Holding)
            .expect("progress");
        api.report_hold_ready(&f, &hold_ready())
            .expect("hold-ready");
        api.report_candidate(&f, &command(), &candidate_report())
            .expect("candidate report");
        api.report_candidate_acceptance(&f, &command(), &acceptance())
            .expect("acceptance");
        api.wizard_terminal_ack(&f, &terminal_ack()).expect("ack");
        api.transport
    }

    // ── FENCING-4 transport split (§1.1, D2) ────────────────────────────────

    #[test]
    fn lease_token_rides_the_header_and_nothing_else() {
        let transport = drive_every_endpoint();
        let requests = transport.requests();
        assert_eq!(requests.len(), 7, "one request per wire section");
        for request in &requests {
            assert_eq!(
                header(request, LEASE_TOKEN_HEADER),
                Some(LEASE_SECRET),
                "every builder-lane request carries the lease token header"
            );
            assert!(
                !request.url.contains(LEASE_SECRET),
                "the token must never reach a URL (access logs): {}",
                request.url
            );
            if let Some(body) = &request.body {
                assert!(
                    !body.contains(LEASE_SECRET),
                    "the token must never reach a body (body-logging pipelines): {body}"
                );
                assert!(
                    !body.contains("lease_token"),
                    "the strict bodies reject a lease_token KEY too: {body}"
                );
            }
        }
    }

    #[test]
    fn the_control_get_carries_the_other_three_in_the_query() {
        let transport = drive_every_endpoint();
        let control = transport
            .requests()
            .into_iter()
            .find(|request| request.method == "GET")
            .expect("the control poll is the only GET");
        let (path, query) = control
            .url
            .split_once('?')
            .expect("control carries a query");
        // job_id rides the PATH only.
        assert_eq!(
            path,
            "https://api.example/v1/capsule-snapshots/jobs/job_01J1XY/control"
        );
        assert!(
            !query.contains("job_id"),
            "job_id never repeats in the query"
        );
        let mut params: Vec<&str> = query.split('&').collect();
        params.sort_unstable();
        assert_eq!(
            params,
            vec![
                "observed_capture_epoch=0",
                "submission_attempt_id=subatt_01J1XY",
                "worker_claim_id=claim_01J1XZ",
            ]
        );
        assert!(control.body.is_none(), "a GET carries no body");
    }

    #[test]
    fn posts_carry_the_fencing_ids_in_the_body_from_the_tuple() {
        // Every fixture body above deliberately carries "wrong" ids: the client
        // stamps the FENCING-4 tuple over them, so a body can never disagree
        // with the identity the request is fenced under.
        let transport = drive_every_endpoint();
        for request in transport.requests().iter().filter(|r| r.method == "POST") {
            let body = body_json(request);
            assert_eq!(body["submission_attempt_id"], json!("subatt_01J1XY"));
            assert_eq!(body["worker_claim_id"], json!("claim_01J1XZ"));
            assert!(
                body.get("job_id").is_none(),
                "job_id rides the path, never a body: {body}"
            );
        }
    }

    #[test]
    fn every_endpoint_hits_its_spec_url() {
        let transport = drive_every_endpoint();
        let urls: Vec<String> = transport
            .requests()
            .iter()
            .map(|request| {
                format!(
                    "{} {}",
                    request.method,
                    request.url.split('?').next().unwrap_or_default()
                )
            })
            .collect();
        let base = "https://api.example/v1/capsule-snapshots/jobs/job_01J1XY";
        assert_eq!(
            urls,
            vec![
                format!("POST {base}/lease/renew"),
                format!("GET {base}/control"),
                format!("POST {base}/progress"),
                format!("POST {base}/hold-ready"),
                format!("POST {base}/candidates"),
                format!("POST {base}/candidates/cand_01J1Z0/acceptance"),
                format!("POST {base}/ack"),
            ]
        );
    }

    #[test]
    fn the_wizard_terminal_ack_has_no_status_field() {
        // §3.8: the legacy sealed/failed ack body is a schema reject for this
        // job kind — the wizard payload has no `status` member at all, and it
        // DOES carry the fencing ids the legacy body lacks.
        let transport = drive_every_endpoint();
        let ack = transport
            .requests()
            .into_iter()
            .find(|request| request.url.ends_with("/ack"))
            .expect("terminal ack was sent");
        let body = body_json(&ack);
        let mut keys: Vec<&str> = body
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "agent_id",
                "reason",
                "submission_attempt_id",
                "worker_claim_id"
            ],
            "absent optionals are OMITTED, and there is no status/accepted_candidate_id"
        );
        assert_eq!(body["reason"], json!("attempt_ended"));
    }

    #[test]
    fn secrets_never_render_through_debug_or_display() {
        let transport = drive_every_endpoint();
        for request in transport.requests() {
            let rendered = format!("{request:?}");
            assert!(!rendered.contains(LEASE_SECRET), "{rendered}");
            assert!(!rendered.contains(AGENT_TOKEN), "{rendered}");
            assert!(rendered.contains("<redacted>"));
        }
        let api = client(RecordingTransport::new(vec![]));
        assert!(!format!("{api:?}").contains(AGENT_TOKEN));
        // …and an error carrying a server body still cannot carry the token,
        // because nothing in this client writes it anywhere but the header.
        let err = WizardApiError::Status {
            endpoint: "control poll".to_string(),
            code: 500,
            body: "boom".to_string(),
        };
        assert!(!format!("{err}").contains(LEASE_SECRET));
        assert!(!format!("{:?}", fencing()).contains(LEASE_SECRET));
    }

    // ── §1.3 mandatory epoch contract tests ─────────────────────────────────

    #[test]
    fn epoch_a_server_ahead_of_the_observed_epoch_is_accepted() {
        // (a) server advances 0→1 while the builder polls with observed 0: the
        // response epoch MAY differ from the observed one — that is exactly how
        // a capture command is delivered.
        let api = client(RecordingTransport::always_ok(capture_control(1), 1));
        let response = api.poll_control(&fencing(), 0).expect("observed <= server");
        assert_eq!(response.server_capture_epoch, 1);
        assert_eq!(response.directive, ControlDirective::Capture);
    }

    #[test]
    fn epoch_b_an_observed_epoch_ahead_of_the_server_is_a_fault() {
        // (b) observed 2 while the server is at 1: a builder cannot have
        // observed the future. Server-side this is a 409; this side refuses to
        // act on the response either way.
        let api = client(RecordingTransport::always_ok(capture_control(1), 1));
        let err = api
            .poll_control(&fencing(), 2)
            .expect_err("observed > server");
        assert!(
            matches!(err, WizardApiError::Contract { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("ahead of the server epoch"));
    }

    #[test]
    fn epoch_c_a_report_for_a_superseded_epoch_never_leaves() {
        // (c) a report whose capture_epoch is not the candidate's is a
        // `409 fenced` server-side — refuse it before spending the round trip.
        let transport = RecordingTransport::new(vec![]);
        let api = client(transport);
        let mut stale = candidate_report();
        stale.capture_epoch = 2; // the command below names epoch 3
        let err = api
            .report_candidate(&fencing(), &command(), &stale)
            .expect_err("superseded epoch");
        assert!(
            matches!(err, WizardApiError::Contract { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("does not match the candidate"));
        assert!(
            api.transport.requests().is_empty(),
            "nothing left the process"
        );
    }

    #[test]
    fn epoch_d_a_report_at_the_candidates_epoch_is_sent() {
        // (d) the matching epoch is accepted and reaches the wire.
        let api = client(RecordingTransport::always_ok(
            json!({ "candidate_id": "cand_01J1Z0", "status": "reported" }),
            1,
        ));
        let reported = api
            .report_candidate(&fencing(), &command(), &candidate_report())
            .expect("matching epoch");
        assert_eq!(reported.status, CandidateStatus::Reported);
        assert_eq!(api.transport.requests().len(), 1);
    }

    #[test]
    fn epoch_zero_is_a_schema_floor_on_the_report() {
        // §1.3 (c), second half: capture_epoch 0 is a schema reject on
        // report/acceptance regardless of the candidate.
        let api = client(RecordingTransport::new(vec![]));
        let mut zero = candidate_report();
        zero.capture_epoch = 0;
        let err = api
            .report_candidate(
                &fencing(),
                &CaptureCommand {
                    candidate_id: "cand_01J1Z0".to_string(),
                    capture_epoch: 0,
                },
                &zero,
            )
            .expect_err("epoch floor is 1");
        assert!(err.to_string().contains("capture_epoch >= 1"));
    }

    #[test]
    fn a_report_naming_a_candidate_the_control_channel_never_delivered_never_leaves() {
        // §1.2's OTHER half: epoch ↔ candidate is 1:1, so a report at the right
        // epoch naming a DIFFERENT candidate is just as fenced server-side as a
        // superseded epoch. The pre-flight exists to save that round trip.
        let api = client(RecordingTransport::new(vec![]));
        let mut impostor = candidate_report();
        impostor.candidate_id = "cand_SOMEONE_ELSE".to_string();
        let err = api
            .report_candidate(&fencing(), &command(), &impostor)
            .expect_err("a candidate the control channel never delivered");
        assert!(
            matches!(err, WizardApiError::Contract { .. }),
            "got {err:?}"
        );
        assert!(
            err.to_string()
                .contains("cand_SOMEONE_ELSE is not the candidate the control channel delivered"),
            "{err}"
        );
        assert!(
            api.transport.requests().is_empty(),
            "nothing left the process"
        );
    }

    #[test]
    fn an_acceptance_for_a_superseded_epoch_never_leaves() {
        // The acceptance path takes its candidate FROM the command (path-only,
        // never caller-supplied), so the epoch is the one caller-supplied half
        // of the pairing — and it is still checked.
        let api = client(RecordingTransport::new(vec![]));
        let mut stale = acceptance();
        stale.capture_epoch = 2; // the command below names epoch 3
        let err = api
            .report_candidate_acceptance(&fencing(), &command(), &stale)
            .expect_err("superseded epoch");
        assert!(
            err.to_string().contains("does not match the candidate"),
            "{err}"
        );
        assert_eq!(err.endpoint(), "candidate acceptance");
        assert!(api.transport.requests().is_empty());
    }

    // ── request pre-flight validation (§3.5 / §3.7 / §3.8) ──────────────────

    #[test]
    fn every_request_body_is_validated_before_it_leaves() {
        // The strict wire types carry refinements the schema cannot express, and
        // the api enforces every one of them. Sending a body that is knowably
        // invalid spends a round trip UNDER A LIVE LEASE to be told so — each
        // section must refuse locally instead.
        //
        // §3.5: a port of 0 is not a port.
        let api = client(RecordingTransport::new(vec![]));
        let mut portless = hold_ready();
        portless.guest_port = 0;
        let err = api
            .report_hold_ready(&fencing(), &portless)
            .expect_err("guest_port 1..65535");
        assert!(err.to_string().contains("guest_port"), "{err}");
        assert_eq!(err.endpoint(), "hold-ready");
        assert!(api.transport.requests().is_empty());

        // §3.7: `accepted` and a failure_reason are mutually exclusive — the
        // pair would assert an acceptance that also failed.
        let api = client(RecordingTransport::new(vec![]));
        let mut contradictory = acceptance();
        contradictory.failure_reason = Some("rejected after all".to_string());
        let err = api
            .report_candidate_acceptance(&fencing(), &command(), &contradictory)
            .expect_err("failure_reason is only legal with rejected");
        assert!(err.to_string().contains("failure_reason"), "{err}");
        assert_eq!(err.endpoint(), "candidate acceptance");
        assert!(api.transport.requests().is_empty());

        // §3.8: the terminal ack identifies the builder; an empty agent_id
        // leaves an admin a terminal state with no agent to attribute it to.
        let api = client(RecordingTransport::new(vec![]));
        let mut anonymous = terminal_ack();
        anonymous.agent_id = String::new();
        let err = api
            .wizard_terminal_ack(&fencing(), &anonymous)
            .expect_err("agent_id 1..120");
        assert!(err.to_string().contains("agent_id"), "{err}");
        assert_eq!(err.endpoint(), "wizard terminal ack");
        assert!(api.transport.requests().is_empty());
    }

    // ── control response validation + fencing classification ────────────────

    #[test]
    fn a_capture_directive_without_a_candidate_never_reaches_the_capture_seam() {
        // HoldPhase does not call `validate()`; this client does, so a malformed
        // capture command is a fault here instead of a capture against nothing.
        let api = client(RecordingTransport::always_ok(
            json!({ "directive": "capture", "server_capture_epoch": 1, "pause_permitted": true }),
            1,
        ));
        let err = api
            .poll_control(&fencing(), 0)
            .expect_err("capture requires candidate_id");
        assert!(err.to_string().contains("requires candidate_id"));
    }

    #[test]
    fn a_fenced_409_is_classified_as_fenced_and_anything_else_is_not() {
        let api = client(RecordingTransport::new(vec![
            Ok(HttpResponse {
                status: 409,
                body: json!({ "error": "fenced", "message": "claim is not active" }).to_string(),
            }),
            Ok(HttpResponse {
                status: 409,
                body: json!({ "error": "not_claimed", "message": "state changed" }).to_string(),
            }),
        ]));
        let fenced = api.renew_lease(&fencing()).expect_err("409 fenced");
        assert!(fenced.is_fenced(), "got {fenced:?}");
        let other = api.renew_lease(&fencing()).expect_err("409 other");
        assert!(
            !other.is_fenced(),
            "not every 409 is a fence — only the §1 envelope: {other:?}"
        );
    }

    #[test]
    fn a_url_unsafe_fencing_id_fails_closed_instead_of_being_guessed() {
        let api = client(RecordingTransport::new(vec![]));
        let mut hostile = fencing();
        hostile.worker_claim_id = "claim_1&observed_capture_epoch=99".to_string();
        let err = api
            .poll_control(&hostile, 0)
            .expect_err("query smuggling is refused");
        assert!(err.to_string().contains("not URL-safe"));
        assert!(api.transport.requests().is_empty());
    }

    #[test]
    fn a_url_unsafe_job_id_never_reaches_the_request_line() {
        // The claim's `job_id` rides the PATH of EVERY builder-lane call, so a
        // skewed or hostile id is path traversal / route substitution, not a
        // query smuggle: `job_1/../../other` pasted unencoded reaches a
        // different route entirely. Every section is checked, not just the GET.
        let mut hostile = fencing();
        hostile.job_id = "job_1/../../other".to_string();
        for (section, call) in interactive_calls() {
            let api = client(RecordingTransport::new(vec![]));
            let err = call(&api, &hostile).expect_err("a traversing job_id fails closed");
            assert!(
                err.to_string().contains("job_id is not URL-safe"),
                "{section}: {err}"
            );
            // The error names its WIRE SECTION, never the path fragment it was
            // building — `/control: job_id is not URL-safe` names nothing an
            // operator can look up in the contract.
            assert_eq!(err.endpoint(), section, "got {err}");
            assert!(
                api.transport.requests().is_empty(),
                "{section}: nothing left the process"
            );
        }
    }

    #[test]
    fn a_job_id_carrying_a_query_separator_fails_closed_too() {
        // `?`/`&` in the PATH position silently turns the rest of the URL into a
        // query the api would read as fencing input.
        let mut hostile = fencing();
        hostile.job_id = "job_1?worker_claim_id=someone_else".to_string();
        let api = client(RecordingTransport::new(vec![]));
        let err = api
            .renew_lease(&hostile)
            .expect_err("query smuggling through the path is refused");
        assert!(err.to_string().contains("job_id is not URL-safe"), "{err}");
        assert!(api.transport.requests().is_empty());
    }

    // ── §3.8 helpers ────────────────────────────────────────────────────────

    #[test]
    fn failure_stages_are_parsed_from_the_wire_enum_not_a_local_table() {
        assert_eq!(
            wizard_failure_stage("build"),
            Some(WizardFailureStage::Build)
        );
        assert_eq!(
            wizard_failure_stage("capture_seal"),
            Some(WizardFailureStage::CaptureSeal)
        );
        // A builder stage with no wizard counterpart omits the refinement.
        assert_eq!(wizard_failure_stage("artifact_metadata"), None);
    }

    #[test]
    fn a_build_failure_acks_build_failed_with_the_stage_refinement() {
        let api = client(RecordingTransport::always_ok(json!({}), 1));
        ack_interactive_build_failure(&api, "builder-1", &fencing(), "holding", "boom")
            .expect("ack sent");
        let body = body_json(&api.transport.requests()[0]);
        assert_eq!(body["reason"], json!("build_failed"));
        assert_eq!(body["failure_stage"], json!("holding"));
        assert_eq!(body["failure_reason"], json!("boom"));
        assert!(body.get("status").is_none(), "never the legacy body");
    }

    #[test]
    fn a_hold_torn_down_without_an_ack_sends_nothing() {
        // §3.8: lease expiry is server-owned. There is no legal ack for this
        // outcome, so the helper must send NOTHING — not a best-effort one.
        let api = client(RecordingTransport::new(vec![]));
        ack_hold_termination(
            &api,
            "builder-1",
            &fencing(),
            &HoldTermination::TornDownWithoutAck {
                failure_reason: "fenced".to_string(),
            },
        )
        .expect("no ack is a success");
        assert!(api.transport.requests().is_empty());

        // …while an orderly end DOES ack.
        let api = client(RecordingTransport::always_ok(json!({}), 1));
        ack_hold_termination(
            &api,
            "builder-1",
            &fencing(),
            &HoldTermination::AttemptEnded,
        )
        .expect("ack sent");
        assert_eq!(
            body_json(&api.transport.requests()[0])["reason"],
            json!("attempt_ended")
        );
    }

    #[test]
    fn a_failed_hold_acks_its_diagnostics_not_a_bare_failure() {
        // §3.8's optional members are REFINEMENTS of the reason, and they are
        // the only thing an admin has to tell "ineligible capsule" from "the
        // source was lost mid-acceptance". A bare `build_failed` with neither is
        // an unactionable terminal state.
        let api = client(RecordingTransport::always_ok(json!({}), 1));
        ack_hold_termination(
            &api,
            "builder-1",
            &fencing(),
            &HoldTermination::FailedClosed {
                failure_reason: "capsule requires External State".to_string(),
            },
        )
        .expect("ack sent");
        let body = body_json(&api.transport.requests()[0]);
        assert_eq!(body["reason"], json!("build_failed"));
        assert_eq!(body["failure_stage"], json!("holding"));
        assert_eq!(
            body["failure_reason"],
            json!("capsule requires External State")
        );

        // ADR-012's terminal branch projects to its OWN reason and stage.
        let api = client(RecordingTransport::always_ok(json!({}), 1));
        ack_hold_termination(
            &api,
            "builder-1",
            &fencing(),
            &HoldTermination::AcceptanceFailedSourceLost {
                failure_reason: "resume failed after capture".to_string(),
            },
        )
        .expect("ack sent");
        let body = body_json(&api.transport.requests()[0]);
        assert_eq!(body["reason"], json!("acceptance_failed_source_lost"));
        assert_eq!(body["failure_stage"], json!("acceptance"));
        assert_eq!(body["failure_reason"], json!("resume failed after capture"));
    }

    #[test]
    fn a_diagnostic_is_truncated_to_the_builders_failure_reason_budget() {
        // The api bounds `failure_reason` at 2000 UTF-16 code units and REJECTS
        // an over-long ack outright — an unbounded build log spliced into the
        // reason would lose the whole terminal ack, not just the tail.
        let api = client(RecordingTransport::always_ok(json!({}), 1));
        let flood = "x".repeat(FAILURE_REASON_BUDGET + 500);
        ack_interactive_build_failure(&api, "builder-1", &fencing(), "holding", &flood)
            .expect("ack sent");
        let body = body_json(&api.transport.requests()[0]);
        assert_eq!(
            body["failure_reason"]
                .as_str()
                .expect("string")
                .chars()
                .count(),
            FAILURE_REASON_BUDGET
        );

        // …and the same budget bounds a hold's own diagnostic.
        let api = client(RecordingTransport::always_ok(json!({}), 1));
        ack_hold_termination(
            &api,
            "builder-1",
            &fencing(),
            &HoldTermination::FailedClosed {
                failure_reason: flood.clone(),
            },
        )
        .expect("ack sent");
        let body = body_json(&api.transport.requests()[0]);
        assert_eq!(
            body["failure_reason"]
                .as_str()
                .expect("string")
                .chars()
                .count(),
            FAILURE_REASON_BUDGET
        );
    }

    #[test]
    fn a_diagnostic_is_truncated_in_the_unit_the_wire_bound_is_counted_in() {
        // The truncator and the validator have to measure the SAME thing. The
        // bound is 2000 UTF-16 code units (zod counts `String.length`), so a
        // truncator counting scalars keeps 1800 astral emoji — 3600 code units,
        // over the bound. `validate()` then refuses the body and the terminal ack
        // is never sent AT ALL: the job sits in `holding` until the server sweep
        // and the author's failure has no diagnostic anywhere. Emoji in a
        // Node/Vite build log is the ordinary case, not a contrived one.
        let api = client(RecordingTransport::always_ok(json!({}), 1));
        let astral = "🙂".repeat(FAILURE_REASON_BUDGET);
        assert_eq!(astral.chars().count(), FAILURE_REASON_BUDGET);
        ack_interactive_build_failure(&api, "builder-1", &fencing(), "holding", &astral)
            .expect("the ack still leaves");
        let body = body_json(&api.transport.requests()[0]);
        let sent = body["failure_reason"].as_str().expect("string");
        assert_eq!(
            sent.encode_utf16().count(),
            FAILURE_REASON_BUDGET,
            "the budget is spent in code units, the unit the api measures"
        );
        // …and never mid-scalar: the result is still the emoji, just fewer.
        assert_eq!(sent, "🙂".repeat(FAILURE_REASON_BUDGET / 2));
    }

    #[test]
    fn a_rejected_acceptance_truncates_its_diagnostic_like_the_terminal_ack_does() {
        // §3.7 bounds `failure_reason` exactly as §3.8 does. Untruncated, an
        // acceptance rejection carrying a real verification log fails its own
        // `validate()` — a LOCAL error, so the api is never told the candidate was
        // rejected and the attempt waits on the sweep instead.
        let api = client(RecordingTransport::always_ok(
            json!({ "candidate_id": "cand_01J1Z0", "status": "rejected" }),
            1,
        ));
        let flood = "e".repeat(FAILURE_REASON_BUDGET + 700);
        api.report_candidate_acceptance(
            &fencing(),
            &command(),
            &CandidateAcceptanceRequest {
                status: AcceptanceStatus::Rejected,
                acceptance_receipt: None,
                failure_reason: Some(flood),
                ..acceptance()
            },
        )
        .expect("the rejection report still leaves");
        let body = body_json(&api.transport.requests()[0]);
        assert_eq!(
            body["failure_reason"]
                .as_str()
                .expect("string")
                .encode_utf16()
                .count(),
            FAILURE_REASON_BUDGET
        );
    }

    #[test]
    fn a_server_error_body_is_bounded_before_it_becomes_a_local_error() {
        // An error body is non-secret server output, but it is still unbounded
        // REMOTE input: an api that answered a 500 with a megabyte of HTML would
        // otherwise carry all of it into every log line the error touches.
        let flood = "e".repeat(ERROR_BODY_BUDGET + 500);
        let api = client(RecordingTransport::new(vec![Ok(HttpResponse {
            status: 500,
            body: flood,
        })]));
        let err = api.renew_lease(&fencing()).expect_err("500");
        let WizardApiError::Status { body, .. } = &err else {
            panic!("got {err:?}");
        };
        assert_eq!(body.chars().count(), ERROR_BODY_BUDGET);
    }

    #[test]
    fn a_2xx_that_does_not_match_the_wire_contract_is_a_contract_fault() {
        // A 200 is not agreement: an api that answered the renew with a body
        // this side cannot read has skewed the contract, and inventing a
        // deadline out of it would be exactly the "assume far away" the driver
        // refuses. It must surface as a CONTRACT fault, not a silent default.
        let api = client(RecordingTransport::new(vec![Ok(HttpResponse {
            status: 200,
            body: json!({ "lease_expires_at": 1234 }).to_string(),
        })]));
        let err = api.renew_lease(&fencing()).expect_err("skewed body");
        assert!(
            matches!(err, WizardApiError::Contract { .. }),
            "got {err:?}"
        );
        assert!(
            err.to_string()
                .contains("response did not match the wire contract"),
            "{err}"
        );
        assert_eq!(err.endpoint(), "lease renew");
    }

    #[test]
    fn the_acceptance_receipt_envelope_reaches_the_wire_intact() {
        let api = client(RecordingTransport::always_ok(
            json!({ "candidate_id": "cand_01J1Z0", "status": "accepted" }),
            1,
        ));
        api.report_candidate_acceptance(&fencing(), &command(), &acceptance())
            .expect("acceptance");
        let body = body_json(&api.transport.requests()[0]);
        assert_eq!(
            body["acceptance_receipt"]["receipt_schema"],
            json!(ACCEPTANCE_RECEIPT_SCHEMA)
        );
        assert!(body.get("failure_reason").is_none(), "omitted, never null");
    }

    // ── the production transport ────────────────────────────────────────────

    /// A loopback HTTP server that serves `responses` in order and then stops.
    /// The only way to exercise [`UreqTransport`] — the production transport
    /// every other test replaces — including the load-bearing
    /// `ureq::Error::Status` → `Ok(HttpResponse)` mapping that makes a
    /// `409 fenced` a classifiable RESPONSE instead of a transport error.
    fn loopback(responses: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().expect("local addr"));
        let server = std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                read_whole_request(&mut stream);
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            }
        });
        (base, server)
    }

    /// Read one WHOLE HTTP/1.1 request — the head, then exactly `content-length`
    /// more bytes — before the fixture answers.
    ///
    /// Not pedantry: closing a socket whose receive buffer still holds unread
    /// bytes sends an RST, and an RST discards the response already queued on
    /// that socket. The client then fails reading the STATUS LINE instead of
    /// seeing the scripted status, so the outcome depends on how the request
    /// happened to be split into packets — a deterministic test that fails
    /// occasionally under load. Draining the request first makes the close a
    /// clean FIN.
    fn read_whole_request(stream: &mut std::net::TcpStream) {
        let mut request: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            if let Some(head_end) = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|at| at + 4)
            {
                let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
                let body_len: usize = head
                    .split("content-length:")
                    .nth(1)
                    .and_then(|rest| rest.split("\r\n").next())
                    .and_then(|value| value.trim().parse().ok())
                    .unwrap_or(0);
                if request.len() >= head_end + body_len {
                    return;
                }
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(read) => request.extend_from_slice(&chunk[..read]),
            }
        }
    }

    #[test]
    fn the_production_transport_makes_a_status_a_response_not_a_transport_error() {
        // A `409 fenced` MUST reach `classify` with its body intact: ureq
        // surfaces every non-2xx as `Err(Error::Status)`, and letting that stand
        // as a transport error would make the fenced envelope unreadable — every
        // fail-closed decision on this lane depends on telling the two apart.
        let (base, server) = loopback(vec![
            (409, r#"{"error":"fenced","message":"claim is not active"}"#),
            (200, r#"{"lease_expires_at":"2026-07-22T09:20:00.000Z"}"#),
        ]);
        let api = HttpWizardApi::new(base, AGENT_TOKEN.to_string(), UreqTransport::new());
        let fenced = api.renew_lease(&fencing()).expect_err("409 fenced");
        assert!(fenced.is_fenced(), "got {fenced:?}");
        assert!(
            fenced.to_string().contains("claim is not active"),
            "{fenced}"
        );
        let renewed = api.renew_lease(&fencing()).expect("200 over the wire");
        assert_eq!(renewed.lease_expires_at, "2026-07-22T09:20:00.000Z");
        server.join().expect("loopback server");
    }

    #[test]
    fn the_production_transport_reports_a_real_failure_as_a_transport_error() {
        // Bind then drop, so the port is guaranteed to refuse rather than
        // belong to someone else.
        let dead = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = dead.local_addr().expect("local addr");
        drop(dead);
        let api = HttpWizardApi::new(
            format!("http://{addr}"),
            AGENT_TOKEN.to_string(),
            UreqTransport::new(),
        );
        let err = api.renew_lease(&fencing()).expect_err("refused");
        assert!(
            matches!(err, WizardApiError::Transport { .. }),
            "got {err:?}"
        );

        // …and a method this transport cannot send is refused locally rather
        // than silently downgraded to one it can.
        let unsupported = UreqTransport::new()
            .execute(&HttpRequest {
                method: "PUT",
                url: format!("http://{addr}/x"),
                headers: Vec::new(),
                body: None,
            })
            .expect_err("PUT");
        assert!(
            unsupported.contains("unsupported method PUT"),
            "{unsupported}"
        );
    }

    // ── lease renew driver ──────────────────────────────────────────────────

    /// A settable wall clock (the deadline is a wall-clock instant, so this is
    /// the clock the driver is tested on).
    struct FakeWallClock {
        now: Mutex<SystemTime>,
        slept: Mutex<Duration>,
    }

    impl FakeWallClock {
        fn at(iso: &str) -> Self {
            FakeWallClock {
                now: Mutex::new(parse_utc_instant(iso).expect("fixture instant")),
                slept: Mutex::new(Duration::ZERO),
            }
        }

        fn advance(&self, by: Duration) {
            let mut now = self.now.lock().unwrap();
            *now += by;
        }

        fn slept(&self) -> Duration {
            *self.slept.lock().unwrap()
        }
    }

    impl WallClock for FakeWallClock {
        fn now_utc(&self) -> SystemTime {
            *self.now.lock().unwrap()
        }

        /// Time passes without being spent: a backoff bounded by a wall-clock
        /// deadline is only testable if the fake advances instead of blocking.
        fn sleep(&self, duration: Duration) {
            *self.slept.lock().unwrap() += duration;
            self.advance(duration);
        }
    }

    /// A scripted [`WizardApi`] for the driver / control-source tests — the
    /// SEMANTIC seam, with no sockets and no request bytes.
    struct ScriptedApi {
        renewals: Mutex<VecDeque<Result<LeaseRenewResponse, WizardApiError>>>,
        renew_calls: Mutex<u32>,
        controls: Mutex<VecDeque<Result<ControlResponse, WizardApiError>>>,
        control_calls: Mutex<u32>,
        acks: Mutex<Vec<WizardTerminalAck>>,
        /// Every §3.6 that actually left, with the pairing it was sent under —
        /// the pairing is the point, so it is recorded, not just counted.
        candidate_reports: Mutex<Vec<(CaptureCommand, CandidateReportRequest)>>,
        acceptances: Mutex<Vec<(CaptureCommand, CandidateAcceptanceRequest)>>,
    }

    impl ScriptedApi {
        fn new(
            renewals: Vec<Result<LeaseRenewResponse, WizardApiError>>,
            controls: Vec<Result<ControlResponse, WizardApiError>>,
        ) -> Self {
            ScriptedApi {
                renewals: Mutex::new(renewals.into()),
                renew_calls: Mutex::new(0),
                controls: Mutex::new(controls.into()),
                control_calls: Mutex::new(0),
                acks: Mutex::new(Vec::new()),
                candidate_reports: Mutex::new(Vec::new()),
                acceptances: Mutex::new(Vec::new()),
            }
        }

        fn renew_calls(&self) -> u32 {
            *self.renew_calls.lock().unwrap()
        }

        fn control_calls(&self) -> u32 {
            *self.control_calls.lock().unwrap()
        }

        /// `n` transport failures of the same shape an api mid-redeploy produces:
        /// the connection is refused, and the refusal comes back in about a
        /// millisecond — which is exactly why a retry budget counted in ATTEMPTS
        /// is spent long before the outage is.
        fn refused_polls(n: usize) -> Vec<Result<ControlResponse, WizardApiError>> {
            (0..n)
                .map(|_| {
                    Err(WizardApiError::Transport {
                        endpoint: "control poll".to_string(),
                        message: "connection refused".to_string(),
                    })
                })
                .collect()
        }

        /// `n` DETERMINISTIC control faults of the shape a builder/api version
        /// skew produces: a `200` whose body this builder cannot parse, or a
        /// refinement/epoch breach — all [`WizardApiError::Contract`]. Unlike a
        /// refused connection these answer identically on every retry, so a loop
        /// that treats them as a transient blip never exits.
        fn contract_polls(n: usize) -> Vec<Result<ControlResponse, WizardApiError>> {
            (0..n)
                .map(|_| {
                    Err(WizardApiError::Contract {
                        endpoint: "control poll".to_string(),
                        message: "response did not match the wire contract".to_string(),
                    })
                })
                .collect()
        }
    }

    fn renewed(iso: &str) -> Result<LeaseRenewResponse, WizardApiError> {
        Ok(LeaseRenewResponse {
            lease_expires_at: iso.to_string(),
        })
    }

    impl WizardApi for ScriptedApi {
        fn renew_lease(&self, _fencing: &Fencing4) -> Result<LeaseRenewResponse, WizardApiError> {
            *self.renew_calls.lock().unwrap() += 1;
            self.renewals
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("unscripted lease renew"))
        }

        fn poll_control(
            &self,
            _fencing: &Fencing4,
            _observed_capture_epoch: u64,
        ) -> Result<ControlResponse, WizardApiError> {
            *self.control_calls.lock().unwrap() += 1;
            self.controls
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("unscripted control poll"))
        }

        fn report_progress(
            &self,
            _fencing: &Fencing4,
            _stage: WizardStage,
        ) -> Result<(), WizardApiError> {
            panic!("unscripted progress")
        }

        fn report_hold_ready(
            &self,
            _fencing: &Fencing4,
            _request: &HoldReadyRequest,
        ) -> Result<(), WizardApiError> {
            panic!("unscripted hold-ready")
        }

        fn report_candidate(
            &self,
            _fencing: &Fencing4,
            command: &CaptureCommand,
            request: &CandidateReportRequest,
        ) -> Result<CandidateReportResponse, WizardApiError> {
            self.candidate_reports
                .lock()
                .unwrap()
                .push((command.clone(), request.clone()));
            Ok(CandidateReportResponse {
                candidate_id: request.candidate_id.clone(),
                status: CandidateStatus::Reported,
            })
        }

        fn report_candidate_acceptance(
            &self,
            _fencing: &Fencing4,
            command: &CaptureCommand,
            request: &CandidateAcceptanceRequest,
        ) -> Result<CandidateAcceptanceResponse, WizardApiError> {
            self.acceptances
                .lock()
                .unwrap()
                .push((command.clone(), request.clone()));
            Ok(CandidateAcceptanceResponse {
                candidate_id: command.candidate_id.clone(),
                status: request.status,
            })
        }

        fn wizard_terminal_ack(
            &self,
            _fencing: &Fencing4,
            ack: &WizardTerminalAck,
        ) -> Result<(), WizardApiError> {
            self.acks.lock().unwrap().push(ack.clone());
            Ok(())
        }
    }

    #[test]
    fn the_renew_cadence_is_derived_from_the_observed_lease_ttl() {
        // Not a hardcoded api constant: a third of whatever window the server
        // reported, so two consecutive failures still leave a try.
        assert_eq!(
            renew_interval(Duration::from_secs(300)),
            Duration::from_secs(100)
        );
        assert_eq!(
            renew_interval(Duration::from_secs(60)),
            Duration::from_secs(20)
        );
    }

    #[test]
    fn the_lease_is_renewed_before_it_expires() {
        // Claim mints a 300s lease at 09:10. Renews land at 09:11:40 and
        // 09:13:20 — both strictly before the original 09:15 deadline, which is
        // the whole point: a 30-minute hold outlives many lease windows.
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            vec![
                renewed("2026-07-22T09:16:40.000Z"),
                renewed("2026-07-22T09:18:20.000Z"),
                renewed("2026-07-22T09:23:20.000Z"),
            ],
            vec![],
        );
        let mut driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("lease adopted");
        let f = fencing();

        driver.tick(&f).expect("healthy");
        assert_eq!(driver.renews(), 0, "no renew before the cadence says so");

        clock.advance(Duration::from_secs(100));
        driver.tick(&f).expect("healthy");
        assert_eq!(driver.renews(), 1, "renewed at a third of the TTL");

        clock.advance(Duration::from_secs(99));
        driver.tick(&f).expect("healthy");
        assert_eq!(
            driver.renews(),
            1,
            "cadence re-derived from the NEW deadline"
        );

        clock.advance(Duration::from_secs(1));
        driver.tick(&f).expect("healthy");
        assert_eq!(driver.renews(), 2);

        // Past the ORIGINAL 09:15 deadline, still holding — because the lease
        // moved. This is the property a 30-minute hold depends on.
        clock.advance(Duration::from_secs(200));
        driver
            .tick(&f)
            .expect("the lease outlived its first window");
        assert_eq!(driver.renews(), 3);
    }

    #[test]
    fn the_cadence_follows_a_renewed_leases_own_ttl_not_the_first_one() {
        // The distinguishing property: while every window is the same length, a
        // cadence DERIVED from the observed TTL and a hardcoded constant are
        // indistinguishable. Renew into a SHORTER window and the next renew has
        // to move with it — a driver still pacing on the first window's third
        // would let a shortened lease die un-renewed.
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            vec![
                // Renewed at 09:11:40 into a 60s window (not another 300s one).
                renewed("2026-07-22T09:12:40.000Z"),
                renewed("2026-07-22T09:20:00.000Z"),
            ],
            vec![],
        );
        let mut driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("lease adopted");
        let f = fencing();
        clock.advance(Duration::from_secs(100)); // a third of the FIRST window
        driver.tick(&f).expect("healthy");
        assert_eq!(driver.renews(), 1);
        clock.advance(Duration::from_secs(20)); // a third of the NEW window
        driver.tick(&f).expect("healthy");
        assert_eq!(
            driver.renews(),
            2,
            "the cadence is re-derived from the renewed lease's own TTL"
        );
    }

    #[test]
    fn the_production_clock_is_the_wall_clock_the_deadline_is_expressed_in() {
        // `lease_expires_at` is a SERVER wall-clock instant. A driver built on a
        // process-local monotonic `Instant` would be comparing it against a
        // clock the api cannot name, so the production clock has to be the wall
        // clock — and the fake above has to be standing in for the same thing.
        let observed = SystemWallClock.now_utc();
        let skew = SystemTime::now()
            .duration_since(observed)
            .expect("the production clock is not ahead of the wall clock");
        assert!(skew < Duration::from_secs(5), "{skew:?}");

        // A lost lease's diagnostic is readable without going through Display
        // (the hold logs the message, the fault wraps the Display form).
        let lost = LeaseLost::Expired {
            message: "the observed lease deadline passed".to_string(),
        };
        assert_eq!(lost.message(), "the observed lease deadline passed");
        assert_eq!(
            ControlFault::from(lost).message,
            "lease expired: the observed lease deadline passed"
        );
    }

    #[test]
    fn a_fenced_renew_fails_closed_immediately() {
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            vec![Err(WizardApiError::Fenced {
                endpoint: "lease renew".to_string(),
                message: "claim is not active".to_string(),
            })],
            vec![],
        );
        let mut driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("lease adopted");
        clock.advance(Duration::from_secs(100));
        let lost = driver.tick(&fencing()).expect_err("fenced");
        assert!(matches!(lost, LeaseLost::Fenced { .. }), "got {lost:?}");
    }

    #[test]
    fn a_transport_failure_retries_but_the_deadline_still_fails_closed() {
        // A refused renew is not instantly terminal — it retries inside the
        // window. What bounds it is the deadline: once that passes with no
        // successful renew the hold STOPS, never "keep going and hope".
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            (0..8)
                .map(|_| {
                    Err(WizardApiError::Transport {
                        endpoint: "lease renew".to_string(),
                        message: "connection reset".to_string(),
                    })
                })
                .collect(),
            vec![],
        );
        let mut driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("lease adopted");
        let f = fencing();
        clock.advance(Duration::from_secs(100));
        driver.tick(&f).expect("deferred, not terminal");
        clock.advance(Duration::from_secs(34));
        driver.tick(&f).expect("retried inside the window");
        assert!(api.renew_calls() >= 2, "the driver actually retried");

        clock.advance(Duration::from_secs(200)); // past 09:15
        let lost = driver.tick(&f).expect_err("deadline is fail-closed");
        assert!(matches!(lost, LeaseLost::Expired { .. }), "got {lost:?}");
    }

    #[test]
    fn an_unparseable_or_past_lease_deadline_is_refused_at_construction() {
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(vec![], vec![]);
        assert!(matches!(
            LeaseRenewDriver::new(&api, &clock, "not-a-timestamp").expect_err("unparseable"),
            LeaseLost::Contract { .. }
        ));
        assert!(matches!(
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:00:00.000Z").expect_err("past"),
            LeaseLost::Expired { .. }
        ));
    }

    // ── production ControlSource ────────────────────────────────────────────

    /// Absent optionals are OMITTED, never `null` (§3 null policy) — an
    /// explicit `null` is a schema reject on both sides, so the fixture has to
    /// build the object the way the api does.
    fn control(directive: &str, epoch: u64, candidate: Option<&str>) -> ControlResponse {
        let mut value = json!({
            "directive": directive,
            "server_capture_epoch": epoch,
            "pause_permitted": true
        });
        let object = value.as_object_mut().expect("object");
        if let Some(candidate) = candidate {
            object.insert("candidate_id".to_string(), json!(candidate));
        }
        if directive == "hold" {
            object.insert(
                "hold_expires_at".to_string(),
                json!("2026-07-22T09:45:00.000Z"),
            );
        }
        serde_json::from_value(value).expect("fixture control response")
    }

    #[test]
    fn the_control_source_renews_the_lease_on_every_poll_and_remembers_the_command() {
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            vec![renewed("2026-07-22T09:16:40.000Z")],
            vec![
                Ok(control("hold", 0, None)),
                Ok(control("capture", 4, Some("cand_01J1Z0"))),
            ],
        );
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);

        let first = source.poll(0).expect("hold");
        assert_eq!(first.directive, ControlDirective::Hold);
        assert!(source.last_capture_command().is_none());
        assert_eq!(api.renew_calls(), 0, "not yet due");

        clock.advance(Duration::from_secs(100));
        let second = source.poll(0).expect("capture");
        assert_eq!(second.server_capture_epoch, 4);
        assert_eq!(api.renew_calls(), 1, "the renew rides the poll");
        assert_eq!(
            source.last_capture_command(),
            Some(&CaptureCommand {
                candidate_id: "cand_01J1Z0".to_string(),
                capture_epoch: 4,
            })
        );
        assert_eq!(source.polls(), 2);
    }

    /// A report is sent under the pairing the CONTROL CHANNEL delivered, not
    /// under one the caller restated.
    ///
    /// §3.6/§3.7 have the server cross-check epoch↔candidate 1:1 against the id
    /// it minted. If the builder supplied both halves of what is being checked,
    /// the check would be the builder agreeing with itself — so the candidate id
    /// comes from this source's own memo of the `capture` directive, and the
    /// caller's epoch only has to MATCH it.
    #[test]
    fn a_report_rides_the_pairing_the_control_channel_delivered() {
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(vec![], vec![Ok(control("capture", 4, Some("cand_01J1Z0")))]);
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);
        source.poll(0).expect("capture directive");

        let mut report = candidate_report();
        report.capture_epoch = 4;
        source
            .report_candidate(&report)
            .expect("the report leaves under the delivered pairing");

        let sent = api.candidate_reports.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].0,
            CaptureCommand {
                candidate_id: "cand_01J1Z0".to_string(),
                capture_epoch: 4,
            },
            "the pairing is the control channel's, not the caller's"
        );
    }

    /// Nothing leaves for an epoch the control channel never delivered a capture
    /// for — the round trip would only come back `409 fenced`, and the builder
    /// already knows enough to say so.
    #[test]
    fn a_report_for_an_epoch_no_capture_command_named_never_leaves() {
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(vec![], vec![Ok(control("capture", 4, Some("cand_01J1Z0")))]);
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);

        // Before any poll there is no pairing at all.
        let mut report = candidate_report();
        report.capture_epoch = 4;
        let fault = source
            .report_candidate(&report)
            .expect_err("no capture command has been delivered");
        assert!(fault.message.contains("no capture command"), "{fault:?}");

        // And after one, an epoch that is not ITS epoch is refused too.
        source.poll(0).expect("capture directive");
        let mut stale = acceptance();
        stale.capture_epoch = 3;
        let fault = source
            .report_acceptance(&stale)
            .expect_err("epoch 3 is not the delivered epoch 4");
        assert!(fault.message.contains("epoch 4"), "{fault:?}");

        assert!(api.candidate_reports.lock().unwrap().is_empty());
        assert!(api.acceptances.lock().unwrap().is_empty());
    }

    /// A body that cannot satisfy §3.6/§3.7's own refinements is caught before
    /// the round trip — the api would 400 it, and the candidate's verdict would
    /// still be untold either way.
    #[test]
    fn an_invalid_report_body_is_refused_without_a_round_trip() {
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(vec![], vec![Ok(control("capture", 4, Some("cand_01J1Z0")))]);
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);
        source.poll(0).expect("capture directive");

        let mut report = candidate_report();
        report.capture_epoch = 4;
        report.execution_id = String::new();
        let fault = source
            .report_candidate(&report)
            .expect_err("an empty execution_id is not reportable");
        assert!(fault.message.contains("execution_id"), "{fault:?}");
        assert!(api.candidate_reports.lock().unwrap().is_empty());
    }

    #[test]
    fn a_transient_control_poll_failure_never_costs_the_author_their_session() {
        // The scenario this guards: the author is ten minutes into Step 4 (first
        // run setup done, model loaded) and the lease was renewed seconds ago —
        // it is provably alive. One 502 from the edge, or one reset connection,
        // must NOT end the hold: `HoldPhase` turns any ControlFault into
        // `TornDownWithoutAck`, which destroys the held guest, loses the live
        // session and cannot even ack (§3.8 expiry is server-owned), leaving the
        // attempt stuck in `holding` until the server sweep. The renew driver
        // already retries a non-fenced failure inside the window; the poll on
        // the SAME channel has to behave the same way.
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            vec![],
            vec![
                Err(WizardApiError::Status {
                    endpoint: "control poll".to_string(),
                    code: 502,
                    body: "bad gateway".to_string(),
                }),
                Err(WizardApiError::Transport {
                    endpoint: "control poll".to_string(),
                    message: "connection reset".to_string(),
                }),
                Ok(control("capture", 4, Some("cand_01J1Z0"))),
            ],
        );
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);
        let response = source
            .poll(0)
            .expect("a blip inside a live lease window does not end the hold");
        assert_eq!(response.directive, ControlDirective::Capture);
        assert_eq!(
            source.last_capture_command().map(|c| c.capture_epoch),
            Some(4),
            "the capture command that survived the blip is still remembered"
        );
    }

    #[test]
    fn a_fenced_control_poll_is_not_retried() {
        // Fenced is definitive — the claim is already dead server-side, so a
        // retry can only produce another 409. Exactly ONE poll is scripted: a
        // second attempt panics the fake.
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            vec![],
            vec![Err(WizardApiError::Fenced {
                endpoint: "control poll".to_string(),
                message: "claim is not active".to_string(),
            })],
        );
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);
        let fault = source.poll(0).expect_err("fenced");
        assert!(fault.message.contains("409 fenced"), "{}", fault.message);
    }

    #[test]
    fn a_control_poll_retry_is_bounded_by_the_lease_not_by_an_attempt_count() {
        // The concrete outage: ato-api is redeployed and refuses connections for
        // ten seconds. Every attempt comes back in about a millisecond, so ANY
        // bound counted in attempts is spent in milliseconds — while the lease
        // provably has minutes left, and the author has a live guest and a
        // half-hour of Step 4 setup riding on it. Only the lease deadline is
        // allowed to end a hold.
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let mut controls = ScriptedApi::refused_polls(5);
        controls.push(Ok(control("capture", 4, Some("cand_01J1Z0"))));
        let api = ScriptedApi::new(vec![], controls);
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);

        let response = source
            .poll(0)
            .expect("a ten-second outage inside a live lease window keeps the hold");
        assert_eq!(response.directive, ControlDirective::Capture);
        assert_eq!(
            api.control_calls(),
            6,
            "every attempt the lease window allowed was actually spent"
        );
        // The retry waits between attempts instead of spinning the api.
        assert_eq!(clock.slept(), CONTROL_POLL_RETRY_BACKOFF * 5);
    }

    #[test]
    fn a_control_poll_that_keeps_failing_still_fails_closed_at_the_lease_deadline() {
        // The other half: unbounded retrying would be its own bug. An api that
        // never answers must not leave a hold running on a lease it can no longer
        // prove — once the observed deadline passes the hold ends the fail-closed
        // way (torn down, no ack), and the bound is the LEASE, not a count.
        //
        // A 20s lease keeps the arithmetic legible: the poll is retried every 2s
        // and the renew (also refused) defers on its own cadence, so the hold
        // ends at 09:10:20 and not before.
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            (0..64)
                .map(|_| {
                    Err(WizardApiError::Transport {
                        endpoint: "lease renew".to_string(),
                        message: "connection refused".to_string(),
                    })
                })
                .collect(),
            ScriptedApi::refused_polls(64),
        );
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:10:20.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);

        let fault = source
            .poll(0)
            .expect_err("the lease deadline is fail-closed");
        assert!(fault.message.contains("lease expired"), "{}", fault.message);
        // 20s of lease at a 2s backoff — far more than the three attempts a
        // count-bounded retry would have allowed, and still bounded.
        assert_eq!(api.control_calls(), 10);
        assert_eq!(
            clock.now_utc(),
            parse_utc_instant("2026-07-22T09:10:20.000Z").expect("deadline"),
            "the retry never waits past the deadline that bounds it"
        );
    }

    #[test]
    fn a_deterministic_control_fault_is_terminal_even_while_the_lease_renews() {
        // The version-skew hang (#1111 review). `/control` fails DETERMINISTICALLY
        // — a body this builder cannot parse, a `capture` with no candidate, an
        // epoch behind observed — all `Contract`. Meanwhile `/lease/renew` is a
        // DIFFERENT, healthy route: it answers, and every success pushes the lease
        // deadline further out. A retry loop bounded by that rolling deadline can
        // therefore never exit — the very bound it relies on is extended by the
        // renew riding the same poll.
        //
        // Short 6s lease that renews an HOUR out, so the renew provably fires and
        // moves the deadline while control keeps faulting. Before the fix the loop
        // renews (the clock passes the original 09:10:06 deadline yet keeps going)
        // and drains every scripted control, then panics `unscripted control poll`
        // — a bounded stand-in for an unbounded spin. After the fix a `Contract`
        // is terminal on the FIRST answer: one control call, no renew, fail closed.
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            vec![
                renewed("2026-07-22T10:10:02.000Z"),
                renewed("2026-07-22T11:10:02.000Z"),
                renewed("2026-07-22T12:10:02.000Z"),
            ],
            ScriptedApi::contract_polls(8),
        );
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:10:06.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);

        let fault = source
            .poll(0)
            .expect_err("a deterministic contract fault ends the hold, it is not retried");
        assert!(
            fault.message.contains("wire contract"),
            "the fault surfaces the version skew: {}",
            fault.message
        );
        assert_eq!(
            api.control_calls(),
            1,
            "a contract fault is terminal on the first answer, never retried into a spin"
        );
        assert_eq!(
            api.renew_calls(),
            0,
            "failing closed at once means the poll never even reaches a renew"
        );
    }

    #[test]
    fn a_persistent_transient_control_fault_is_bounded_while_the_lease_renews() {
        // The residual hang the `Contract`-terminal fix alone would miss: a
        // RETRYABLE fault (5xx / dropped connection) that simply never clears,
        // while `/lease/renew` stays healthy and keeps the lease alive. Bounding
        // the retry by the LIVE lease deadline would spin forever here too (renew
        // moves it out every pass). The fix bounds the retry by a window captured
        // ONCE on entry — `min(RETRY_WINDOW, lease_remaining_at_entry)` — which no
        // renew can extend.
        //
        // Entry lease has 6s left, so the retry window is 6s. The renew fires at
        // 09:10:02 and pushes the deadline an hour out (the lease is provably alive
        // for an hour), yet the poll still fails closed at 09:10:06 — the entry
        // window, not the renewed lease. Before the fix the loop rode the renewed
        // deadline and drained every scripted poll, then panicked `unscripted`.
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            vec![
                renewed("2026-07-22T10:10:02.000Z"),
                renewed("2026-07-22T11:10:02.000Z"),
                renewed("2026-07-22T12:10:02.000Z"),
            ],
            ScriptedApi::refused_polls(8),
        );
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:10:06.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);

        let fault = source
            .poll(0)
            .expect_err("the entry-captured retry window is spent, so the hold ends fail-closed");
        assert!(
            fault.message.contains("transport"),
            "the fault surfaces the transient failure it gave up on: {}",
            fault.message
        );
        assert_eq!(
            api.renew_calls(),
            1,
            "the lease was actively renewed (deadline pushed an hour out) yet the retry still ended"
        );
        // Attempts at 09:10:00/02/04, then the 09:10:06 attempt is past the 6s
        // window captured at entry — four calls, not the eight scripted, and not a
        // spin against the hour-long renewed lease.
        assert_eq!(api.control_calls(), 4);
        assert_eq!(
            clock.now_utc(),
            parse_utc_instant("2026-07-22T09:10:06.000Z").expect("window"),
            "the retry ends at the entry window, never at the renewed lease deadline"
        );
    }

    #[test]
    fn a_renew_that_breaks_the_wire_contract_is_lost_at_once_not_deferred() {
        // Minor (#1111 review): a renew that answers `200` with a body this
        // builder cannot parse is a `Contract` fault — deterministic version skew,
        // the exact case `a_2xx_that_does_not_match_the_wire_contract_is_a_contract_fault`
        // pins. Folding it in with a transport blip defers it to a backoff that
        // burns the rest of the lease window on a call that answers identically.
        // It must surface as a lost lease at once (no ack, §3.8). One renewal is
        // scripted: before the fix `tick` returns Ok (deferred) and this
        // `expect_err` fails; after, it is `LeaseLost::Contract` on the first try.
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        let api = ScriptedApi::new(
            vec![Err(WizardApiError::Contract {
                endpoint: "lease renew".to_string(),
                message: "response did not match the wire contract".to_string(),
            })],
            vec![],
        );
        let mut driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("adopted");
        clock.advance(Duration::from_secs(100));
        let lost = driver
            .tick(&fencing())
            .expect_err("a contract fault on renew is terminal, not deferred to a backoff");
        assert!(matches!(lost, LeaseLost::Contract { .. }), "got {lost:?}");
        assert_eq!(
            api.renew_calls(),
            1,
            "surfaced on the first answer, not retried"
        );
    }

    #[test]
    fn a_lost_lease_faults_the_control_source_before_it_polls() {
        let clock = FakeWallClock::at("2026-07-22T09:10:00.000Z");
        // No control responses scripted at all: if the source polled, the fake
        // would panic. The lease check must come first.
        let api = ScriptedApi::new(
            vec![Err(WizardApiError::Fenced {
                endpoint: "lease renew".to_string(),
                message: "claim is not active".to_string(),
            })],
            vec![],
        );
        let f = fencing();
        let driver =
            LeaseRenewDriver::new(&api, &clock, "2026-07-22T09:15:00.000Z").expect("adopted");
        let mut source = ApiControlSource::new(&api, &f, driver, Duration::ZERO);
        clock.advance(Duration::from_secs(100));
        let fault = source.poll(0).expect_err("fenced lease");
        assert!(fault.message.contains("lease fenced"), "{}", fault.message);
    }
}
