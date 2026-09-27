# Formation 5b stack review — 2026-09-27

Status: review corrections implemented and locally/integration verified;
**all six PRs remain unmerged, #1413 remains Draft, nothing deployed**.
5b remains **In progress**. Review completion is not merge authorization.

## Scope and exact revisions

Independent cross-review covered core/search, requester/provider, and receiver
by reviewers other than each area's original implementer. Follow-up review
examined the fixes; no blocking finding remains in this bounded review scope.
This is code/negative-test review, not a conclusion from mergeability or counts.

| PR | Originally reviewed head | Corrected code head | Corrected base |
|---|---|---|---|
| [ato #1412](https://github.com/ato-run/ato/pull/1412) | `7f40a4c59d5c44223efc516101eccc12e1d8c865` | `1cefce635e573326d9b1905360791139ed7fa63e` | `7be9c53514f2e01d37d7f049e8892df798981b27` |
| [API #696](https://github.com/ato-run/ato-api/pull/696) | `8d6c0db33dcdb21676f9cfbece3124aca40c76ef` | `ca4c3eebb1c97b2aba47e157bc849c69533453ca` | `1853f280753c2343f41e1c3e699b4cc6429317a6` |
| [API #697](https://github.com/ato-run/ato-api/pull/697) | `60988de923cdce899520ba6b291b6251c3d20f92` | `39c4e7787f144c0bec94fe0bce5bd129597db94a` | `ca4c3eebb1c97b2aba47e157bc849c69533453ca` |
| [ato #1413](https://github.com/ato-run/ato/pull/1413) (Draft) | `d0066dce2f4c89f4d103e0028c48581efacfb8e7` | `5a0b37ecc05d37ff7bf19c874067b3b3a76b00c6` | `1cefce635e573326d9b1905360791139ed7fa63e` |
| [ato #1414](https://github.com/ato-run/ato/pull/1414) | `cd421dbb8d5822432226637d3b958ada3405e6d1` | `8ebd97c34bdc78f89918c72ab12c85f16bfb9e03` | `5a0b37ecc05d37ff7bf19c874067b3b3a76b00c6` |
| [ato #1415](https://github.com/ato-run/ato/pull/1415) | `2b1de18ea51fc1a861067ff89776119510994f82` | `e71554d4ad78fa838faab60080b01b5b56428ca3` | `8ebd97c34bdc78f89918c72ab12c85f16bfb9e03` |

The commit carrying this record only adds documentation/evidence above #1415's
corrected code head. Ancestor fixes were rebased through the stack with explicit
remote-head leases; #1414/#697 have no new feature changes.

## Findings and narrow fixes

1. **P2, #1412 / #696:** compiler bounded input TOML but not serialized output.
   Reproduction: valid 65,408-byte parent plus authorized long entrypoint became
   65,662 bytes, exceeding receiver's 65,536-byte fence. Reject serialized output
   before return with `generation_result_too_large`. Regression rejects the
   near-limit case and accepts a smaller same-K case. Receiver WASM rebuilt from
   core commit `15cf8bf04c7d63be7f99da733f0b22b131f2c038`; later core commit only
   fixes two test-only Clippy warnings. No SQL/schema change.
2. **P2, #1413:** requester independently rejected parent-D duplication but not
   another originally authorized D. Check the compiled ref against *all* initial
   authorized derivations before pin/contracts mutation. New red/green test
   proves rejection leaves state intact and a different valid D remains usable
   and idempotent. Receiver/Core dedup fences remain in place.
3. **P2, #1415 acceptance example:** independently selected provider/context
   settings could capture v2 while sending v1, or vice versa. Reject both live
   mode/version mismatches before provider creation/submission. One added test
   covers both mismatches and valid live/fixed combinations. Historical G9 used
   matching `jev_v2` + v2 and is unaffected; no inference was repeated.

API regression initially failed solely because a Stage4 row-count assertion
compared D1 timing metadata (`duration: 1` versus `0`) as well as rows. Narrowly
project batch results to rows before comparing; all persistence counts remain
asserted. The failed run is retained, not reported as passing. This is test
stability hardening, not a production behavior change.

## Boundary → code → negative evidence

| Boundary | Enforcement | Tests / actual evidence |
|---|---|---|
| Known Ds before generation within Exact; no generation after known PASS | `search.rs` frontier and `!passed` barrier; generated placements retain Exact | `exact_exhausts_known_ds_before_generation_and_never_changes_runtime`, `exact_known_pass_finishes_without_opening_generation`; G0/G7 |
| Only verified frozen source supplies context | Submission retains verified FrozenSource; `serve_generation` removes receiver schema/context before injecting local projection | `v2_evidence_reads_the_frozen_source_not_later_source_mutations`, `receiver_cannot_supply_local_context_or_break_historical_v1_with_context_extras`; G3–G5 |
| Claim/completion stops cannot reinvoke or double-admit | requester invokes only after winning durable claim; 0303 one-generation/write-once/revision/claim fences | lost response/restart and competing-requester tests; 31 migration tests including 11 individual trigger mutation probes; G6/G8 restart before and after completion |
| Recompile, dedup, same-K, ordinary admission in all accepted paths | shared Rust compiler in WASM and requester; known-D check before pin; SearchState validation; normal runtime ticket and fresh receipt path | compiler authority-escape tests, requester malicious echo tests, new full-known-set dedup test; G1/G2/G7/G8 |

UNKNOWN, owner stop, open decision/action, source/attempt budget and deadline
barriers were retained. No permission, K, source, Binding, network, argv or
provider output authority was added. The operation is **bounded parameter
synthesis over owner-authorized existing Python entrypoints**, not arbitrary
program generation or general repair.

## Residual limitations (not hidden by passing tests)

- Late evidence after a generation claim can advance the global search revision.
  Both the old completion and a completion relabelled with the new revision are
  refused; no second claim is allowed. The search waits until generation expiry.
  The new receiver regression proves timeout and absence of generated candidates,
  extra attempts/routes or fabricated provenance. This conservatively loses
  availability and may leave provider usage unrecorded; do not relax revision
  fences or re-call the model to improve liveness.
- CLI status transport errors return an error, while the acceptance example
  repolls through a Coordinator restart. The restart evidence proves durable
  safety/reuse, **not CLI automatic network recovery**.
- Lexical summaries are hints rather than behavioral proofs; the same-K fresh
  receipt remains required. No context/model efficacy conclusion follows from
  fixed G1 (an oracle draft control, not a deterministic new-D searcher) vs G9.

## Verification performed for these corrections

- Rust formation/runtime-attempt/formation-worker/receipt-authority: **557 passed,
  1 existing ignored**, no failures. Acceptance example: **1 passed**.
- All-target Clippy with warnings denied: PASS. CLI check: PASS. Core standalone
  Clippy also passes (test-only clone cleanup moved into its owning PR).
- API #696 alone: **217 passed / 8 files**; final API #697: **220 passed / 8 files**.
  Typecheck and schema check pass at both heads; schema remains 0303.
- Context/provider single-mutation runner: **7/7 killed**, original bytes restored,
  named tests pass before/after. API suite includes **11 DB fence mutation probes**.
  New compiler-size and requester-dedup regressions were each observed red before
  their fix and green after. These are distinct from the seven existing mutations.
- Local logs retained under each worktree's `.tmp/stack-review/`; mutation run
  `1790479606230894000`. [Machine-readable safe review ledger](formation-generation-stack-review-2026-09-27.json).

### Actual Coordinator revalidation (no live model)

Run `1790479743277424824`, isolated Miniflare `stage5b-api` on the acceptance host,
loopback only, existing local 0303 database. Actual rebuilt Rust requester and
actual Rust Runtime; **G0–G8 all PASS**. No remote D1 migration or cloud deployment.

| Gate | Fresh result |
|---|---|
| G0 | No policy: one failed attempt, no generation |
| G1 | Fixed oracle draft: two attempts, same-K PASS, requester exit 0, 6.702 s |
| G2 | Historical v1 format: same-K PASS |
| G3/G4/G5 | Typed facts, canary/size bounds and inspection projection PASS |
| G6 | Restart after result: reused durable result, no reinvocation |
| G7 | Two known failures before generation, third attempt PASS on Exact only; no-policy control preserved |
| G8 | Six claim/completion competitors: one winner each, claim/result restart reuse, same-K PASS |
| G9 | **NOT rerun**; historical one-shot live-v2 success remains unchanged |

G1 Dnew `sha256:45a278e97d9eda677cba105fe2439450c6ba51ae8907e92c040c2a27e1ed3797`;
frozen K `sha256:58508b604c7b354d2cfa53134de1472f412ee82a84d98ee3336aa84423b90cc6`;
fresh receipt attempt `01M3GEHQ9X4BFRYB18AZYC8MHY`, `fully_satisfied=true`.
Zero live model calls in this review; fixed G1 has one local provider invocation,
0/0 tokens and $0 model cost. These timings are regression observations, not a
new controlled efficacy experiment.

Requester SHA256 `cfe912e596578cf58a8e3daa0b47e85cbd2499620f6e851b7b0e6e773b082704`;
unchanged Runtime SHA256 `a02c962cb3a25b8e74be37e57ea66110b5c21b8b477a98b5ad19daa27bac86a6`;
WASM SHA256 `16e1a370344fbb5c99100ecb59d1c584d84ac9a55cdfb685962af8eb7d3d0ddb`.

## Next decision, not executed

Separate approval is still needed for Draft解除/merge. Dependency order remains
**ato #1412 → API #696 → API #697 → ato #1413 → ato #1414 → ato #1415**.
No merges, CI reruns, deployment, remote migration or #1402 edits were performed.
Future rollout requires 0299 → 0300 → 0301 → 0302 → **0303**, with compatible
receiver before requester. This record does not authorize rollout.

After review/merge, preregister a small evaluation with no generation,
evaluation-only deterministic draft selection, and Jev v2 under equal budgets.
Fix cases and call counts before invoking models; use opaque IDs with multiple
predefined ID/file permutations, multiple entrypoints, misleading HTTP markers,
indistinguishable summaries and too_large/unavailable evidence. Record all
declines/invalid proposals/refusals/K failures, not only successes. This plan has
not yet been implemented or run. Preserve historical v1 decline and one-shot G9.
Evidence-based D improvement remains open; Stage 6 20/50/100 coverage is separate.
