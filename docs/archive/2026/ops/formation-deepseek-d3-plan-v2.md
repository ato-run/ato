# Formation D3 v2 — exact request evidence preregistration

Status: preregistered, not executed. Review before merge; the execution driver
remains a separate Draft. No live budget journal has been initialized.

## Immutable pins

- Base: `c3f44c6e548cc4f73acaf37966621d21bad0097a`.
- New execution pin: `d1dde994c2d310a725b546921f9b3455777e28da`.
- Plan: [formation-deepseek-d3-plan-v2.json](formation-deepseek-d3-plan-v2.json).
- Plan SHA-256: `a3af8f45f7641f7c367ff8d118b2d4e72abcf05711382ea2a7d3864548bd2249`.
- API: `38668a97e7632256b33074b0d223670c16b76bfd`.
- Receiver WASM: `d4732b46f1ce5ab3d1193d957d7bcb2d24f0469ffaa80e267c6cfda5bccb8a46`.
- Prompt/1: `a3217b28b4242fdc03e11fe5dee7d91fbbc47b79358c4ae31601002b7a81ea50`.

The implementation commit precedes this plan commit. Build execution binaries
from that clean execution pin, never from the later controller/main checkout.

## v1 superseded before execution, not rewritten

The original [plan](formation-deepseek-d3-plan.json) remains byte-identical:
`b67607c6cff1de5339f79822bd3e5d6610d1663dcce6e3f0d6b418ef8a2a1585`.
It is historical preregistration with zero live calls. This document, not an
edit to v1, records `superseded_before_execution`.

Only the plan schema, execution SHA, supersedes relation, and request-evidence
contract differ. Model, prompt, fixtures, K/ContractRef, known D, catalogs,
source-context hashes/policy, pricing snapshot/caps, cell order, limits and stop
policy are unchanged. `deepseek-v2.py check` enforces this exact whitelist.
All six Rust frozen projections were independently reproduced without calls.

## Capture, serialization and durability

After claim, the existing Requester sets the final timeout. Inside
`DeepSeekCandidateProducer::propose`, JCS serializes the final request/2.
The adapter builds the provider JSON and invokes `serde_json::to_vec` exactly
once. The same Vec is SHA-256 hashed and sent using reqwest `.body(Vec)` with
`Content-Type: application/json`. Authorization headers are excluded.

Before credential access, `CallBudget::reserve_request` locks the journal,
validates its history and appends/fsyncs a single event:

```json
{"request":{"cell":"formation_d3_g0_v1","proposal_request_sha256":"sha256:…","provider_body_sha256":"sha256:…","timeout_ms":29481,"proposal_request_bytes":1837,"provider_body_bytes":4261}}
```

The numbers above illustrate the format, not predicted live observations.
`*_bytes` are lengths, not content. Request/source bodies and credentials are
not persisted. This event **is the reservation**. An interrupted append fails
closed; a durable reservation with no response blocks the next cell, including
crash before credential access. There is no refund or retry.

Historical string Cell reservations and their response records remain readable.
They do not satisfy v2 inspection. New response writes require a Request event;
Cell+Request, duplicate Request, missing Request and duplicate Response writes
are rejected. The Rust parser owns validation; the controller must consume
`proposal_budget snapshot` / `inspect-request`, not parse journal JSON itself.
The helper reads only an explicit BudgetPlan JSON and journal, never credentials.

## Exact existing timeout formula (no semantics change)

The existing code is deliberately preserved:

1. `remaining_round_ms = expires_at_ms.saturating_sub(now_ms_after_claim)`.
2. `remaining = min(remaining_round_ms, owner_policy_timeout_ms)`.
3. Require `remaining > 500`; request timeout = `remaining - 500`.
4. HTTP timeout = `min(adapter_config_timeout_ms, request_timeout_ms)`.

For this preregistration, owner policy and adapter timeout are both 30,000 ms,
and the round is at most 30,000 ms. Thus request and HTTP timeout are equal to
`remaining_round_ms - 500`. In general the policy cap is applied **before**
subtracting the reserve: this is not falsely described as a different general
formula. The source implementing the formula is hash-pinned without edits.
The journal records the final request timeout (0 < timeout <= 30,000), not a
preclaim estimate. Actual request/body hashes are runtime observations and are
never predicted in preregistration.

## Budget and unchanged gates

- DeepSeek `deepseek-flash`, thinking disabled; G0 → G1 → G2 → G3 → G4 → G5.
- Max 6 calls; input cap 262,144; output cap 2,048; retries/fallback = 0.
- Registered peak prices remain $0.30/M cache-miss input and $1.20/M output.
  This is the inherited snapshot, not a fresh price/availability assertion.
- Reservation: 78,644 + 2,458 = **81,102 micros/call**, total **486,612 micros**,
  below authorized 5,000,000 micros. No actual billing claim is made.
- Fresh official availability/peak pricing must be checked at execution, before
  any key access. An increase or artifact drift stops the run.
- G1/G2 require actual same-K Runtime/Verifier PASS. G2 requires actual known-D
  failure before the call; no injected summary. G5 requires response-loss GET
  recovery with one Request event and no second model invocation.

Preclaim projection remains useful for source/K drift and G2 actual evidence,
not for an exact provider request hash. #1431 remains Draft/unmerged until v2
consumer changes are reviewed. N15–N19 cover capture implementation, evidence
schema, timeout formula, Rust helper and serializer drift. E15–E19 belong to
that separate controller update; they are not claimed completed by this PR.

Live calls = 0; spend = $0; `DEEPSEEK_API_KEY` unread. No live journal, deployment,
remote migration, #1421/E2 work, compiler/Verifier/receiver or authority change.
