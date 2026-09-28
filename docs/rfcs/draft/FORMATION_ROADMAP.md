# Formation roadmap — foundation, exploration, then adaptation

Updated 2026-09-28: architecture correction after #1420 merged. Stage numbers are retained; the historical 5b-a–c1 track is not canonical Candidate Generation. See [ADR-041](ADR-041-formation-provider-split.md).
This is an implementation plan; pending rows are not shipped functionality.

## Original Phase correspondence

The complete downloaded original Formation §10 has **Phase 3 = Jev Decision
Layer**, **Phase 4 = LLM Candidate Generation**, **Phase 5 = Continuous
Adaptation**. Our 5a ≒ Phase 3, 5b ≒ Phase 4, 5c ≒ Phase 5. Our 6a/6b/6c
are additional product coverage implementation tracks, not original Phase
numbers. See [source pins and section mapping](Formation.md#original-formation-correspondence).

The minimum 5b / original Phase 4 completion path is: **no Preset, zero known
D**, small OSS fixture → CandidateProducer → canonical new D → admission →
actual Runtime → Verifier → same K PASS. An existing-D entrypoint switch is not
completion. Core B0–B12 tests and fixed fixtures precede, but do not replace,
that actual acceptance.

## Milestones

- Through 4: Formation foundation v1. Known D exploration uses the common
  execution/verification substrate; stop/resume and retained-result reuse work.
- 5b: Candidate Generation through a provider-neutral CandidateProducer and Ato
  ProposalValidator. K, Runtime constraints and policy stay frozen.
- 5c: bounded adaptive integration of selection, proposals and actual evidence.
- 6a can start with known-D Formation now; neither 5b nor E2 blocks measurement.
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
| 5b | **Candidate Generation**: general LLM CandidateProducer → typed proposal → Ato ProposalValidator/compiler → candidate-pool expansion | **Fixed CandidateProducer path: implemented, actual integration verified, merged, not deployed.** C2 #1424 merged at `d4fa39a693eb253949c65486288908e9ae47cf57`. General LLM CandidateProducer is **in progress** (PR D: receiver/core compatibility merged; requester adapter mock-verified, live gate pending); general LLM efficacy and real-world coverage are separate from fixed-path completion. [Actual ledger](../../ops/formation-proposal-requester-2026-09-28.md): zero-known-D/no-Preset same-K PASS, P0–P11 and L1–L3. E1/E2 selection efficacy is not a prerequisite |
| 5c | **Adaptive Formation Loop**: DecisionProvider ↔ CandidateProducer ↔ Runtime/Verifier evidence, bounded iterative adaptation | Pending; no unbounded generation/execution loop |
| 6a | **Real-world coverage measurement**: 20 real applications using current known-D Formation | Can start independently of 5b efficacy; measurement not yet performed by this correction |
| 6b | **Capability expansion**: 20 → 50 → 100, measured Runtime/Adapter/build/service gaps | Pending; prioritize observed blockers, not repository registration count |
| 6c | **Continuous adaptation**: revalidate on new authorized Runtime, verified D, Adapter or evidence | Pending; triggers never expand execution/data permissions |


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
Probe remains deferred until a safe typed primitive exists. The 5a gate stays
closed and is separate from Candidate Generation.

### Historical 5b-a–c1 and selection evaluation (preserved)

The former “5b = Jev typed generation → efficacy → coverage” reading is replaced
by **5a selection / 5b Candidate Generation / 5c adaptive loop**, with independent
6a measurement. No historical evidence is deleted or relabeled as a new result.

- ADR-037–040, GenerationDraft compiler, same-K checks, durable claim,
  restart/concurrency fences, migration 0303, context/1–2, point/1–3,
  prompt/1–3 and failure/inspection projections remain reusable bounded
  selection / proposal-validation infrastructure.
- #1412/#1413, API #696 and #1414/#1415, API #697 are merged. Fixed-draft and
  prompt-v2 same-K PASS are recorded in the [generation ledger](../../ops/formation-typed-generation-2026-09-26.md)
  and [context ledger](../../ops/formation-generation-context-2026-09-27.md).
  Prompt-v1 decline remains historical. These are not proof of a general LLM
  CandidateProducer or general repair.
- [E1 results](../../ops/formation-efficacy-e1-results.md) remain 60 cells,
  A 0/20, deterministic B 9/20, Jev C 2/20, 20 model calls, zero robust
  additional-success cases. The historical efficacy criterion was not met.
  This measures the finite selector, not LLM CandidateProducer efficacy.
- #1419 context recovery merged at `660dbf6eaedc3384017137f1f20282a359d423aa`.
  #1420 point/3 + prompt/3 merged at
  `34c2ef2a8882f6fc53438c1e0f1722b240ed3d6e`; ADR-040 remains the original
  pre-merge design record, not a current status dashboard.
- #1421 E2 is **OPEN and nonblocking**, observed head
  `dc0fba4e0ecdd585985320551b52b72768002a5c`. Preserve its preregistration,
  fixtures and ledgers as an evaluation asset for finite-choice selection.
  Do not merge, execute oracle/model/72 cells, rewrite history, or further
  harden it as a 5b completion gate. Any later evaluation needs separate scope.

### Corrected implementation order and completion evidence

PR A #1422 and PR B #1423 are merged (architecture correction and proposal
core). C1 API #700 merged at `b81ef143caac3f8e478b0954e9a5db2932c99f19`
after explicit exact-head approval. C2 #1424 connects requester/source inventory/raw recompilation and fixed-producer
actual acceptance; merged at `d4fa39a693eb253949c65486288908e9ae47cf57`,
not deployed. PR D proceeds receiver provenance (D1), requester/source context
(D2), then separately preregistered bounded live acceptance (D3).
D1 API #701 merged at `9fa4be45adf7dbda1fa68689c365a1806921569f`;
D2-A ato #1426 at `c4bb53564669065aa6c47d1ed283e02ac6fa68ae`;
D2-B API #703 at `38668a97e7632256b33074b0d223670c16b76bfd`.
D2-C is the [requester-owned DeepSeek adapter](FORMATION_DEEPSEEK_REQUESTER.md),
implemented and mock-integration verified; #1427 merged at
`8980ab8aef353cd5e33dadce4a8dbf7367f4afa2`, not deployed. No live model
calls or G0–G5 acceptance yet; D3 needs a separately approved preregistration.
The receiver-first merge order is preserved. PR D separately adds a general LLM adapter and source-context
opt-in after fixed-producer acceptance. Migrations, deployments and model calls
are not implied by this order.

P0–P11 in [Formation](Formation.md#acceptance) are the integration acceptance
contract. Unit/mock tests are not actual Runtime/Verifier acceptance. No corrected
P0–P11 execution is claimed by PR A. No rollout or remote migration.

### 6a measurement taxonomy

Classify each of the first 20 real-application failures as known-D/authoring,
CandidateProducer-solvable hypothesis, build capability, runtime/toolchain,
Adapter, multi-service, dependency/network, verifier, Browser/UI, or
retention/replay. Record actual evidence and uncertainty separately. Prioritize
6b by this distribution and use it to extend CandidateProducer vocabulary.
Do not wait for CandidateProducer efficacy to start measuring known-D coverage.

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

5a Jev selects only finite choices; 5b general LLM proposes typed operations.
Only Ato validates/authorizes/compiles D; only Verifier decides K.
EscalateToCandidateProducer is not offered until implemented and within budget.
See [Formation authority boundaries](Formation.md). Proposals are not arbitrary shell; no source patching, universal computer-use or
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
