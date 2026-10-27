# Continuous Patching — design (draft, #1573)

Status: **draft** for review. No implementation or production change in this
document.

## 1. Goal

Keep an Application's Contract (Identity K) reproducible while its realization
path D is updated safely and continuously: when a dependency or base image has
a CVE fix, a new candidate D is generated, verified against the SAME frozen K,
and adopted through the #1572 state-migration machinery — or the old D/State is
kept.

This is NOT a new migration mechanism. It reuses:

- Formation (build/materialization of the candidate D),
- the existing Rust/WASM Contract verifier (receipt authority),
- #1572 SCE (state continuity evidence),
- #1572 EffectPolicy (side-effect acceptance),
- #1572 Atomic Adoption (single-transaction D/Volume switch, wake refusal,
  quiescence proof, crash safety).

## 2. Terms (see K_D_MODEL.md)

- Identity K: unchanged by patching. A patch that would make a new condition
  mandatory for Capsule validity is a NEW K, not a patch.
- Candidate D: a new realization path for the same K (new image digest,
  dependency set or build). It is proposed, not adopted.
- Release: records the approved D set; a patch adds a D by a new Release (or a
  release amendment policy), never by rewriting K.
- Receipt: bound to (K, candidate D, attempt, conditions, Verifier version).
- SCE: required only when the Application is state-bound.

## 3. Pipeline

```
trigger (CVE / dependency advisory / base-image update)
  -> patch intent (Application, Release, current D, target K)
  -> candidate D generation
       (existing build infra or AI-assisted patch; MUST produce a bundle/D
        whose K recomputes to the frozen K)
  -> verification
       - SameK: recomputed ContractRef == frozen K
       - Rust verifier receipt (fully_satisfied with evidenced observations)
       - EffectPolicy: the patch's side effects are accepted
       - SCE (state-bound only): body/id/created_at/count/new-data continuity
  -> Atomic Adoption (#1572): single D1 batch + CAS guard, wake refusal during
     adoption, quiescence proof, crash-safe old/new state
  -> success: adopted D; failure: old D and old State unchanged
```

## 4. Candidate generation policy

- A candidate is a D for the SAME K. If the patch needs a new mandatory
  contract condition, it is a new K and leaves this pipeline.
- Generation sources: existing build pipeline (preferred), dependency bump,
  base-image digest update, AI-assisted patch. The generator never writes K.
- A candidate must be explicitly selected (no planner/fallback); its D and
  attempt are recorded in the receipt.
- Candidates are immutable artifacts; a failed candidate is retained as
  evidence, never edited in place.

## 5. Verification and acceptance

- The Rust verifier is the authority; TypeScript routes and records, and does
  not re-implement Contract satisfaction.
- Receipt acceptance uses the shared rule (no failed/deferred/empty/duplicate/
  unevidenced observation; K/D/attempt/request binding).
- EffectPolicy is evaluated before any state is touched; a rejected policy
  aborts the patch with no state change.
- SCE is required for state-bound Applications; a missing/invalid SCE blocks
  adoption.
- Adoption is atomic: one D1 batch with CAS guards; an Instance with a live
  patch/adoption refuses ordinary wakes; the snapshot is only valid with the
  quiescence proof (old Run/Lease terminal, fence advanced).

## 6. Approval and rollout policy

- No auto-rollout and no production feature enablement in this design.
- Adoption may be automatic only for an explicitly allowlisted Application
  class with a bounded blast radius; otherwise a human approval gate applies.
- A failed patch never degrades the current D/State; retries are new attempts
  with their own receipts.
- Rollback = adopt the previous D through the same #1572 pipeline (the old
  Volume/State is retained).

## 7. Reuse map (#1572)

| Need | Reused |
|---|---|
| candidate bundle validation | existing capsule-bundle quarantine/validator |
| Contract receipt | existing verification-job claim/observe/ack |
| state continuity | `ato.state-continuity-evidence/1` + SCE probes |
| side effects | existing migration EffectPolicy |
| switch | `adoptDerivation` single batch + CAS guard (0365) |
| quiescence | snapshot quiescence proof (0366) |
| crash safety | adoption CAS guard + wake refusal + reconciliation |
| state volume | candidate volume copy-on-migrate |

## 8. Non-goals

- No new Verifier Registry; no composite-application support.
- No K rewrite, no change to existing hashes/identifiers.
- No production migration/flag change in this design.

## 9. Open questions

- Which advisory sources are trusted for a patch trigger (and how to avoid
  false positives)?
- The allowlist and blast-radius definition for automatic adoption.
- Whether a patch Release is a new Release row or an amendment of the existing
  Release's D set.
- SCE probe selection per Application (stop->wake vs Runner restart).
- Cost/quotas for candidate generation and verification.
