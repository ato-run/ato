# Formation 5b-b — bounded source evidence acceptance

Date: 2026-09-27. [Machine-readable ledger](formation-generation-context-2026-09-27.json).
Design: [ADR-038](../rfcs/draft/ADR-038-formation-generation-context.md).

## Status and stack

| State | Result |
|---|---|
| implemented | Exact correctness fix, bounded source/context projection, requester prompt v2, fallback provenance compatibility |
| locally/integration verified | Rust/API suites, mutation probes, actual isolated Coordinator + Rust requester/Runtime G0–G9 |
| merged | **No** — all 5b-a/5b-b PRs remain open; #1413 remains Draft |
| deployed | **No** — no staging/production/remote migration or feature flag change |

Fetched main before work: ato `7be9c53514f2e01d37d7f049e8892df798981b27`,
ato-api `1853f280753c2343f41e1c3e699b4cc6429317a6`.
Existing 5b-a branches, not main, are the explicit dependencies of this stack.

| PR | Exact base | Tested head / code checkpoint |
|---|---|---|
| [ato #1412](https://github.com/ato-run/ato/pull/1412), Exact correctness | `7be9c53514f2e01d37d7f049e8892df798981b27` | `7f40a4c59d5c44223efc516101eccc12e1d8c865` |
| [API #696](https://github.com/ato-run/ato-api/pull/696), matching WASM + receiver regression | `1853f280753c2343f41e1c3e699b4cc6429317a6` | `8d6c0db33dcdb21676f9cfbece3124aca40c76ef` |
| [ato #1413](https://github.com/ato-run/ato/pull/1413), historical requester, Draft | `7f40a4c59d5c44223efc516101eccc12e1d8c865` | `d0066dce2f4c89f4d103e0028c48581efacfb8e7` |
| [ato #1414](https://github.com/ato-run/ato/pull/1414), pure bounded context | `d0066dce2f4c89f4d103e0028c48581efacfb8e7` | `cd421dbb8d5822432226637d3b958ada3405e6d1` |
| [API #697](https://github.com/ato-run/ato-api/pull/697), fallback provenance compatibility | `8d6c0db33dcdb21676f9cfbece3124aca40c76ef` | `60988de923cdce899520ba6b291b6251c3d20f92` |
| [ato #1415](https://github.com/ato-run/ato/pull/1415), requester v2 + acceptance | `cd421dbb8d5822432226637d3b958ada3405e6d1` | production code `5561461a`, G0–G9 harness `641be03ea7645ac0aad88626f908f52eb862775b`, claim-restart extension `eb15981d` (this ledger is a later docs commit) |

Linear ato stack is #1412 → #1413 → #1414 → #1415; this keeps each new diff
separate without retargeting the historical Draft. API stack is #696 → #697;
receiver compatibility is a prerequisite for #1415. No automatic merge.
Migration **0303 is unchanged**; no new migration. Existing 0299–0303 have not
been applied to remote D1 by this task. Acceptance used already-initialized
isolated local Miniflare D1/R2 on the acceptance host, not staging/production.

## Exact Runtime correction

With generation policy, explore every authorized known D in the current Exact
Runtime/environment before generation. A known PASS finishes without generation.
Generated D remains constrained to that same Exact Runtime. Other Runtime
placements cannot provide a parent failure or execute a generated D. Without
generation policy, historical Exact early termination remains unchanged.
This removes the 5b-a special-case skip-known-D frontier; it does not weaken
UNKNOWN, owner stop, in-flight decision/action, deadline or budget barriers.

## Information and authority

Context `ato.formation-generation-context/1`: sorted opaque IDs, Python language,
size/scan/count buckets, allowlisted import/framework markers, main-guard/listen/
custom-HTTP-handler booleans; project manifest/lockfile/README/runtime presence;
typed failures and safe inspection kinds/refusal codes. No arbitrary names,
strings, comments, paths, source excerpts, URLs, env values, logs, receipts,
Runtime/attempt IDs or unrelated Runtime facts reach provider state.

Requester retains its **verified frozen source**, never re-reads the original
mutable directory, and discards receiver-supplied context/schema before promoting
the local point to `ato.formation-generation-point/2`. Existing inspection evidence
is consumed through a separate provider-specific projection. Fixed question text
uses `ato.formation-generation-prompt/2`; untrusted evidence is state data only.

Limits: 16 entrypoints; at most 64 KiB source read per entry (oversized source is
not prefix-scanned); <=1 KiB entry, <=16 KiB context; <=16 failures, 4 inspections,
8 refusal codes per inspection; <=11 import and 6 framework markers. IDs are
1–32 ASCII alphanumeric/underscore excluding `none`. String-interpolation nesting is capped at 16; provider response is capped at 64 KiB.
Transport timeout is 20 s by default and cannot exceed 30 s; the durable
generation deadline and one-generation budget remain unchanged. Malformed lexical source is
unavailable, unknown codes map to `other`; no new Python parser dependency.

Output remains `{schema: ato.formation-derivation-draft/1, operation:
python_script, entrypoint_id}`. Static validation/canonical D/dedup/same frozen K,
ordinary Runtime admission/execution and receipt authority are unchanged. No new
K/effect/Binding/permission/network/argv/source authority. Provenance is not identity.

## Tests and mutation probes

- Rust formation/runtime-attempt/formation-worker/receipt-authority: **555 passed,
  0 failed, 1 existing ignored**. Clippy all-targets `-D warnings`, CLI check passed.
- API Runtime Network full suite: **219 passed, 8 files**; typecheck and
  schema:check passed at immutable migration 0303.
- **7 reversible source/provider mutation probes**: entry count, wire byte bound,
  source-name leakage, string shielding, exact context-ID match, shared context
  validator, point version. Each named baseline passed, mutant failed at runtime
  (not compilation), original bytes restored, restored test passed.
- **11 existing DB-trigger mutation probes**: open/claim/completion guards,
  write-once, no-delete, candidate dedup, issue/decision barriers, revision advances
  and attempt sequence fence. These execute the real SQL migration with each
  trigger removed independently and demonstrate the violated property.
- Negative tests retain unauthorized/nonexistent entrypoint, duplicate D,
  K/effect/requirements/Binding/network/argv/source/secret escape rejection,
  UNKNOWN/open-action/budget barriers, late completion and second invocation.
- A0–A9 and B0–B8 actual regression passed (B5/B6 are checked inside B2).
  Regression run `1790473188497574399` used the corrected WASM. K/receipt authority
  and Stage 4 canonical fixtures remain covered by the Rust/API suites.
- Independent review found a receiver-context trust issue; fixed by discarding
  incoming context/schema, with v1/v2 regression tests. Follow-up review found no
  remaining critical issue. No CI rerun; commits use `[skip ci]`.

## Actual acceptance

Run `1790473678153561058`, production Coordinator routes in an isolated process,
real Linux/aarch64 Rust requester + Runtime (not a mocked executor).

| Gate | Result |
|---|---|
| G0 | PASS: no-policy Exact, one failed known attempt, no generation row/call |
| G1 | PASS: fixed typed provider, D1 FAIL → canonical Dnew → same K PASS, 2 attempts |
| G2 | PASS: historical v1 provider-input format remains usable, same draft authority |
| G3 | PASS: expected HTTP vs CLI-only typed facts; private names/literals absent |
| G4 | PASS: source canaries absent; 16 entrypoints, oversized file marked too_large, bounds held |
| G5 | PASS: actual Inspect → Attempt → generation; candidate_refusals projected to safe fixed codes |
| G6 | PASS: Coordinator restart after admitted result, one provider call, exact durable row reused |
| G7 | PASS: two known Ds FAIL before generation; third attempt Dnew PASS; all on same Exact Runtime. No-policy control still stops after first failure |
| G8 | PASS: six claim competitors / six completion competitors, one winner each; one admitted D; arbitrary draft additions refused; restart reuse |
| G9 | **PASS: one live v2 call → Dnew → same frozen K PASS → Requester accepts fresh receipt** |

Additional G8 run `1790473934289286647` restarts after claim **before** completion,
rejects a second claim, then restarts after the winning completion. Both durable
rows survive unchanged. No model was invoked in this direct receiver race.

Requester executable SHA256:
`649137432035438bd788e2b89b8cff459a69fd51ac236c3a3dfd59886dd4d253`.
Runtime CLI SHA256:
`a02c962cb3a25b8e74be37e57ea66110b5c21b8b477a98b5ad19daa27bac86a6`.
WASM SHA256:
`17d378b2f7bab68ce339cb75b54e937a686e55b00d632220ac457d4f90377548`.
Local and acceptance-host requester/provider source hashes matched. Further
artifact digests and safe receipt projection are in the JSON ledger.

## Single live v2 observation — no retry

Run `1790473851035524355`, model **jev-1.13.0**, prompt **/2**. A read-only
`GET /v1/models` preflight returned the documented aliases; the versioned model
was pinned explicitly. The key was passed over stdin into requester process
environment only, never logged or written to the acceptance host filesystem.
Coordinator/Runtime did not receive it. A persistent prompt-version once marker
prevents repeating G9. There was **one** model inference call, no cherry-pick.

| Arm | Attempts | Final outcome | Model calls | Settled elapsed | Provider latency | Input/output tokens | Estimated cost |
|---|---:|---|---:|---:|---:|---:|---:|
| No generation policy G0 | 1 | unsatisfied | 0 | 3.035 s | n/a | 0/0 | $0 |
| Fixed typed draft G1 | 2 | same-K PASS | 0 (1 local provider invocation) | 6.268 s | 0 ms | 0/0 | $0 |
| Live Jev v2 G9 | 2 | same-K PASS | 1 | **6.725 s** | **244 ms** | **1100/75** | **$0.0000462** |

G1/G9 fixture bytes, frozen K, original candidate set, generation policy and
budget caps match. Only isolated-owner Exact Runtime IDs differ; each arm stays
on its authorized Runtime. Elapsed includes Runtime startup; the later 7.033 s
audit observation includes comparison/read overhead, not additional execution.
One pair does not establish speed/cost superiority; fixed G1 is a control, not an
implemented deterministic new-D planner. Model output was `entry_app`.
Pricing estimate: $0.042 per million input tokens, output free, checked against
[TypeSafe model pricing](https://docs.typesafe.ai/models) on 2026-09-27; not an invoice.

Generated D:
`sha256:45a278e97d9eda677cba105fe2439450c6ba51ae8907e92c040c2a27e1ed3797`.
Same frozen K:
`sha256:58508b604c7b354d2cfa53134de1472f412ee82a84d98ee3336aa84423b90cc6`.
Fresh receipt attempt `01M3G8XSRAW1BWEKT64PMMM56B`, `fully_satisfied=true`;
health, page and source-identity observations all satisfied. Requester exit 0.

## Remaining 5b gate

This closes the **minimal live typed Python generation acceptance** for 5b-b,
not all Formation 5b. Overall status remains **In progress**: stack review/merge,
broader fixture coverage and evidence of practical improvement remain separate.
Only owner-authorized Python entrypoint drafts exist; this is not a general
repair agent. Deployment readiness and remote migration are a separate track.
The historical prompt-v1 valid decline and unknown usage are unchanged, and 5a
remains Completed/merged. Probe, arbitrary source/code/shell generation and
permission expansion remain out of scope. #1402 was neither merged nor edited.
