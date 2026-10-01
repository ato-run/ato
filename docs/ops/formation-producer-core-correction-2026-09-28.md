# Formation CandidateProducer — core semantics correction

Date: 2026-09-28. This supersedes the semantics and source-access limitation of
[the first-slice checkpoint](formation-provider-split-2026-09-28.md).
**Core/local acceptance only, not original Phase 4 completion.**

## Delivery and source correspondence

- [PR A #1422](https://github.com/ato-run/ato/pull/1422), Draft/open:
  `a4f838db5bf70ef53238ed3223bd69807d0577bc` (documentation only).
- [PR B #1423](https://github.com/ato-run/ato/pull/1423), Draft/open, remains
  based on `docs/formation-provider-split`; no retarget or GitHub merge.
  Core implementation/test commit: `125aff7cbdcd40237741351e3f5a06d63c646e33`.
  A was incorporated with ordinary merge `229016f256c240d49d6a9314c76d8d7548e5289b`;
  no pushed history was rewritten. The subsequent ledger commit changes docs only.
- Refetched ato main: `34c2ef2a8882f6fc53438c1e0f1722b240ed3d6e` (#1420).
- #1421 stays open at `dc0fba4e0ecdd585985320551b52b72768002a5c`, unchanged,
  unexecuted, nonblocking finite-choice evaluation asset.

Full 330-line source read:
`docs/Continuation_Model_rewritten/Continuation Model/Formation.md`
(workspace, not this Git repository). SHA-256:
`7d8a4187db2910eeeb3e34cc8d0325dd19f8588442d34e00a5e5ef8aa12af043`.
Parent Continuation Model SHA-256:
`ce0f0e3c91cefaf0fafe2ce4a48cbcca801739a96cd906b30d13ea4c136d15fb`.

The [companion correspondence table](../rfcs/draft/Formation.md) maps all
original sections and preserves ADR-041's authority boundaries:

| Original | Implementation roadmap |
|---|---|
| Phase 3 — Jev Decision Layer | 5a |
| Phase 4 — LLM Candidate Generation | 5b |
| Phase 5 — Continuous Adaptation | 5c |

6a/6b/6c are **additional product coverage tracks**, not original phase numbers.
6a known-D measurement does not wait for 5b. No original document or historical
ADR-037–040 was rewritten. No existing Capsule identity/wire profile changed.
The sign-in/full-text-unverified limitation is resolved, not a current caveat.

## Corrected interfaces and semantics

- `DecisionProvider::decide(&DecisionPoint) -> ProviderAnswer` is unchanged:
  finite offered `choice_id`, Attempt / Inspect / Stop. No proposal, D, argv, K
  or PASS output. **EscalateToCandidateProducer remains unexposed** until durable
  receiver and requester integration. Only Verifier establishes `C' ⊨ K`.
- `CandidateProducer::propose(&ProposalRequest) -> Result<ProducerOutput,
  ProducerError>`. `ProducerOutput` has private bounded raw bytes (16 KiB max)
  and bounded provenance. FixedCandidateProducer uses that same raw boundary.
  Only Ato parses the strict envelope and individual typed proposals; preserving
  original JSON bytes catches duplicate keys. Bad members do not discard valid
  members; bad envelopes reject the batch. No trusted provider-created batch.
- Request v0 **has no source_context or SourceExcerpt**. Unknown-field parsing
  rejects their injection; serialized fixed requests contain no source text or
  private file/module resolutions. Source policy remains false/zero; only PR D
  may add validated explicit source opt-in. No transport is enabled here.
- `FrozenSearchV1.initial_source: Option<InitialSource>` is an opt-in DTO with
  `closure_ref` and `archive_digest`, independent of all candidate D. It is not
  a semantic primitive. Both are validated SHA-256 refs under proposal policy.
  With no proposal policy, the field must be absent and candidates >=1;
  canonical legacy frozen bytes are unchanged. Old bytes are not reinterpreted.
  This additive, omitted-when-absent field avoids a new SearchState version:
  zero-candidate behavior requires explicit new policy plus I and valid frozen K.
  Old receivers must not receive this opt-in until PR C/integration exists.
- `ProposalAuthorization` owns `modifiable_derivation_refs`, `source_domain`
  (opaque entrypoint/module ID resolutions), `python_http_process` constructor
  parameters, and bounded policy. It is not serialized to the producer.
- Strict `ato.formation-proposal/1`:
  - **ProposeDerivation** accepts only `python_http_process@1 {entrypoint_id}`
    in v0. I + frozen K + owner authorization construct a complete new
    AuthoringDraft through the existing parser/binder; no known D, base recipe,
    preset or fake D is consulted. Injected base_ref is an unknown field.
  - **ModifyDerivation** requires an explicit authorized known base ref and
    matching private recipe from a ref-keyed map. Existing `python_script@1`
    and `python_module@1` reuse the old compiler **only here**. Base identity,
    source scope and same K are checked; no implicit fallback base.
  - **Unsupported** has no operations and creates no candidate/evidence.
- Ato pins Python version, process/serve, HTTP port mapping, cwd `.`, denied
  network and pure effect. Provider only selects an owner-issued opaque ID,
  never a path, argv, executable, port, runtime version, Binding or permission.
  v0 supports frozen HTTP GET conditions plus optional exact workspace digest;
  unsupported K/port mappings fail closed. Required capability facts reuse
  requester planning's common execution-requirements function. Runtime
  admission is still required; these facts are not a K verdict.
- Proposal ID is explicitly a **search-local content key**:
  `proposal-content:sha256:<JCS(schema, typed proposal)>`. Persist only paired
  with search_id/frozen domain; opaque ID resolutions are search-local, so this
  is **not** global semantic identity. Modify includes its base in hashed bytes.
  Identical proposals in the same frozen domain yield deterministic ID/D;
  distinct base-relative proposals have distinct IDs. D identity is DerivationRef.
- Ato validates schema, catalog/domain, authorization and safe scope; binds and
  compares generated K byte-identically to frozen K; computes D; deduplicates;
  admits to the separate registry. Compiler output is not a receipt and the
  independent receipt ABI rejects it.

## Scheduling, budget and persistence boundary

Known candidates remain frozen. Registry + completed round read model exposes
known/generated candidates through the **same** effective iterator for default
search and DecisionProvider. Zero known D is immediately proposal-eligible;
nonempty known frontiers are considered before opening the round. Modify-only
operations are not offered without authorized known bases.

UNKNOWN / owner stop / inflight work / exact Runtime / deadline / cumulative
attempt-and-byte budget barriers are retained. One proposal round, max four
proposals, timeout <=30 s. A claimed/open read-model round waits or expires;
completed/error/timeout rounds cannot reopen. Unsupported/error/timeout creates
no synthetic attempt or K failure and ends boundedly when no candidates remain.

**No durable storage implemented.** Read-model serialization is not DB persistence.
No provider invocation is connected to a live requester. Claim-before-call,
revision CAS, one concurrent winner, no retry after crash, write-once generated
recipe persistence, source-inventory verification, actual timeout enforcement
and receipt acceptance remain deferred. The raw ABI's `base_recipes` lookup is
keyed by explicit authorized D; it is empty for base-free compilation.
0303 unchanged; **0304 not created; PR C not started; migration = none**.

## Local core acceptance

`lib/formation/tests/proposal.rs` has **41 passed / 0 failed** (23 prior
regressions retained/adapted, 18 added). These are not actual Runtime acceptance.

| Case | Result |
|---|---|
| B0 | PASS: old no-policy frozen canonical bytes/behavior, empty frontier refused |
| B1 | PASS: zero known D + independent I/K/authorization opens round; incomplete input refused |
| B2 | PASS: first canonical D without base recipe/known D/preset |
| B3 | PASS: generated K canonical bytes and ContractRef equal frozen K |
| B4 | PASS: Modify accepts explicit authorized known base only |
| B5 | PASS: base_ref injection into Propose rejected |
| B6 | PASS: known but unauthorized Modify base rejected |
| B7 | PASS: fixed raw duplicate/unknown fields rejected by Ato; no source_context wire field |
| B8 | PASS: same frozen-domain proposal gives same ID/D; irrelevant base recipes cannot affect Propose |
| B9 | PASS: two different authorized bases yield different modification IDs/Ds |
| B10 | PASS: zero-D unsupported terminates CandidatesExhausted without K evidence |
| B11 | PASS: generated first Ds visible to both deterministic and finite-choice scheduling |
| B12 | PASS: compilation does not create PASS/attempt evidence; native receipt ABI rejects compiler output |

Additional coverage: strict envelope duplicates, mixed batches, no Modify catalog
without bases, incompatible K, provider-controlled parameter injection, inflight
barrier, zero-D stop/deadline/byte/attempt budgets, timeout/error no retry,
rehydration/scope/dedup, exact Runtime, policy bounds and existing known-D path.

Full selected-package regression (same set as previous checkpoint):

```sh
TMPDIR="$PWD/.tmp" cargo test -p ato-formation -p ato-receipt-authority -p ato-formation-worker --locked
TMPDIR="$PWD/.tmp" cargo clippy -p ato-formation -p ato-receipt-authority -p ato-formation-worker --all-targets --all-features --locked -- -D warnings
TMPDIR="$PWD/.tmp" cargo build -p ato-receipt-authority --target wasm32-unknown-unknown --release --locked
cargo fmt --all --check
git diff --check
```

- Regression **567 passed / 0 failed / 1 existing ignored** across 42 test
  binaries/doc-test result groups (prior checkpoint 548/0/1; +19 tests).
  Existing ignored `export_source_hardening_request` requires manual Linux
  Network acceptance; no new skip was added. Native authority unit suite: 5/0.
- Clippy, formatting/diff checks and release WASM compile passed.
- Local WASM SHA-256:
  `0683a74b75419f359007e382c5069f08f380513e98eef07791b83f6add923f3c`.
  Not installed into ato-api or deployed.
- This is not a full workspace regression, Windows local run, durable integration
  or new small-OSS actual Runtime acceptance.

## CI baseline comparison (not green)

Actual job logs were read, not inferred from changed paths. No reruns, skips or
unrelated fixes were introduced; these failures are not billing exceptions.

| PR observation | Same workflow/platform main evidence | Matching failure |
|---|---|---|
| PR A corrected head `a4f838db…`, [Rust CI 36371783065](https://github.com/ato-run/ato/actions/runs/36371783065), windows-latest | main `34c2ef2a8882f6fc53438c1e0f1722b240ed3d6e`, [Rust CI 36359409659](https://github.com/ato-run/ato/actions/runs/36359409659), windows-latest | `runtime-attempt/browser_sandbox.rs` Unix-only APIs/imports |
| Same A run, ubuntu-latest | Same main Rust CI, ubuntu-latest | `hosted_python_and_node_use_the_common_process_runtime` |
| Same A run, macos-latest | Same main Rust CI, macos-latest | CLI `a_process_run_is_owned_by_its_run_until_stopped`; hosted validator b/c/d/g1/g2 candidate-never-listened failures |
| PR B prior head `a9d888fb8629685bd85ee7e3276d4c79b5d7efa1`, [computation CLI 36369732202](https://github.com/ato-run/ato/actions/runs/36369732202), windows-latest | main `660dbf6eaedc3384017137f1f20282a359d423aa`, [computation CLI 36357618146](https://github.com/ato-run/ato/actions/runs/36357618146), windows-latest | same Unix-only browser_sandbox compile errors |
| Same B CLI run, macos-latest | Same main CLI run, macos-latest | `portable_application::a_process_run_is_owned_by_its_run_until_stopped` at line 54, 14 passed / 1 failed |

These failures are evidenced on main independently of proposal changes; this
is not proof all CI is green or a new-head CI success claim. PR B corrected-head
CI is a separate observation from the prior-head baseline comparison.

## Status and next step

| State | Result |
|---|---|
| implemented | PR A source/phase mapping; PR B base-free I/Proposal/raw compiler/registry core |
| locally/integration verified | B0–B12 + 567 local regressions; native ABI/WASM build; durable/full-path integration NOT RUN |
| merged | Neither A nor B; both Draft/open |
| deployed | None; no flag change or remote migration |

Original Phase 4 / roadmap 5b still requires **no Preset, no known D, small OSS
fixture → producer → new D → admission → actual Runtime → Verifier → same K PASS**.
Changing an existing D's entrypoint, or passing these unit tests, does not meet
that gate. Corrected actual P0–P11 remain NOT RUN.

Next is **review of A/B**, not PR C implementation. After explicit merge approval:
A merge → B main retarget/review → durable receiver/migration → requester
integration → FixedCandidateProducer actual P0–P11 → general LLM adapter PR D.
Live model calls = **0**. No Jev E2/oracle/72 cells, source patch generation,
autonomous shell/agent loop, migration or deploy executed.
