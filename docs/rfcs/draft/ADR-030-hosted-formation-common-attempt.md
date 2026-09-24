# ADR-030: Hosted Formation results from the common attempt

Status: proposed implementation (stage 2d). Deployment is a separate decision.

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
implicit v1 fallback and no deployment in this change. The receiver requires no DB
migration: the existing result JSON stores all four outcomes and the receipt.

## Publication failures

The common attempt journal records durable start/finish state. The receipt itself
is persisted before publication in `hosted-outcome.json`, atomically replaced and
fsynced beside the job scratch;
it records the caller's publication outcome separately from the immutable attempt.
An upload failure keeps the successful verification and marks publication failed.
No schema is registered. This evidence is local worker storage; this stage does
not claim remote retention of failed-publication receipts or a restartable upload
queue. Normal successful results retain the full receipt in API result JSON.

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
