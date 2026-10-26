# Formation provider split — partial implementation checkpoint

Date: 2026-09-28. **Historical first-slice checkpoint, superseded by the
[core-semantics correction](formation-producer-core-correction-2026-09-28.md).**
The typed producer return, implicit-base Propose behavior and CI uncertainty
below describe the earlier heads, not the current implementation.
**Overall end-to-end request remains incomplete.** This records PR A
architecture correction and the first PR B pure-core slice, not full 5b
acceptance. Nothing was merged or deployed by this work.

## Sources and exact baseline

- ato origin/main: `34c2ef2a8882f6fc53438c1e0f1722b240ed3d6e` (#1420 merged).
- ato-api origin/main inspected read-only:
  `d2bba669ce0ad2b6ac9728f9f306018adb5ce5f8`.
- [PR A #1422](https://github.com/ato-run/ato/pull/1422):
  `a67b758ecf5b49df233986b46bfecbea869ddb97`, Draft, documentation only.
- PR B implementation commit:
  `af3d44bac0a68fefc72c051363a22a2002ea6a68`; stacked on PR A. The later
  checkpoint documentation commit does not change the tested Rust source.
- [#1421](https://github.com/ato-run/ato/pull/1421) remains OPEN at
  `dc0fba4e0ecdd585985320551b52b72768002a5c`. No changes or execution.

Original-document correspondence is now verified from the downloaded full
330-line `Continuation Model/Formation.md` and the parent Continuation Model.
See the [correction record](formation-producer-core-correction-2026-09-28.md)
for hashes and phase correspondence. The earlier hosted URL access limitation
no longer applies. The original documents and historical ADR-037–040 are not
edited. The [Formation companion](../rfcs/draft/Formation.md) remains a
repository mapping, not a replacement for the original note.

## Roadmap before / after

| Before | After |
|---|---|
| 5a finite Jev Decision | 5a Decision Layer, completed historical gate preserved |
| 5b Jev typed-generation / E1–E2 efficacy critical path | 5b general LLM Candidate Generation, fixed producer first |
| Implicit adaptive generation/search loop | 5c bounded Adaptive Formation Loop, pending |
| 6 coverage after generation | 6a known-D measurement of 20 apps, independent of 5b |
| Undifferentiated coverage expansion | 6b measured capability expansion 20→50→100; 6c event-driven revalidation |

E1/E2 are finite-choice selection evaluation assets, not general LLM efficacy.
#1421 does not block CandidateProducer. Historical compiler, same-K checks,
claims, fences, context/1–2, point/1–3, prompt/1–3 and ledgers are retained.
The 20-app coverage measurement itself was not executed in this work.

## Implemented core and authority boundaries

- Existing DecisionProvider remains `decide(&DecisionPoint) -> ProviderAnswer`:
  finite `choice_id` over Attempt / Inspect / Stop. Both deterministic search
  and AllowedChoices consume `SearchStateV1::candidates()`, now known plus
  admitted proposal candidates. Neither provider result establishes PASS.
- New provider-neutral `CandidateProducer::propose(&ProposalRequest) ->
  Result<ProposalBatch, ProducerError>` and `FixedCandidateProducer`. Request
  holds read-only K/Runtime constraint, known refs, source/evidence slots,
  catalog and remaining budget. No external adapter or production request
  projection is enabled; the fixed producer is exercised in pure tests only.
- Strict `ato.formation-proposal/1`: propose_derivation, modify_derivation
  (authorized base only), unsupported. Unknown fields, duplicate JSON member
  keys and arbitrary operations/arguments are rejected. Malformed members
  do not discard other valid proposals in a bounded valid envelope.
- Catalog: `python_script@1 {entrypoint_id}` and
  `python_module@1 {module_id}`. Private owner maps resolve IDs. The compiler
  constructs script or module argv, never accepts provider-authored argv.
  `static_http@1` is not implemented. v0 allows exactly one serving operation
  per proposal, no source patches or multi-step shell program.
- Ato validates frozen authorization, catalog/argument domains, pure/no-Binding/
  denied-network scope, and the original base identity. It reuses the existing
  AuthoringDraft/bind compiler, compares unchanged K, round-trips canonical D,
  computes proposal identity, and deduplicates against known/earlier admitted D.
  D refs are computed *after* the proposal, not precomputed offered selections.
- The bounded Rust authority ABI exposes `compile_proposals`; TypeScript does
  not reimplement canonicalization. Compiled output is not a verification
  receipt and is rejected by the independent receipt ABI.
- Policy ceiling is one round, at most four proposals, timeout ≤30 seconds.
  Source text is fail-closed disabled in this slice (`allow_source_text=false`,
  `max_source_bytes=0`), pending PR D's explicit opt-in and privacy projection.
- Search read model has a separate proposal round and immutable generated
  candidate vector. It checks scope, dedup, bounds, Exact Runtime, UNKNOWN,
  owner stop, deadlines and budgets; provider timeout/error does not create
  attempt or K-failure evidence. Historical generation fields keep semantics.

## Not implemented / not verified end to end

**Generated candidate persistence is not implemented.** Serialization/replay
of the read model in unit tests is not durable storage or crash acceptance.
The API claim/completion transaction, one-winner concurrency, revision CAS,
no-call-before-claim, post-claim no-retry, and write-once rows remain PR C work.
0304 was unused in the inspected API main but is not reserved or created;
check numbering again before implementing it. 0303 remains untouched.

Requester policy enablement, immutable source inventory checks for the new
catalog, durable claim before producer invocation, actual timeout enforcement,
result posting/recompilation, ticket/recipe rehydration and requester receipt
acceptance for the new batch are not connected. Do not enable new proposal
policy against the existing receiver. No EscalateToCandidateProducer choice is
exposed yet; it must wait for that integration and remaining round budget.

General LLM adapter/live acceptance is PR D, not this fixed core slice. Audit
found existing `src/services/llm_provider.ts` in ato-api (AI SDK Anthropic /
OpenRouter hints), but no corresponding general LLM trait/transport in the
inspected Rust Formation requester. Reuse must respect requester ownership;
do not move provider credentials into the Coordinator or build an agent engine.

## P0–P11: actual results, not inferred results

All **corrected full-path actual acceptance cases remain NOT RUN**. The local
checks below are only partial evidence and must not be labeled actual success.

| Case | Local evidence | Actual acceptance |
|---|---|---|
| P0 | Legacy no-policy wire goldens and deterministic regressions pass | Not run |
| P1 | Fixed producer → validator → two new canonical Ds → registry passes | No actual known-D FAIL / durable producer invocation |
| P2 | Finite decision chooses second generated D; scheduler issues only an attempt | No Runtime/Verifier/requester full path |
| P3 | Existing typed evidence mechanisms preserved | New wrong-generated-D FAIL→next-decision→PASS not run |
| P4 | K/base changes, shell, unauthorized op/ID and unknown fields rejected | Compiler tests only; actual receiver not connected |
| P5 | Two valid + duplicate + unauthorized admits exactly two | Compiler and native authority ABI tests only |
| P6 | Error/timeout read model exhausts without manufactured K evidence | No timed transport or durable completion |
| P7 | Completed read model rehydrates without opening another round | Crash-after-claim no-second-call not tested |
| P8 | None for new receiver | New concurrency acceptance not tested |
| P9 | Valid UNKNOWN row blocks next issue; barrier tests pass | No actual Coordinator UNKNOWN path |
| P10 | K and ContractRef unchanged by script/module compiler | No actual known FAIL/generated PASS |
| P11 | Choice issues attempt only; compiler output rejected by receipt ABI | Actual generated-attempt Verifier receipt not collected |

## Local verification

On the implementation commit above, macOS:

- `cargo test -p ato-formation -p ato-receipt-authority -p ato-formation-worker --locked`:
  **548 passed, 0 failed, 1 existing ignored**. Added 23 proposal tests and
  2 native Rust authority tests. OS-conditional regression passes are not
  evidence of a Linux Runtime or deployed Coordinator acceptance run.
- `cargo clippy -p ato-formation -p ato-receipt-authority -p ato-formation-worker --all-targets --all-features --locked -- -D warnings`: PASS.
- `cargo fmt --all -- --check` and `git diff --check`: PASS.
- `cargo build -p ato-receipt-authority --target wasm32-unknown-unknown --release --locked`: PASS (compile only; no WASM runtime integration test or API binary replacement).
- PR A local Markdown links: PASS; ADR-037–040 unchanged.

No E2 oracle, E2 cells, live Jev or live general LLM calls. No credentials
configured. No API/PWA modification, migration application, flag change or deploy.

## Delivery state and next boundary

| State | Result |
|---|---|
| implemented | PR A docs; PR B proposal/compiler/registry/pure scheduling/Rust ABI subset |
| locally/integration verified | Local core and native ABI regressions above; full integration pending |
| merged | Neither PR A nor PR B merged by this work |
| deployed | Nothing deployed; no remote migration |

The subsequent instruction changes the next boundary: correct and review PR A/B
core semantics first. PR C, migration and requester integration must not start
until that review. See the correction record for current state.

## CI observation (not a local regression result)

[PR A Rust CI run 36368536659](https://github.com/ato-run/ato/actions/runs/36368536659)
failed after actual steps ran; this is **not** a billing/spending-limit exception.
Logs identify Windows `browser_sandbox.rs` Unix-only imports, Ubuntu
`hosted_python_and_node_use_the_common_process_runtime`, and macOS CLI
`a_process_run_is_owned_by_its_run_until_stopped` plus five
`hosted_validator_verification` tests (candidate never listened).
These paths were not edited by PR A. An equivalent baseline CI reproduction
has not been performed here, so this record does not assert the failures are
proven pre-existing, nor mark CI green. No skips/fixes/reruns were added.
PR B checks were still running at the last observation; only local results
above are confirmed. Both PRs remain Draft and unmerged.

PR B: [#1423](https://github.com/ato-run/ato/pull/1423), base PR A branch.
Local WASM compile artifact SHA-256 (not installed into ato-api):
`6a689cacbbe77ed7c171e02018e9058960bb635748c39c555f09482d454d8c84`.
