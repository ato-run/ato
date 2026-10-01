# CandidateProducer C2 — fixed requester actual acceptance

2026-09-28. **Fixed path implemented and locally/integration verified.** C2 is
under review, not merged/deployed. This is not general LLM efficacy evidence.
The historical A/B and E1/E2 ledgers remain historical, not current status.

## Exact pins and authority

- ato A #1422 merge: `fdb090b541c3efcf175e4daeab9f0f70790a4d0c`.
- ato B #1423 merge / C2 base: `0ff11e14c3ae19c30c2f3957a50d59ab14eb55c8`.
- API [C1 #700](https://github.com/ato-run/ato-api/pull/700) merged the approved
  head `af2bb697cd8a86ef5a506d4341352f32a6c90ceb` normally, without admin bypass,
  at **`b81ef143caac3f8e478b0954e9a5db2932c99f19`**. Acceptance stayed pinned to
  this clean receiver checkout; no C2 API edits or main-following.
- Tested C2 implementation/examples/harness:
  **`ae0a17f01a583591f63d0ad1c99bd34a20cf1c6d`**. Subsequent ledger/roadmap changes
  do not change these tested bytes. The PR metadata records the final head.
- C1 Rust WASM provenance stays B merge `0ff11e14...`, SHA-256
  `8ab4dfb6b6e0376cf0e1aec8c3d66a30c7db7839e9acd26857d0af942a072452`.
- #1421 remains open at `dc0fba4e0ecdd585985320551b52b72768002a5c`, unchanged
  and unexecuted; never a CandidateProducer blocker.

[Original Formation correspondence](../rfcs/draft/Formation.md): Phase 3 → 5a
Decision, Phase 4 → 5b Candidate Generation, Phase 5 → 5c adaptive loop;
6a/b/c remain additional coverage tracks. Original source and ADR-037–040 were
not rewritten. Only Verifier decides K. DecisionProvider cannot generate D;
CandidateProducer cannot judge K. Escalate is still absent (5c scope).

## Implemented requester boundaries

See the [requester contract](../rfcs/draft/FORMATION_PROPOSAL_REQUESTER.md).

- Explicit `enable_candidate_producer` re-verifies the frozen archive digest
  and source closure, extracts a private inventory, and checks every authorized
  entrypoint as a regular file. Symlink file/directory and out-of-root paths
  cannot authorize source. Module IDs require corresponding `.py` or package
  files, including parent packages, not a host-installed module. Public DTO
  mutation cannot bypass verified enablement or change pinned I/K/policy/bases.
- `prepare_proposal_submission` accepts independent explicit K/I with **zero
  known D, no preset, no base recipe**. Owner authorization pins runtime, port,
  cwd and logical IDs. Neither executable/path/argv nor network/effect rights
  can be proposed by the producer.
- `ato.formation-proposal-request/1` is built locally: search_id, frozen K,
  Runtime constraint, known D refs, bounded enum failure/inspection evidence,
  OperationCatalog and remaining attempt/proposal budget. No source_context,
  source bytes, private mapping/path, credentials, Binding values, raw logs,
  receipts or host identity. Actual recorded producer requests were checked.
- Policy: one round, at most four proposals, timeout <=30 s; actual fixture
  uses 10 s; `allow_source_text=false`, `max_source_bytes=0`.
- GET/open → successful claim/revision/private fence → one producer invocation.
  Claim timeout/lost response/409 never grants a call. Local claim-attempt
  latch also prevents stale-GET retries. Restart cannot recover a fence.
- Exact bounded raw bytes are submitted once, not parsed/reserialized to a
  trusted ProposalBatch. Completion response loss uses GET only; no completion
  retry and no new proposal. An uncommitted completion expires safely.
- Every durable completion is independently recompiled by native Rust from
  requester-owned frozen I/K/auth/original recipes and exact raw bytes. Compare
  member count/order, outcome, proposal ID, D, recipe, bound derivation and
  SearchCandidate plus effective candidate vector. Reject any mismatch before
  admitting contracts/recipes. Later changes/disappearance also fail closed.
- Only independently admitted D receives a canonical recipe and original-K
  mapping. Frozen known D is unchanged. Normal placement/ticket/Runtime/
  observation/Verifier/receipt/requester acceptance is reused, not a new executor.
- `fixed` provenance only; error/timeout has no fabricated provenance or K
  evidence. PR D must separately version general provider/model/usage/error data.

## Actual environment and evidence

Isolated **Linux aarch64 test host**, local Miniflare Coordinator/D1/R2, actual
production C1 routes with normal runner-token authentication and production
Rust requester/Runtime. This is not Cloudflare remote D1 or a deployed service.
Schema baseline 0304 initialized locally. Python 3.12.7 was already provisioned.
All spawned harness/requester/Runtime processes were stopped afterwards; test
credentials were removed. No environment feature flag was enabled.

Final hardened run: `1790576069513738409`; 15 scenarios cover P0–P11, zero-D, both provider
failures, unsupported and L1–L3. **All assertions passed; 12 fixed invocations
in total, 0 external/live model calls.**

The [machine-readable evidence](formation-proposal-requester-2026-09-28.json)
contains actual attempts, receipts, immutable K, raw completion/outcomes,
independent recipes, provider requests, finite decisions, exact source hashes
and binary hashes. Complete original result JSONs are retained beneath the
isolated run path recorded there, with their SHA-256 hashes.

| Gate | Actual result | Fixed calls |
|---|---|---:|
| P0 | No proposal policy; one known D attempted, actual FAIL; no round/call/new candidate | 0 |
| P1 | Known D actual FAIL, frontier exhausted, exactly two admitted new D | 1 |
| P2 | Fixed finite DecisionProvider chooses generated good D; actual PASS and requester accepts | shared P1 call |
| P3 | Known FAIL → generated bad D actual FAIL → durable failure evidence used at next finite decision → good D PASS | 1 |
| P4 | K injection / shell / unauthorized ID each rejected; 3 rejected members, execution 0 | 1 |
| P5 | Four members: 2 admitted, duplicate + unauthorized rejected; only admitted pool available | 1 |
| P6 | Separate provider_error and timeout; no attempt/candidate/K failure, no retry | 1 each |
| P7 | Process exits after successful claim/invocation; requester restart sees claimed then timeout; no second call | 1 total |
| P8 | Two real HTTP claims held at a barrier then raced; one CAS winner; one admitted D/PASS | 1 total |
| P9 | Real Runtime cannot persist start journal; unresolved UNKNOWN blocks proposal and next attempt | 0 |
| P10 | Known FAIL and generated PASS keep byte-identical frozen K/ContractRef | shared P1 call |
| P11 | Choice/compilation not PASS; actual Verifier receipt required; removing receipts from a copy of a real reply is refused by requester | no extra call |
| zero-D | No preset/capsule.toml/base D → first canonical D → durable admission → actual Runtime/Verifier → accepted route | 1 |
| unsupported | Zero D → unsupported → bounded CandidatesExhausted, no attempt | 1 |
| L1 | Claim commits but HTTP response dropped; restart cannot obtain fence; no invocation | 0 |
| L2 | Completion commits but response dropped; restart GET independently recompiles, reuses D, PASS | 1 total |
| L3 | Completion persisted while owner-specific attempt insert paused; no verified route yet; Coordinator+requester restart, identical durable round, recompile, normal attempt PASS | 1 total |

No fabricated PASS/FAIL was injected. P9 uses an unwritable journal, L1/L2 drop
real HTTP responses, P8 races actual claim CAS. L3's test-only D1 trigger pauses
attempt issuance (never supplies a result) and is dropped after restart.
P11's negative reply copy is only passed to requester acceptance, never stored
or submitted to the API. Verifier receipts come from actual HTTP responses.

### Zero-D same-K witness

K / ContractRef for **all** scenarios:
`sha256:70d957ef2d15c61af1651a5ef9ae04468b286c815d29e71b3e7b8acdfe63e6ee`.

Zero-D first D:
`sha256:304c3726af1979dd2707f703f00570892b4c5d2370c7e233a79487276ed5482f`.
GET `/health`: status 200, body digest
`sha256:e1facfb37c9f39db4aedb1c196f56a0b5c0f90b0fc2318977b1036d9e0aaf999`,
`fully_satisfied=true`, receipt linked to the actual attempt and D.

The small OSS fixture vendors CPython 3.12.7 `Lib/http/server.py` unchanged at
`0b05ead877f909b7efe712db758012d9dbece7ce` with its license. An Ato-owned fixed
launcher adapts its port to the existing logical-port ABI. It is explicitly an
OSS-based small fixture, not arbitrary-OSS synthesis or LLM efficacy. No source
patch was generated, no code-repair agent ran, and the bad/good batch was fixed
before execution. This closes the **fixed-path** Phase-4-shaped actual gate,
not the general LLM/coverage gate.

## Integration defects exposed and conservatively fixed

Earlier zero-D trials did not PASS. Their evidence was retained, not relabeled:

1. Every Python provisioning prerequisite claimed network even when exact
   Python was already installed, so denied-policy admission refused it. The
   shared Runtime now matches the **entire platform-owned** prerequisite and
   replaces only a present pinned Python's provision with an isolated, read-only,
   network-denied exact-version check. Missing Python still requires network;
   authored/dependency steps are never reclassified. Disappearance/version
   mismatch fails execution; no download fallback. D/K are unchanged.
2. Authored cwd `.` was passed directly to the launch wire, which requires the
   root as an empty relative string. Normal launch now reuses the existing
   checked workspace-relative projection, also used by build steps. No cwd
   permission is broadened or D rewritten.
3. Harness-only bundle fixes: preserve node crypto for ulid; exclude AppleDouble
   metadata from Wasm modules. No C1 source changes. Initial harness receipt
   assertion used `contract_ref` instead of the actual route's
   `effective_contract_ref`; the assertion was corrected, not the receipt.

## Regression and CI (not conflated)

- Full **selected package** regression: formation + formation-worker +
  runtime-attempt + receipt-authority, **638 passed / 0 failed / 1 existing
  ignored**. This is not a whole-workspace/all-platform green claim.
- Includes 10 new requester boundary tests and 3 cached-toolchain boundary
  tests. No test was skipped/removed to obtain a pass. The prior ignored
  `export_source_hardening_request` stays ignored.
- Worker + CLI + Runtime all-target Clippy `--no-deps -- -D warnings`: PASS.
  `cargo fmt --all --check`, `git diff --check`: PASS.
- Linux actual binaries built with Rust 1.96.0 and `--locked`. All changed
  source/build inputs were byte-compared against tested local commit. API
  bundle is C1; receipt-authority WASM is B merge, not C2 replacement WASM.
- Main/base CI is **not green**. Same workflow/platform baseline at `0ff11e14...`:
  [Rust CI 36373944654](https://github.com/ato-run/ato/actions/runs/36373944654)
  and [computation CLI 36373944653](https://github.com/ato-run/ato/actions/runs/36373944653).
  Downloaded failed logs confirm Windows `browser_sandbox.rs` Unix-only APIs;
  macOS `portable_application::a_process_run_is_owned_by_its_run_until_stopped`.
  Rust CI also has main Ubuntu hosted-process and macOS hosted-observation
  failures. These baseline failures are documented, not hidden or called green.
  C2 PR CI status must be read separately; baseline evidence does not excuse
  new failures automatically.

## Rollout state

| State | C1 | C2 |
|---|---|---|
| implemented | receiver | requester/fixed path |
| locally/integration verified | reviewed receiver evidence; exercised by C2 | P0–P11 + zero-D + L1–L3 actual PASS |
| merged | `b81ef143...` | **no**, Draft/review only |
| deployed | **no** | **no** |

0303 untouched; no new migration in C2. 0304 **local only**, no remote apply.
No staging/production deployment, general LLM, source transmission, E2/oracle/
72 cells, Escalate action or unbounded loop. Future general LLM effectiveness
requires separately authorized PR D; 6a coverage need not wait for it.

## Final hardening review — response headers fixed, Browser CI hold retained

The implementation/harness pin above is now `ae0a17f01a583591f63d0ad1c99bd34a20cf1c6d`.
The original actual record at `dc2c4401...` remains in Git history. Only the
acceptance Python changed: LossProxy no longer reads/forwards upstream response
headers. It emits fixed `Content-Type: application/json` and a computed
Content-Length. No CR/LF sanitizer, suppression, or arbitrary header pass-through.

- CodeQL alerts **142 and 143 are `fixed`**, queried on `refs/pull/1424/head`.
  Python analysis **1849453447** (old head) had 2 results; **1849547971**
  (hardened head) has **0 results, no error**, Analyze (python) job
  [108809734163](https://github.com/ato-run/ato/actions/runs/36385440267/job/108809734163)
  succeeded. Aggregate CodeQL was temporarily neutral because Rust analysis
  was still missing; aggregate neutral alone was **not** used as proof of fix.
- All **15 actual scenarios rerun**, not just P8/L1/L2: P0–P11, zero-D,
  unsupported, both provider errors and L1–L3 PASS; fixed calls **12**, models **0**.
  Run `1790576069513738409`. Every frozen search and recompiled recipe remained
  byte-identical to the preceding acceptance. Native binaries were reused only
  after confirming unchanged Rust inputs and binary hashes; the hardened harness
  was transferred and its host/local hashes compared. JSON evidence is refreshed.
- Selected regression rerun **638/0/1**; Clippy, fmt and diff checks PASS.

### Browser failure is NOT a baseline failure

Exact target:
`tests::browser_e2e_routes_keyboard_and_click_through_authority_then_ack_then_record`
in `ato-connected-realization-worker`.

| Observation | Result |
|---|---|
| Base `0ff11e14...`, macOS Rust CI 36373944654 | Target PASS |
| C2 `4b83ae51...`, [job 108803830389](https://github.com/ato-run/ato/actions/runs/36383463542/job/108803830389), attempt 1 | Target FAIL at line 5514: stale_webmcp lacks `stale_operation` |
| **Same exact C2 head**, [job 108809601526](https://github.com/ato-run/ato/actions/runs/36383463542/job/108809601526), attempt 2 | Target FAIL at **earlier line 5271**: Browser bridge handshake timeout, before the stale assertion |
| Hardened head `ae0a17f0...`, [job 108809739680](https://github.com/ato-run/ato/actions/runs/36385443292/job/108809739680) | Target **PASS**; whole job still fails known portable/hosted tests |
| Hardened head, local macOS 15.7.4 arm64 / Rust 1.96.0 / Chrome 153.0.8010.53, isolated 3 sequential runs | **FAIL (handshake timeout), PASS, PASS**; no skips |

The retry did **not** reproduce the original stale assertion, but also did not
PASS. Do not erase the retry failure or call the whole Browser test green.
This is a nondeterministic Browser observation; its root cause remains open.
**Merge hold retained**, per the requested fail-again branch. No test assertion,
timeout, retry policy or skip was changed to conceal it.

Causality inspection: the test, BrowserHost, Browser Adapter/bridge and Cargo.lock
are unchanged against base. Connected worker links runtime-attempt, but this
Browser E2E starts `BrowserHost::start` directly and does not call changed
`plan_candidate`/cached Python or ephemeral process cwd lowering. The retry fails
while awaiting the ready file written after the Browser bridge Hello/HelloAck;
the original failure is later at replacement-document stale validation. Thus no
direct C2 changed-function cause was found; unchanged sources and occasional PASS
are **not proof of independence or of a pre-existing defect**. More Browser
startup/stale-document diagnostics are needed before removing the hold.

The local timeout left its isolated headless Chrome child running because the
start error preceded a constructed BrowserHost. Only that exact test profile's
process was stopped; no user's ordinary browser was touched. No Browser lifecycle
fix is mixed into this CandidateProducer hardening patch.

Windows Unix-API, Ubuntu hosted Python/Node, macOS portable ownership and hosted
validator candidate-never-listened failures retain their existing base evidence.
C2 remains Draft/unmerged; 0304 remote apply = none; deploy = none; live model
calls = 0; #1421 unchanged. No PR D or 6a work was started in this hardening.
