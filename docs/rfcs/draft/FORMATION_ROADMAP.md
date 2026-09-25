# Formation roadmap — foundation, exploration, then adaptation

Updated 2026-09-25 after the sequential 2e-b / 3c-b integration track. Stage numbers are retained.
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
| 2e | Hosted Run/validator and existing OCI/service group through common execution and handle ownership | 2e-a: #1399 implemented, unmerged. 2e-b: #1401 implemented and Linux integration validated, unmerged/undeployed. 2e-c OCI/service group remains pending |
| 2f | Remove duplicated D→projection→string→intent→build-plan interpretations and unused re-exports | Pending |
| 3c | Search budget reservations/accounting, then streaming content-addressed source transport | 3c-a merged: ato #1400 `aaaaaa15` / API #689 `88cb8aae`. 3c-b: ato #1403 / API #691 implemented and Linux integration validated, unmerged/undeployed |
| 3d | Retained objects → validated closure/tree → new Run → same K → new receipt without source/scratch | Pending |
| 4 | Durable deterministic SearchState; D1 failure→evidence→authorized D2; restart preserves search/attempt identity | Pending |
| 5a | Optional finite AllowedChoices DecisionProvider; Jev failure uses deterministic choice under same budget | Pending |
| 5b | Typed new-D generation first, then evidence-based D improvement; reject privilege/K/UNKNOWN escape | Pending |
| 6 | Measure 20, expand to 50 then 100; known-D revalidation on new authorized Runtime/Adapter/D | Pending |

The sequential track's [integration record](../../ops/formation-integrated-2026-09-25.md)
separates implementation, integration checks, merge and deployment. 64/128 MiB
actual source -> receipt authority -> Requester-accepted VerifiedRoute passed.
The fixed P0 Node-RED source now reaches build; npm DNS EAI_AGAIN remains the
next observed blocker. Existing Chrome E2E SIGABRT also reproduces on baseline;
that full-browser gate is not green. No 3d saved-object replay is claimed.

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
