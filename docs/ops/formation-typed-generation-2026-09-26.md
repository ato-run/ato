# Formation 5b-a — typed generation, including an unmet live target

**Implemented; local/unit/integration safety verified; not merged or deployed.**
The fixed typed-draft path reaches D1 FAIL → Dnew → same-K PASS. **The one live
Jev call declined. LLM-generated Dnew PASS and the overall 5b gate remain open.**
No live retry or prompt adjustment was used to select a successful result.

## Revisions and PR split

- 5a evidence PR [#1411](https://github.com/ato-run/ato/pull/1411) merged at
  `7be9c53514f2e01d37d7f049e8892df798981b27`, with reviewed head
  `0541aa6579406c6d27c0f081352bbd363dfd5fd6`. This is the ato base.
- API base: `1853f280753c2343f41e1c3e699b4cc6429317a6` (#695).
- Core [ato #1412](https://github.com/ato-run/ato/pull/1412):
  `a6cc5693be4bb4025f42f47254613e1df850b3c5`.
- Receiver [API #696](https://github.com/ato-run/ato-api/pull/696):
  `57cb3ed67fc1a88d0f2fcff572539b7adccdcb94`.
- Requester/provider, harness, ADR-037 and this ledger are a separate stacked
  follow-up to the core. Its live target remains unmet.
- [#1402](https://github.com/ato-run/ato/pull/1402) remains open at
  `2c1c6924286782f090783f1bd47a68e2e9b979ba`. After #1411, its Roadmap equals
  main (empty file diff). Do not merge it as part of 5a/5b; after future 5b
  merges its older Roadmap must not roll progress back.

The core can merge first; the API receiver must merge before the requester
integration. All commits contain `[skip ci]`; no manual CI rerun. No 5b PR was
merged automatically. Implementation, merge and deployment are separate states.

## Implemented boundary

The only model draft is `{schema: "ato.formation-derivation-draft/1",
operation: "python_script", entrypoint_id}`. An owner-authorized map resolves
opaque IDs to existing immutable Python files. No K, permissions, Runtime
facts, argv, URL, environment, Binding, source patch or secret field exists.
The compiler copies one supported pinned Python parent, substitutes only the
script and rebinds through the common canonical authoring compiler, proving
K equality. The initial candidate vector is immutable; the admitted candidate
is a separate record. Canonical duplicates are not added. Provenance is not
D identity. See [ADR-037](../rfcs/draft/ADR-037-formation-typed-generation.md).

The provider sees only opaque IDs and a fixed projection of typed failures;
source/manifest/paths/raw logs/receipts/host identity/unrelated facts are absent.
Runtime admission, execution and verification remain the ordinary authority.
The requester independently recompiles recovered drafts and pins its first
generated D. Exact placement permits only the generated D at the original
explicit Runtime/environment, never fallback to another known D or Runtime.
No-policy exact behavior is unchanged.

`max_generations=1`, a maximum 30 s generation point and 20 s live HTTP timeout
bound the process. A durable one-shot claim precedes the provider call; a crash
without a completion expires instead of retrying the model. Existing attempt,
byte, deadline and decision budgets remain independent. UNKNOWN, owner stop,
unfinished execution and unaccepted receipts remain barriers.

Migration **0303** adds one durable generation record per owner/search. Eleven
trigger fences cover opening, revision advance, claim/completion, append-only
input/result, no-delete, candidate scope/dedup, blocked tickets/decisions and
fixed decision attempt sequence. Structural checks bound JSON/TOML sizes.
The API compiles through Rust WASM; TypeScript does not implement K authority.
Prior migrations were not edited.

## Actual primary run

Isolated `oci-linux-test`: actual production Coordinator routes under Miniflare
local D1/R2, actual Rust `formation_search` requester using shared production
helpers, actual existing Rust Runtime (unchanged source-ticket wire). The
requester was built from `8daa6a19` and receiver from `f4ddc503`; the final
post-run changes preserve decline telemetry only. Exact SHAs and executable
hashes are in the [JSON ledger](formation-typed-generation-2026-09-26.json).
The local database was a fresh copy of the quiescent 0302 acceptance state,
then received 0303. This is not a remote Cloudflare D1 migration.

Run ID: `1790425639229723495`.

| Case | Result |
|---|---|
| C0 no generation policy | PASS: original D FAIL, 1 attempt, no generation row |
| C1 fixed typed draft | PASS: original D FAIL → new canonical D PASS, same K; 2 attempts |
| C-failure | PASS: provider_error is durable, no new D, 1 attempt |
| C-duplicate | PASS: parent-equivalent canonical D rejected as duplicate, 1 attempt |
| C-exact | PASS: both attempts on the exact owner-authorized Runtime |
| C-timeout | PASS: unclaimed point expires, 1 attempt, no model call |
| C-restart/concurrency | PASS: 6 claims → 1 winner; 6 completions → 1 winner; row/provenance unchanged across Coordinator restart; Dnew PASS |
| C-live | **Target FAIL**: valid Jev decline/none; no Dnew; 1 failed original attempt |

The concurrency case also rejects draft fields for K/permission/argv/URL/source
patch/secret, without creating an additional candidate. Provider projection
checks exclude the source canary and private entry paths.

### Fixed versus live (not a performance win)

Both arms used identical source archive/closure, K, initial D and frozen
policy/budget limits, verified from their durable states. Only the draft
provider differed. One authorized alternative entrypoint makes this a bounded
integration fixture, not a realistic general repair-quality benchmark.

| Arm | Attempts | External model calls | Elapsed | Input/output usage | Cost | Outcome |
|---|---:|---:|---|---|---|---|
| Fixed typed draft | 2 | 0 | 5.275 s | 0 / 0 | $0 | same-K verified PASS |
| Live `jev-1.13.0` | 1 | 1 | full duration not retained; provider+submission 555 ms | unknown | unknown | declined; no Dnew |

There was one local fixed-provider invocation, but no external model call in
that arm. The valid live response passed exact model/label validation. No raw
model text was given execution authority. A decline is not a K verdict.

The original decline branch dropped model/usage provenance and the harness
asserted before saving full elapsed time. These values cannot be recovered;
**unknown is not zero**. The final code now persists bounded decline provenance
and writes the observation before the target assertion. The original live
result is retained as-is; no live rerun was performed after that fix.

The user-designated `JEV_API_KEY` in ato-api `.dev.vars` was passed through SSH
stdin and bound only to the requester process as `ATO_GENERATION_JEV_API_KEY`.
It was not printed, committed, written on the host, passed to the Runtime or
Coordinator, or installed as persistent configuration.

## Verification and investigations

- Four-package Rust suite (`formation`, `runtime-attempt`, `formation-worker`,
  `receipt-authority`): **527 passed**, one pre-existing manual
  Linux test remains ignored. No new skip was introduced.
- Worker + CLI all-target Clippy, `--no-deps -- -D warnings`: PASS.
- Runtime Network full suite: **209 tests / 8 files PASS**. API typecheck and
  schema:check PASS. **11 actual trigger-deletion mutation probes**.
- Stage 4 canonical fixtures, retained replay, UNKNOWN, budget and placement
  rechecks are included in the Rust/API suites. The large source/replay
  cross-host foundation benchmark was not repeated for this slice.
- Post-hardening actual synthetic decline: PASS at **12:38:20Z**, run
  `1790426294410987029`; exact bounded provenance/usage persisted, one attempt,
  no generated D. This was **not another live Jev invocation**.
- Actual **A0–A9** and **B0–B8** regression: PASS at **12:40:53Z**, run
  `1790426300894804789`. B5/B6 are assertions within B2. Includes restart,
  evidence reuse, concurrent answers, out-of-set/default behavior, deadline,
  decision budgets and provider-visible canaries. No provider key was passed.
- Coordinator/requester/acceptance Runtime processes were stopped; temporary
  owner-token files were removed. Artifact digests are in the JSON ledger.

Failures were investigated, not hidden: an initially stale worker bundle used
a self-refusing generation check; rebuilding the corrected receiver resolved
it. Independent review found missing transfer-cost admission and a receiver
error-cause mapping issue. Exact-runtime integration then exposed the core's
old early termination; the opt-in-only fix preserves other placement behavior.
A loopback test inherited nonblocking sockets on macOS; it now explicitly uses
blocking accepted sockets. API parallel migration hooks timed out; the unchanged
suite passed with serial file scheduling.

The legacy A2 harness expected one decision point even though 5a-b now offers
Inspect/Stop at the second frontier. This reproduced against the baseline
Stage-5a Coordinator/WASM with the same no-generation requester. The current
harness asserts two out_of_set points; A3/A6/A9 explicitly authorize one point
to preserve their original isolated deadline/concurrency test intent.

## Remaining gates / no deployment

5a stays Completed on main. 5b is **In progress**, not Completed:

1. Prospectively define and version any improved bounded-input/prompt design;
   preserve this valid decline as evidence, not an erased trial.
2. Achieve a live typed Dnew → actual execution → same original K PASS.
3. Retain complete usage/cost/elapsed provenance for that run and compare with
   the same authorized deterministic baseline before making value claims.
4. Review broader Adapter vocabulary and evidence-based improvement separately;
   this slice is only a bounded Python-entrypoint compiler.

Probe remains deferred. No deploy, remote migration, staging/production change,
feature flag or persistent provider-key setup occurred. A future separately
authorized rollout needs 0299 → 0300 → 0301 → 0302 → 0303 and the compatible
API/Coordinator before connecting generation-capable requesters, followed by
stage/production-specific acceptance. Local integration is not deployment.
