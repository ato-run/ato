# Formation roadmap — foundation, exploration, then adaptation

Updated 2026-09-26 against live GitHub state after merging 5a-a, 5a-b and the 5a live-acceptance follow-up #1411. Stage numbers are retained.
This is an implementation plan; pending rows are not shipped functionality.

## Milestones

- Through 4: Formation foundation v1. Known D exploration uses the common
  execution/verification substrate; stop/resume and retained-result reuse work.
- Through 5b: AI-assisted Formation. Proposals use the same frozen K, Runtime
  constraints, policy and cumulative search budget.
- Through 6: practical coverage and continuous operation. Count usable retained
  software, not registered repositories, toward 20 → 50 → 100 applications.

## Order and gates

| Stage | Scope and completion gate | State |
|---|---|---|
| 1, S1/S2 | Common Local/Network attempts, generic process/Node; four independent result states and separate Browser judge semantics | Merged |
| 2a–2c | Runtime layer; Stop/HandOff ownership; portable CLI process/static without rebuilding | Merged through #1395 |
| 3b | Durable UNKNOWN across Runtimes/requests; historical attestation, owner effect finding plus physical cessation evidence, crash-consistent settlement | Merged: ato #1396 `722f0761`, API #686 `c0f2682e`; not deployed |
| 3a | Coordinator uses frozen K and Rust receipt authority via bounded WASM; fail closed and keep Requester recheck | Merged: ato #1397 `16d11baf` / API #687 `7e0e7373`; real local acceptance; deployment not verified in this track |
| 2d | Hosted Formation actual observation/common attempt; API v1/v2 receiver first, then v2 worker | Merged: API #688 `44c14abe` → ato #1398 `01933b2b`; Static/Python/Node actual execution; deployment not verified in this track |
| 2e | Hosted Run/validator and existing OCI/service group through common execution and handle ownership | 2e-a merged: ato #1399 `ef7f345f`. 2e-b merged: ato #1401 `3c9ddd05`; Linux integration validated; not deployed. 2e-c merged: ato #1405 `de4e4fc3`; Linux Docker comparison validated; not deployed |
| 2f | Remove duplicated D→projection→string→intent→build-plan interpretations and unused re-exports | Merged: ato #1406 `7ece42f6` (canonical D → `execution::ExecutionPlan`; K/D refs byte-identical on the fixed goldens); not deployed |
| 3c | Search budget reservations/accounting, then streaming content-addressed source transport | 3c-a merged: ato #1400 `aaaaaa15` / API #689 `88cb8aae`. 3c-b merged: API #691 `e8b0056b` → ato #1403 `0df51b22`; Linux integration, PAX boundary/resource-limit hardening and separate 64/128 MiB rerun validated; not deployed |
| 3d | Retained objects → validated closure/tree → new Run → same K → new receipt without source/scratch | Merged: API #692 `23d69735` → ato #1407 `4697b3b0` (migration 0299 not applied remotely); actual Coordinator replay Static/Python/Node after source deletion; not deployed |
| 4 | Durable deterministic SearchState; D1 failure→evidence→authorized D2; restart preserves search/attempt identity | Merged: API #693 `18fe2c75` → ato #1408 `a46fd62d` (migration 0300 not applied remotely); actual Coordinator restart acceptance cases 1–7, restart points 1–9, review hardening H1–H4; not deployed |
| 5a | Optional finite AllowedChoices DecisionProvider; deterministic fallback under the same permissions and budget; compare attempts, elapsed time, provider usage and cost | **Completed (implemented and locally/integration verified)**. 5a-a merged: ato #1409 / API #694 (ADR-035, migration 0301). 5a-b merged: API #695 `1853f280` → ato #1410 `c883087e` (ADR-036, migration 0302): Inspect / Stop and independent decision sequence; B0–B8 verified. Probe deferred (no safe existing primitive). Live Jev acceptance and same-fixture/same-budget comparison: passed (2026-09-26); both arms 2 attempts / PASS; deterministic 3.573 s, Jev 3.022 s, 1 call, 1612 input / 134 output tokens, estimated $0.000067704. Not deployed; migrations not applied remotely |
| 5b | Typed new-D generation first, then evidence-based D improvement; reject privilege/K/UNKNOWN escape | **In progress; completion gate open**. 5b-a bounded Python-entrypoint draft compiler/requester and receiver 0303 implemented. Fixed-draft actual D1 FAIL → Dnew same-K PASS, dedup/Exact/restart/concurrency verified. One live Jev call validly declined; **LLM-generated Dnew PASS not achieved**. Core ato #1412 / API #696 / requester ato #1413 (Draft) open, not merged/deployed. See [typed generation ledger](../../ops/formation-typed-generation-2026-09-26.md) |
| 6 | Measure 20, expand to 50 then 100; known-D revalidation on new authorized Runtime/Adapter/D | Pending |

**Formation foundation v1 complete** (2026-09-26): all twelve Stage 4 gates are
met on the stack ato #1406 → #1407 → #1408 and ato-api #692 → #693 — implemented
and locally/integration verified with an actual Coordinator restart acceptance
and a final-stack regression ([foundation ledger](../../ops/formation-foundation-track-2026-09-25.md),
[P0 remeasure](../../ops/formation-p0-remeasure-2026-09-26.md)). Merged 2026-09-26
(ato `a46fd62d`, ato-api `18fe2c75`); not deployed, and migrations 0299/0300 are
not applied to any remote database.

5a-a and 5a-b are merged. **5a completion gate closed** after the
[live Jev acceptance and comparison ledger](https://github.com/ato-run/ato/blob/7be9c53514f2e01d37d7f049e8892df798981b27/docs/ops/formation-live-jev-2026-09-26.md).
This single pair establishes bounded integration and records cost; it does not
show an attempt-count improvement or a statistically meaningful speedup.
Probe is deferred until a safe typed primitive exists. 5b-a has started as a
local/integration track, not a deployment. Its fixed typed-draft execution works,
but the single live Jev call declined: the 5b LLM-generation PASS gate is open.
The 5a gate stays closed; its result must not be conflated with 5b.

The sequential track's [integration record](../../ops/formation-integrated-2026-09-25.md)
separates implementation, integration checks, merge and deployment. 64/128 MiB
actual source -> receipt authority -> Requester-accepted VerifiedRoute passed.
The fixed P0 Node-RED source now reaches build; npm DNS EAI_AGAIN remains the
next observed blocker. Existing Chrome E2E SIGABRT also reproduces on baseline;
that full-browser gate is not green. (Historical note from the 3c-b integration; 3d and Stage 4 evidence is in the [foundation ledger](../../ops/formation-foundation-track-2026-09-25.md).)

P0 regression measurement continues at each stage. Go, Java, native build and
new multi-service capacity belong to coverage expansion, not foundation PRs.
No new features are used to mask foundation inconsistencies.

## Preserved boundaries

Formation owns exploration, fixed K, allowed candidate D, constraints, evidence,
budget and termination. It has no private executor, K evaluator or Network
fallback loop. Common attempts own admission, durable start, execution,
observation, verification and receipt. Callers own authorization, leases/fences,
publication and continuation policy. HandOff cleanup is not_attempted, not failed.

2d v2 formation_key is an artifact-candidate lookup key (source closure, D,
target), not proof of K. Separate v1/v2 namespaces. Reuse also checks effective K,
non-secret binding references, policy and execution conditions. Never relabel a
past attempt's receipt. Receiver compatibility lands before v2 sending/old IR
removal. Publication failure must not erase runtime verification evidence.

2f keeps the minimal ExecutionPlan from canonical D + I + Runtime binding.
Presets remain AuthoringDraft frontends. Preserve public K/D identities, not old
internal IR digests or obsolete dual execution paths.

3c accumulates attempts, deadlines, transfer/expanded/stored bytes and future AI
cost across requests using 3b search_id. The three 10 GiB limits are distinct;
retry/re-expansion accumulates, deletion does not refund consumption. Reserve
before concurrent use and settle actual usage. Disk capacity is a separate gate.
A 32 MiB inline source limit is not a search budget; extend with object references,
streaming digest checks and bounded expansion, not larger base64 JSON.

4 inherits search_id and UNKNOWN/stopped state. Freeze K once, improve D only.
Distinguish verified, candidates_exhausted, budget_exhausted and effect_unknown.
At this gate, explicitly record “Formation foundation v1 complete”.

5a selection Jev is separate from the Browser evidence judge. 5b outputs typed
proposals, not arbitrary shell; no source patching, universal computer-use or
unbounded repair loop in the first slices. Existing attempts never change.

6 separates D-generation/selection failures from Runtime capability failures.
Count D-gate, typed-K, model-judged Browser and retained-object replay separately;
100 means retained software actually usable, not 100 repo registrations. Choose
common fixes by measured improvement, then remeasure the affected cohort.
Continuous adaptation first revalidates known D under existing permissions and
budget; new Runtime availability is not new execution/data-movement permission.
LLM adaptation requires explicit scope/policy/budget.

Deployments, migrations, feature flags and staging/production canaries remain
separate authorization gates. Merging any of these PRs grants none of those.

The final 3c-b [archive hardening evidence](../../ops/runtime-network-3c-b-hardening-2026-09-25.md)
records restricted transports, preserved normal identities and re-review heads;
it does not advance 3d or alter the historical integration results.
