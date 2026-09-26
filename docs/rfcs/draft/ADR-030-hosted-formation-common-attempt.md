# ADR-030: Hosted Formation results from the common attempt

Status: proposed implementation (stage 2d). Deployment is a separate decision.

> **Current status (appended 2026-09-26).** Stage 2d is merged (ato #1398, API #688). The removal anticipated below was done in 2f (ato #1406, unmerged foundation stack): the active Hosted v2 path no longer generates `ProgramIntent` / `EffectiveBuildPlan`; old refs stay readable for compatibility.

## Boundary

`run_claimed_job` reserves the durable job/attempt identity before source acquisition,
freezes K/D using the existing authoring frontend, then calls `run_reserved_attempt`
with `FormationRealizer`, `Unattended` effect authorization and `Stop` continuation.
Hosted does not own another process executor, HTTP observer, or Contract verifier.
The shared builder receives the original Hosted job id, attempt id and fence.
Static candidates are served over loopback HTTP; process candidates execute inside
the existing containment boundary. Artifact existence is not runtime verification.

The caller owns artifact publication and registration. The candidate runtime and
its realization-owned scratch are stopped before publication. Build workspaces and
unpublished artifacts are not covered by the cleanup outcome.

## Result v2 and receiver-first rollout

The API accepts both `ato.formation-result.v1` and `ato.formation-result.v2` before
v2 workers are enabled. Existing v1 canonical digests remain unchanged. v2 carries
canonical Contract/Derivation refs, the common Rust receipt and four independent
outcomes: seal, runtime_verification, cleanup, publication. Old ProgramIntent and
EffectiveBuildPlan refs are optional; this does not remove those internal IRs.

The API checks the exact job/attempt and K/D receipt correspondence and outcome
states. It does not reimplement Contract evaluation. Only a successful sealed,
runtime-verified and published result can register a usable schema. Cleanup failure
must not rewrite a satisfied receipt. The worker itself retains candidates only
after successful cleanup, so it will not publish a candidate it failed to stop.

Rollout order is receiver v1/v2 acceptance, then worker v2 sending. There is no
implicit v1 fallback and no deployment in this change. Successful results use the existing result JSON. Final hardening adds migration
0296 for failure reports and the Hosted job stop boundary.

## Publication failures

The common attempt journal records durable start/finish state. The receipt itself
is persisted before publication in `hosted-outcome.json`, atomically replaced and
fsynced beside the job scratch;
it records the caller's publication outcome separately from the immutable attempt.
An upload failure keeps the successful verification and marks publication failed.
No schema is registered. Typed failure reports retain receipt/outcomes at the API
when delivered; a reporting failure leaves the local sidecar. No restartable
upload/report queue is introduced. Successful results retain the full receipt in
API result JSON.

## Reuse is not verification

`v2:sha256:…` keys name artifact candidates using source closure, canonical D and
target. The v1/v2 namespaces are separate. The key omits K and is never proof of
satisfaction of the current K. No key-based automatic reuse is introduced here.
Future artifact reuse must check effective K, non-secret binding refs, policy and
execution conditions; every new attempt verifies again and issues its own receipt.
A receipt must never be relabelled with a new attempt id.

## Remaining work

Hosted Run, validator_agent and the OCI/service-group paths remain stage 2e.
Removing projection/ProgramIntent/EffectiveBuildPlan is stage 2f. Search budgets,
retained-object resumption, persistent exploration and AI remain later stages.

## Typed failure and Hosted uncertainty (final hardening)

`HostedAttemptFailure` retains the common failure, original error chain, journal
state, receipt and independent outcomes. Daemon and one-shot share one classifier
and reporter. Operator-only causes do not become user-facing raw build output.
Admission, verification, cleanup, record and publication errors keep their codes.
Reservation/history refusals also retain the typed journal state.

Typed reports use `/formation/attempts/:id/outcome`. An old receiver returns an
error instead of silently ignoring uncertainty fields on the old failure API.
`started_unfinished`, `history_unavailable` and `blocked_by_unknown` install a
permanent Hosted job stop; known `not_started` / `finished` refusals remain known
failures. Effect classification never resolves uncertainty.

API migration 0296 adds immutable report rows and a monotonic
`formation_job_unknowns` overlay. The overlay is the authoritative UNKNOWN state;
legacy job/attempt status CHECKs are left intact and their values are frozen.
Both owner-facing status readers return `unknown` before reading legacy status or
accepted results. This is not a failed job or a pending retry. Database triggers
block new attempts, fence/status updates and result inserts even when their
preflight read raced the UNKNOWN report. Late reports remain evidence; no report,
result, timeout or retry clears the stop. Authenticated superseded workers may
report uncertainty, but cannot publish a result at a stale fence.

No automatic or manual resolution endpoint is added in this Hosted slice. The
job cannot resume automatically; an operator must separately reconcile execution
before any explicit new work. This is a job boundary, not Network SearchState.

Failure receipts/outcomes are persisted remotely when the typed report reaches
the API. The fsynced local sidecar remains the fallback when reporting fails.
There is no durable report-delivery queue or remote reconstruction of a lost
worker filesystem. Rollout requires migration and receiver before sender;
those deployment operations remain separately authorized.
