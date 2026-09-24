# ADR-028 — UNKNOWN stops the Runtime Network search

Status: proposed; stage 3b, building on ADR-021 and ADR-027.

## Decision

An attempt handed to a Runtime can have executed even when no result arrives.
Absence of a start notification is not evidence of non-execution. The Coordinator
records `unknown` for a claimed attempt with no result at its deadline. A pending,
unclaimed ticket can expire, including when its Runtime goes offline, and the
request can try its next authorized candidate.

The common Rust attempt returns its durable journal state directly:

| `attempt_record` | Meaning | Coordinator behavior |
|---|---|---|
| `not_started` | Rejected before execution/start, or refused a new attempt | Known refusal; existing fallback policy applies |
| `finished` | Durable start and finish | Known result; existing receipt/effect policy applies |
| `started_unfinished` | Started record without a durable finish | UNKNOWN regardless of the reported verdict |

`execution_started` remains on the internal v0 wire for compatibility, but is
derived from this state and must agree with it. Failure-code text is not an
attestation. Failure to durably finish leaves the journal UNKNOWN and stops any
candidate that was going to be handed off. Redelivery never executes an attempt
again. The journal abstraction permits tests to inject a finish failure without
production environment switches.

UNKNOWN and effect classification are independent. `pure` and `idempotent` do
not prove non-execution and never permit automatic retry of UNKNOWN. Effects
only govern fallback after a **known** result.

## Search scope and persistence

`SatisfyRequest.search_id` is an opaque identifier, scoped to the requesting
user. It contains no K, source, user identity, or secret. Each current CLI
`ato form --runtime-network` invocation generates a fresh random id and prints
it with the request id. Stage 4 will persist and reuse it in SearchState; that
full state machine is not introduced here. Starting a fresh id is an explicit
new exploration, not a retry of the stopped search.

Across all requests of one user/search, any unresolved UNKNOWN blocks issuance
and claim of another ticket, another D, another Runtime, and creation of another
satisfy request (`409 search_effect_unknown`). At most one `running`/`unknown`
request exists for the search. Completed satisfied/unsatisfied/exhausted requests
allow another request only when the search is neither unresolved nor stopped.

Migration 0295 in ato-api widens request/attempt status constraints, backfills
old search ids from request ids, and adds UNKNOWN time/reason, resolution and
late evidence columns. Partial unique indexes serialize active requests and
attempts. SQLite triggers hold the request atomically with UNKNOWN and guard
issue, claim, stale request transitions and creation at the write boundary;
an asynchronous pre-check alone is insufficient.

## Late results and owner resolution

A same-Runtime, same-fence late result is retained as evidence, including when
it races expiration. It does not create a route, change UNKNOWN to PASS, advance
the request or resolve UNKNOWN. Wrong Runtime/fence is still refused. The status
view exposes identity, journal state, outcome and receipt count from late evidence;
arbitrary captured payloads and binding/secret values are not echoed there.

Only the owner's authenticated session may call:

`POST /v1/runtime-network/satisfy/:id/attempts/:attemptId/resolution`

Runtime credentials, including another owned Runtime's credential, cannot
represent an owner's explicit finding. The typed body is one of:

```json
{"action":"resolve","resolution":"no_effect_confirmed","note":"Owner's evidence"}
{"action":"resolve","resolution":"effect_reconciled","note":"Owner's reconciliation"}
{"action":"terminate_search","note":"Owner chooses to stop"}
```

`no_effect_confirmed` asserts the owner established that execution/external
effects did not occur. `effect_reconciled` acknowledges possible/actual effects
and explicitly permits continuation after the owner has handled them. A nonempty
note, actor user id and timestamp are persisted once. The historical attempt
status remains `unknown`. Resolution permits the next candidate within the
original constraints/budget; an exact Runtime constraint still prevents fallback.

`terminate_search` marks the search stopped without claiming there was no effect.
No timeout, late result or same-id request reopens it. Stop and resolve use CAS
conditions so simultaneous decisions cannot both authorize continuation. Recording
resolution and reopening the held request are atomic; polling can resume issuance
if the resolving caller disappears after that commit.

## Requester and protocol

The requester reads `search_id`, `unknown_attempts` (including resolved history)
and `unknown`/`stopped` terminal states. The CLI reports UNKNOWN as
`effect_unknown`, never unsatisfied/inconclusive/exhausted.

Both repositories update the existing internal `ato.runtime-network/0` contract
in one coordinated change; no remote deployment is part of this stage. Identical
JSON fixtures and SHA-256 manifests in both repositories test the request and
all three journal states. This is an internal wire change, not v1 interoperability.

## Validation and remaining scope

Rust tests cover admission refusal, normal pass/fail, finish failure and
redelivery. The API tests use real local D1 migrations and HTTP routes with two
simulated Runtime clients. They advance Coordinator time to model R1's lost
report, prove no R2 ticket and no same-search request, explicitly resolve as the
owner, then let R2 report PASS. Separate tests cover late evidence, fencing,
isolation, concurrent creation/expiration/results/advance, and migration of
nonempty tables. These simulated workers do not claim a live remote deployment.

Receipt WASM (3a), Hosted migration (2d/2e), old IR removal (2f), full SearchState,
request budgets, source transport and AI selection/generation remain out of scope.
