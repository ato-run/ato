# Formation 5c — bounded adaptive integration

**A0–A9 actual PASS; implemented and integration verified.** One proposal
round, finite DecisionProvider choices, requester-owned FixedCandidateProducer,
actual Runtime/Verifier only. No autonomous repair or repeated model generation.
Merge/deploy status is separate; this record precedes requester merge.

## Exact pins

- Core/actual Rust binaries: `20d825bd5ff09e1e2238a4975299e896093c545c`.
- Harness: `01c803a505ab74d77fbb3c657e8f34c33921c1c6`.
- 0307 migration #708 merge: `22e8ce535aa2ba9603a564b4cafc8d0b66d8bff7`.
- Receiver #707 merge: `21e7e0e0b4bf82949278340d4dad7daa8116cf86`.
- WASM: `eff1969064bd1a5a102c532fb5a757eb718913b9430dad4dc4982a46bfe6e342`.
- Main integration: `acafe049840ec9040db31ecfeae2edde2de56e9f`.

Actual Python-process binaries are the already verified core pin, not relabeled
as a build of the integrated head. Integration adds the 6a measurement helper/docs
and main's unrelated OCI egress correction; this Python path does not call OCI.
Integrated selected regression is separately re-run. Historical blocked evidence
and actual binary provenance are preserved in `formation-adaptive-5c-pre0307.*`.

## Actual acceptance (fresh isolated Linux D1/Coordinator)

| Gate | Result |
|---|---|
| A0 | No DecisionProvider: old deterministic proposal behavior, actual PASS |
| A1–A3 | Known bad FAIL → evidence → Escalate → one round → generated bad FAIL → generated good same-K PASS |
| A4 | Actual unresolved UNKNOWN, escalation/producer 0 |
| A5 | Actual FAIL then controlled history-unavailable D1 fault: EffectUnknown, escalation/producer 0 |
| A6 | Attempt budget and deadline exhaustion: producer 0 |
| A7 | Pause after durable chosen Escalate, restart before proposal open: same choice, round1, producer1, actual PASS |
| A8 | Five cases: old invalid-label timeout, malformed JSON timeout, valid-format out_of_set, explicit invalid, provider_error. All advance to generated-candidate decision and actual PASS |
| A9 | Original K/ContractRef, Runtime constraint, network denied/empty bindings preserved; Verifier receipt required |

A8 records seq0's unchanged fallback outcome, round count1, producer call1,
then seq1 with **attempt_seq=0**. No fake attempt consumes the release. Exactly
one real generated attempt follows the next chosen decision; its receipt is
embedded in JSON. Same ContractRef throughout:
`sha256:70d957ef2d15c61af1651a5ef9ae04468b286c815d29e71b3e7b8acdfe63e6ee`.

Adaptive: **12 scenario groups PASS, fixed calls8, live calls0**.
C2: **15 groups PASS, fixed calls12, live calls0**, including zero-D, P0–P11,
unsupported, provider error/timeout, restart/concurrency, UNKNOWN and L1–L3.
A5/A7 are explicit local fault injection, not invented natural incidents.

## Regressions and durable boundary

- Selected Rust: **675 PASS / 0 FAIL / 1 existing ignored**; Clippy/fmt/diff PASS.
- Receiver: **336 PASS / 0 FAIL**; typecheck PASS.
- Real D1 MIG0–MIG12 (17 tests), one byte-preserving scope test.
- schema generate/check/fresh-bootstrap and non-empty 0306→0307 upgrade PASS.
- 0301/0302 unchanged; 0307 replaces only decision-open release guard.
- Open/claimed proposal's independent fence remains intact. Legacy/fallback
  Attempt counting, chosen Inspect/Stop/Attempt and issue fence unchanged.

Prior failure and CI observations remain in the pre0307 ledger, not erased or
relabelled. Receiver CI run 36519799995 has billing failure before any steps;
not green. Integrated-head ato CI classification is recorded separately.

No live Jev/DeepSeek calls, credential read, remote migration, deployment,
production flags, or #1421 changes. 0307 exists/applied only to local tests.
