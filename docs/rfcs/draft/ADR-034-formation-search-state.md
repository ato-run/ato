# ADR-034 — Durable deterministic SearchState

Status: implemented on the unmerged foundation stack (ato + ato-api); not
deployed. Migration 0300 is local-only.

## Decision

A Formation search is one explicit state machine, but not a second store. The
existing rows — `runtime_network_searches` (budget, 3c-a), `satisfy_requests`,
`satisfy_attempts` (tickets, results, UNKNOWN and its resolution, 3b),
`verified_routes` (3a) and `runtime_network_retained` (3d) — stay the only
authority. `SearchStateV1` (`ato.formation-search-state/1`,
`lib/formation/src/search.rs`) is their versioned, canonical read model.

```text
durable rows ──read at revision r──▶ SearchStateV1 ─┐
                                                     ├─▶ decide_next(state, placements, now)
Runtime placements (availability, hard filter) ─────┘        │  pure Rust, bounded WASM
                                                             ▼
                       typed SearchAction ──CAS on revision r──▶ one row effect
```

- **Frozen identity.** The first request of a search freezes K (base and
  effective), the Browser Contract, policy (mode, Runtime constraint,
  network, managed, bindings), budget and the authorized D list in order.
  Rust canonicalizes it (`freeze_search`); `frozen_json` is write-once
  (trigger) and a later request of the search must carry byte-identical
  identity (trigger + 409 `search_identity_frozen`). There is no API that
  changes K. A search created before 0300 is frozen from its first durable
  request, never from a new caller.
- **Deterministic order.** Candidates are tried in frozen D order; within a
  D, over the admissible placements in the Coordinator's ranking. A Runtime
  that is absent is `WaitForRuntime`, not a D failure; availability never
  reorders or removes D.
- **Pure decision.** `decide_next` has no clock, database, scheduler,
  executor or K evaluator. It returns one typed action:
  `IssueAttempt`, `ReplayRetained`, `WaitForRuntime`, `WaitForAttempt`,
  `WaitForUnknownResolution`, `AcceptPendingRoute`, `Finish{reason}`.
  Events (`SearchCreated`, `CandidateAdmitted/Refused`, `AttemptIssued`,
  `AttemptClaimed`, `AttemptFinished`, `AttemptUnknown`, `UnknownResolved`,
  `RetainedCandidateAvailable`, `RouteVerified`, `BudgetExhausted`,
  `OwnerStopped`) are a typed projection of the rows for audit, not an
  event store.
- **Effects under CAS.** `revision` advances in the same statement as every
  authoritative change (triggers on attempt insert/update, request status,
  route insert, retained ready). A ticket insert carries the revision it was
  decided on and is dropped if the search moved; settlement is conditioned on
  the same revision. TypeScript assembles snapshots, authenticates, schedules
  and applies effects; the fallback/settlement rules it used to duplicate are
  removed.
- **Termination.** `verified`, `candidates_exhausted`, `budget_exhausted`,
  `effect_unknown` and `owner_stopped` are recorded separately
  (`satisfy_requests.termination_reason`). A request held on an unresolved
  UNKNOWN has not terminated and reports none.

## Rules carried forward

UNKNOWN blocks every later issue of the search until an owner resolution with
physical-cessation evidence (3b); it is never a failure, never retry
permission, never refunded, and is not erased by restart or a late result.
Retained candidates (3d) are an explicit materialization in the frozen
candidate list; a retained replay failure is never overwritten by an older
PASS. Receipts are accepted only by `accept_verified_route`; K is judged only
by `verify_observed_candidate`. No AI decision exists in this core.

## Consequences

The Stage 4 restart points (creation, reserved ticket, claim, failure result,
D1→D2 boundary, retained registration, PASS acceptance, UNKNOWN, resolution)
are covered by an actual Coordinator restart acceptance and Coordinator unit
tests (`docs/ops/formation-foundation-track-2026-09-25.md`). Deployment and
the remote migration remain separate authorization gates.
