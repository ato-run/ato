# Agent-native Formation — 既存 Rust 機構の再利用計画（次フェーズ）

- Status: 提案（read-only research。コード変更・staging write・deploy なし）。2026-10-28 JST。
- 対象: Remote MCP から「K 候補の形成 → D の探索・選択 → Receipt 確認」を行う次フェーズの実装計画。
- 結論: intent → K → D → attempt → receipt は既存 Rust 機構で成立している。必要なのは新しい engine ではなく、MCP に不足している小さな read/selection 面（D 列挙、seal 前の K 候補読み取り、frozen-K reform、runtime-network 探索、receipt の正式取得）である。
- 本計画は Portable Capsule v1 のスコープ文書（`docs/ops/portable-capsule-v1-scope-20261028.md`）の §7 と対になる。
- 以下の本文は調査時点の file:line 証拠を含む英語メモをそのまま残したもの。

## 0.1 日本語サマリ（7 ギャップ）

1. **D の列挙が未公開**: formation は選択済み `derivation_ref` のみ返し、export job の `derivation_refs` 以外に D 集合を読む API が無い。
2. **seal 前の K 候補読み取りが無い**: `get_formation_job.result` は job ready 後のみ。形成中の候補 K と observation を bounded に読む tool が無い。
3. **frozen-K reform の user API が無い**: CLI `ato reform` と admin reformation のみ。
4. **Runtime-Network 探索が CLI 専用**: `ato form --runtime-network` 相当が MCP に無い。
5. **receipt 確認が間接的**: `wait_formation_job` は compact summary。full receipt は `get_formation_job` / `get_import`。formation receipt に `bundle_sha256` が無いのは仕様。
6. **object-graph の private 登録が MCP に無い**: `/v1/capsule-object-graphs` + CLI `ato upload` のみ。
7. **認証境界**: MCP は management OAuth（apps:read/write）。user-owned Runtime token は別設計で、新しい auth system は作らない。

最小の次ステップ（新 engine なし）: (a) formation job result の bounded read tool、(b) 宣言済み D の列挙 read、(c) receipt 確認手順の明文化（`wait_*` 依存をやめる）、(d) frozen-K reform / runtime-network は要件判断後に既存 SatisfyRequest / binding を包む tool として追加。

---

- Trees examined (read-only):
  - ato: `.tmp/portable-capsule-20261028/ato` at `17442ceb5` (stg `9fbfe8f7f` + fixture fix). During research origin/stg moved to `4988e265f` (merge of ato#1609); that commits only the two fixture lines, so the files cited below are unchanged apart from `ephemeral.rs`/`state_bindings.rs` test fixtures.
  - api: `.tmp/portable-capsule-20261028/api` at task-7 branch `cc37c8f9` (`fix/mcp-file-fetch-fail-closed`); origin/stg `6ce3827e`. The MCP route file is being edited by task-7; line numbers for `management_mcp.ts` may shift, tool names are stable.
  - pwa: `.tmp/portable-capsule-20261028/pwa` at `d57e67d2` (origin/stg).
  - Host: `ubuntu-sugamo` inspected read-only (services and processes only).

## 0. Conclusion

The existing Rust mechanisms already implement the whole intent -> K candidate -> D -> attempt -> receipt chain, and the repository already ships a Remote MCP server for app management (`/v1/mcp`, server name `ato-app-management`, 45 tools). The next phase therefore does **not** need a new engine, canonicalizer, queue or table. What is missing is **(a) read/selection surface** for a few things the MCP server does not expose (D enumeration, K-before-seal, frozen-K reform, runtime-network search) and **(b) confirmation that the existing receipt is reachable through those tools**.

Top gaps (detail in sections 4-5):

1. **D enumeration is not exposed anywhere user-facing/MCP.** Formation returns one selected `derivation_ref`; export returns a bundle-level `derivation_refs` list; `get_app_schema` returns no K/D at all. A remote agent cannot list the D set of a formed app and choose explicitly.
2. **K candidate read before seal is missing.** `get_formation_job` exposes `result.contract_ref`/`derivation_ref`/`verification_receipt` (after the job is ready), but there is no tool that returns the candidate BoundContract K (or a bounded summary of its observations) before/while Formation runs.
3. **Frozen-K D candidate (reform) has no user API.** `ato reform` is CLI-only (`apps/cli/src/lib.rs:2659-2663`); the API has only an admin re-formation endpoint (`api/src/routes/admin.ts:959`).
4. **Runtime-Network search/exploration is CLI-only.** `ato form --runtime-network` (budgets, decision provider, route selection) has no MCP tool; hosted `create_app_schema` runs a different, implicit formation path.
5. **Receipt confirmation is reachable but indirect.** The full receipt is exposed via `get_formation_job.result` and `get_import`; `wait_formation_job` returns only a compact terminal summary, so any MCP loop must call `get_formation_job` afterwards.
6. **No MCP tool for capsule-object-graph upload / private registration** (`/v1/capsule-object-graphs`), so today that path stays CLI/PWA-only.
7. **MCP auth scope is OAuth access-token based (`apps:read`/`apps:write`)**, which is sufficient for existing hosted formation/export/import routes but is not the Runtime/worker token path; remote exploration on a user-owned Runtime would need an explicit design, not a new auth system.

## 1. K candidate formation: the existing chain and the exact gates

### 1.1 Chain (all Rust, all existing)

1. Source + intent arrive as `capsule.toml/2` (or MCP `files`/`zip`/`archive` source, see section 4).
   - `lib/formation/src/capsule_toml_v2.rs:360` `parse_capsule_toml_v2`, and `:374` `parse_capsule_toml_v2_derivation_candidate` for a single D candidate.
2. Draft is bound against the verified source closure:
   - `lib/formation/src/authoring.rs:665` `bind(draft, BindingContext)` returns `(BoundContract, BoundDerivation)`.
   - `BindingContext.source_closure_ref` (`authoring.rs:655-662`) is the resolved source tree; `bind_derivation` (`authoring.rs:737`) refuses anything but the workspace-root input (`authoring.rs:750-771`).
   - `bind_derivation_against_contract` (`authoring.rs:683`) is the frozen-K transformation path; it never re-derives K.
3. Identity is canonical bytes: `BoundContract` (`authoring.rs:466-471`), `BoundDerivation` (`authoring.rs:563-599`), `digest_of` = JCS + SHA-256 (`authoring.rs:601-606`), `contract_ref()` (`authoring.rs:611`), `derivation_ref()` (`authoring.rs:617`).
4. The worker builds/realizes the candidate and observes it: attempt run in `lib/runtime-attempt/src/attempt.rs` (receipt context at `:44-46`, `:83`; verification stored at `:475-531`).
5. One verification assembly point: `lib/runtime-attempt/src/verification.rs:114-147` `verify_observed_candidate` -> `verify_runtime` -> `ContractVerificationReceipt::for_attempt` -> attaches `execution` evidence.
6. Result is composed and published: `apps/formation-worker/src/job.rs:579-604` (`compose_result`, `formation-result-v2.json`, `publish_result`) and posted to `POST /v1/internal/formation/results` (`apps/formation-worker/src/api.rs:544`; route `api/src/routes/internal_formation.ts:1108`).

### 1.2 The exact gate: seal admissible vs run acceptance

- `lib/formation/src/verify.rs:76-86` `ObservationOutcome` = `Satisfied | Deferred { by } | Failed { code, detail }`.
- `verify.rs:106-108` `passed()` is defined as `seal_admissible()`.
- `verify.rs:111-115` **seal admissible** = no observation is `Failed`. `Satisfied + Deferred` is admissible, and every remaining gate is named by `by`.
- `verify.rs:121-125` **fully_satisfied** = every verdict is `Satisfied`. Receipts of an actual Run require this stronger result; a deferred Formation verdict is deliberately not launch success.
- `Deferred` is only produced when readiness is derived from the same Contract observation: `lib/formation/src/projection.rs:16-40`.
- Attempt outcomes are reported separately, never folded: `lib/formation/src/request.rs:217-247` (`seal`, `runtime_verification`, `cleanup`, `publication`; `AttemptStatus` at `:119-128`, `FormationAttempt` at `:130-170`).
- Consequence for the MCP plan: a K candidate formed without running the app can legitimately be sealed with `Deferred` observations. A receipt that claims `fully_satisfied = true` must come from an actual attempt run (hosted/hosted runner/local runtime), not from formation alone.

### 1.3 Receipt shape (single Rust-owned schema)

- Schema ids: `ato.contract-verification-receipt/1` and `/2` (`lib/formation/src/verify.rs:49-50`).
- `ContractVerificationReceipt` (`verify.rs:305-319`): `schema`, optional `bundle_sha256`, `contract_ref`, `derivation_ref`, `target.kind`, optional `execution`, `observations[]`, `fully_satisfied`.
- `VerificationExecutionEvidence` (`verify.rs:249-291`): `realization`, `runtime_executable`, `runtime_version`, `pid`, `container_id`, `image`, `platform`, `endpoint`, `run_id`, `lease_id`, `attempt_id`, `request_id`, dependency/portability fields, `services[]`.
- A Formation candidate has **no bundle digest**: `bundle_sha256` is absent and must not be invented (`verify.rs:307-311`, `:438-457`). Receipt confirmation of a K candidate therefore keys on `contract_ref` + `derivation_ref` + `attempt_id`, not on `bundle_sha256`.

## 2. D exploration and planning: what exists, and explicit-only selection

### 2.1 Formation side (a search, not a fallback)

- Requester plans each D; the coordinator turns them into `candidates = D x environments`: `apps/formation-worker/src/runtime_network.rs:1-20` (module doc states the authority split: requester computes identities, Runtime attests, coordinator only matches typed facts).
- Search policy, budget and reduction admission: `lib/formation/src/exploration.rs:44` `ExplorationPolicy`, `:138` `SubmissionStatus`, `:202` `admits_reduction`.
- Exploration attempts are realized through the normal builder/executor, never a mock: `lib/runtime-attempt/src/exploration_realizer.rs:1-30`.

### 2.2 Portable export side (no new D is derived)

- `apps/portable-application/src/portability_plan.rs:46-51` `plan_portable_export` starts from `validate_all_derivations(bundle)`; it only sizes/decides thin|cached|offline payloads. Planning and packing do not change K or D (`portability_plan.rs:43-45`).
- `apps/portable-application/src/lib.rs:647-655` validates every declared D; `:674` validates the selected one; `:803` `validate_selected_derivation`.
- Export job reports bundle-level identity: `api/src/routes/portable_application_exports.ts:235-260` (`bundle_sha256`, `contract_ref`, `derivation_refs`, `download_url`).

### 2.3 Selection is explicit-only (matches the architecture requirement)

- Multi-D bundle with no selection fails closed: `apps/portable-application/src/local_instance.rs:41` and `apps/cli/src/lib.rs:2485` (`portable Capsule declares N derivations; select one with --derivation`).
- The selected D is a caller input, never inferred: `apps/portable-application/src/portable_run.rs:1502` (`The one Derivation the caller selected. Never inferred here.`).
- Hosted import requires an explicit ref: `api/src/routes/capsule_bundles.ts:2105-2110` (POST import-app) and `api/src/services/compute_sources/input.ts:127-129` (forwarded verbatim as `authoring.derivation_ref`).
- MCP import tool requires `derivation_ref` (`api/src/routes/management_mcp.ts:956`).

Conclusion: there is no implicit planner/fallback for *selection*. Formation explores candidates internally, but every run/import/export consumer must name the D it consumes.

## 3. Receipt confirmation: what is authoritative today

### 3.1 Rust authority

- Only `lib/runtime-attempt/src/verification.rs:114-147` assembles Contract verdicts + receipt for an executed candidate. Formation, local CLI and hosted validator paths all reach `verify_runtime` (`lib/formation/src/verify.rs:623` definition) through this module or `attempt.rs`.
- `tools/receipt-authority` exists as a separate WASM-buildable authority crate (cited in `docs/archive/2026/ops/formation-generalization-inventory-20261023.md:36`): TypeScript must not re-decide `fully_satisfied`.

### 3.2 Hosted ingress (staging)

- Capsule bundle validation (portable .capsule graph): `api/src/routes/capsule_bundles.ts:2253` claim, `:3086` content, `:3102` blobs, `:3492` ack; validator auth via `CAPSULE_VALIDATOR_AGENT_TOKEN(_NEXT)` (`api/src/bindings.ts:698-700`, checks at `capsule_bundles.ts:177-186`).
- Rust validator agent: `apps/portable-application/src/validator_agent.rs:97` `run_once`, `:350` `run_forever`, `:1087` `claim`, `:1114` `claim_runtime`, `:1132` `claim_export`.
- Formation attempt/result ingress: `api/src/routes/internal_formation.ts:242` jobs, `:357` attempts, `:518` portable bundle upload, `:1051` outcome, `:1108` results; worker side `apps/formation-worker/src/api.rs:544`.
- User-visible projection: `api/src/services/formation/job_outcome.ts:32-48` (K/D/bundle/receipt) and `:81-96` (`contract_ref`, `derivation_ref`, `portable_bundle_sha256`, `fully_satisfied`, full `verification_receipt`, observation summary); served by `api/src/routes/compute_instances.ts:352-384` (additive `result`) and `api/src/routes/formation_wait.ts:19-21` (`/wait`).

### 3.3 Services observed on ubuntu-sugamo (read-only)

- `ato-capsule-validator.service` (active) - independent portable Capsule graph validator.
- `ato-portable-validator.service` (active) - portable application capsule validator.
- `ato-runtime-validator --api-url https://staging.api.ato.run` process (active).
- Hosted runner slots 1-6 active (`ato-hosted-runner-slot*.service`), plus staging-specific runners. This is the evidence that receipts come from real validators/runtimes, not from TS.

## 4. Remote MCP surface that exists today

- Mount: `/v1/mcp` -> `api/src/index.ts:1057` (management MCP). Existing companion MCP: `/v1/controller/mcp` -> `api/src/index.ts:1055` (activity operation MCP).
- Auth: `requireManagementAuth()` + same-origin for app mutations (`api/src/routes/management_mcp.ts:1038-1050`); each tool declares its required scope array (examples at `:607`, `:645`, `:652`).
- Server identity and agent instructions: `management_mcp.ts:1108-1117` (`ato-app-management`, treats source/build output as untrusted).
- 45 tools. Formation/export/import relevant names with definition lines:
  - `create_source_archive` `:637`, `get_source_archive_admission` `:645`
  - `create_app_schema` `:652`, `create_app_schema_version` `:658`, `preview_app_schema_source_patch` `:739`, `create_app_schema_version_patch` `:753`
  - `get_formation_job` `:664`, `wait_formation_job` `:670`
  - `get_app_schema` `:682`, `get_app_schema_source` `:688`, `list_app_schema_source_files` `:698`, `search_app_schema_source` `:711`, `read_app_schema_source_file` `:725`
  - `search_apps` `:794`, `fork_app` `:811`, `add_app` `:817`, `create_instance` `:823`
  - `check_library_exportability` `:862`, `export_app` `:870`, `get_export` `:893`
  - `get_import_validation` `:925`, `import_app` `:933`, `complete_import_upload` `:960`, `get_import` `:968`

Gap-relevant facts: `get_export` already reports `bundle_sha256`, `contract_ref`, `derivation_refs`, `download_url` (`management_mcp.ts:897`); `get_import` reports `verification_status` and the full `verification_receipt` (`:985`); `wait_formation_job` is explicitly a compact summary (`:680`).

## 5. Mapping: MCP intent -> existing mechanism -> status -> minimal next step

| # | MCP intent | Existing mechanism (file:line) | Status | Minimal next step |
|---|---|---|---|---|
| 1 | Deliver intent + source | hosted source intake `files`/`zip`/`archive` (`api/src/services/compute_sources/input.ts:90-120`) and `POST /computes/from-source` (`api/src/routes/compute_sources.ts:69`); MCP `create_app_schema` (`management_mcp.ts:652`) | EXISTS | none; verify `files` mode end to end |
| 2 | Form a K candidate (build + observe) | formation job flow (`internal_formation.ts:242/357/1051/1108`), worker `job.rs:579-604`, verification `verification.rs:114-147` | EXISTS | none (hosted formation already used by MCP) |
| 3 | Read the candidate K (contract_ref + observations) before/while forming | job `result` after ready (`job_outcome.ts:32-95`) | PARTIAL | add a read tool returning `contract_ref` + observation ids/outcomes from the job (bounded); no new storage |
| 4 | Enumerate declared D of a formed app | `derivation_refs` exists only on export job (`portable_application_exports.ts:235-260`); `get_app_schema` returns no K/D (`api/src/services/compute_sources/service.ts:945-966`) | MISSING | expose declared D list (from bundle/formation result) on schema read or a new read-only MCP tool; reuse stored derivation refs, no new table |
| 5 | Run D exploration on a user-owned Runtime | `ato form --runtime-network` (CLI) + `apps/formation-worker/src/runtime_network.rs:1-20`; coordinator routes `internal_formation.ts` | MISSING (MCP) | decide whether remote exploration is in scope; if yes, add a bounded submit+poll MCP tool over the existing SatisfyRequest endpoints, not a new engine |
| 6 | Derive a new D against a frozen K (reform) | `bind_derivation_against_contract` (`authoring.rs:683`), CLI `ato reform` (`apps/cli/src/lib.rs:2659-2663`), admin reformation (`api/src/routes/admin.ts:959`) | PARTIAL (CLI/admin only) | add a user-scoped hosted endpoint/tool only if frozen-K reform is required for remote agents; reuse binding + attempt machinery |
| 7 | Confirm receipt (fully_satisfied, attempt linkage) | `verify.rs:305-319`, `verification.rs:114-147`, `job_outcome.ts:83-95`, `get_import` receipt (`management_mcp.ts:985`) | EXISTS | document that agents must call `get_formation_job`/`get_import` after `wait_*`; no new receipt |
| 8 | Export bundle (cross-account artifact) | export jobs `api/src/routes/portable_application_exports.ts:277+`, MCP `export_app`/`get_export` (`:870`/`:893`) | EXISTS | none |
| 9 | Import bundle + get verification | `capsule_bundles.ts:2105-2110`, MCP `import_app`/`get_import` (`:933`/`:968`) | EXISTS | none |
| 10 | Private registration via object graph | `POST /v1/capsule-object-graphs/*` (`api/src/routes/capsule_object_graphs.ts`) + CLI `ato upload` (`apps/cli/src/lib.rs:1399`) | MISSING (MCP) | out of scope for K->D->receipt loop; add only if private registration is required without PWA |
| 11 | Auth for remote MCP client | management OAuth access token + scopes (`management_mcp.ts:1038-1050`, tool scopes) | EXISTS (hosted) | no new auth; user-owned Runtime tokens stay a separate design |

## 6. Not building (deliberate)

- No new canonicalizer or ContractRef/DerivationRef computation outside Rust. Digests are defined only in `authoring.rs:601-613`; MCP/TS must pass refs through (`job_outcome.ts:32-33` states the receipt stays Rust evidence).
- No new wire version, bundle profile, `.capsule` format, or transport; v3/v4 and the export artifact are already the format (`DECISION.md`, `.tmp/portable-capsule-20261028/`).
- No new engine, planner, scheduler or fallback: explicit D selection stays mandatory (`portable_run.rs:1502`, `local_instance.rs:41`).
- No new DB tables/queues: reuse `formation_jobs`/`formation_attempts`/results, `capsule_bundles` + validation jobs, `portable_application_exports`/`imports`, `compute_schemas`.
- No TypeScript Contract verification, receipt minting, or `fully_satisfied` decision.
- No MCP-side source tree copy or file fetch outside the bounded source tools; no new file-path parameter that bypasses task-7 fail-closed handling.
- No second auth system for MCP; if user-owned Runtime exploration is needed, design the credential scope as a separate, explicit decision.

## 7. Risks and limits

1. **Authority drift risk**: any new MCP tool that returns K/D/observations must proxy Rust output. Recomputing JCS or refs in TS would be a semantic fork (`authoring.rs:601-613`; workspace AGENTS.md: TS may route/display, not canonicalize).
2. **Deferred vs fully_satisfied confusion**: formation can seal with `Deferred` (`verify.rs:111-115`) while a receipt for a Run requires all `Satisfied` (`:121-125`). An agent loop that treats a formation result as `fully_satisfied` would be wrong; the job result carries `contract_verification` separately from `verification_receipt` (`job.rs:917-942`).
3. **bundle_sha256 absence is legitimate** for formation receipts (`verify.rs:307-311`); confirmation logic must not require it.
4. **Task-7 fail-closed MCP file/upload changes** are landing in the same file (`management_mcp.ts`, +465 lines on branch `cc37c8f9`) and touch client-supplied file URLs and bounded reads. The plan must build on those bounds; any new read tool must reuse the existing bounded source readers rather than add a path parameter.
5. **Host operations**: validators and 6 hosted runner slots share `ubuntu-sugamo`; the host ran at 98-99% disk during this phase (a 1.7G build target had to be removed). No rebuild/restart was performed for this research.
6. **Unverified by this task**: no MCP call was executed and no staging write was made (read-only mandate). Tool names/line numbers are from the code above; the tool list was not exercised live.
7. **stg movement**: origin/stg advanced from `9fbfe8f7f` to `4988e265f` (merge of ato#1609, fixture-only) while this research ran; api origin/stg is `6ce3827e` while the local api worktree carries task-7 branch `cc37c8f9`.

## 8. Verification plan for the next phase (commands, not yet run)

These are the minimal checks that would turn the EXISTS rows into verified facts. They require Lead approval because they touch staging with new accounts/apps.

```sh
# 1) MCP handshake and tool list (read-only)
curl -s https://staging.api.ato.run/v1/mcp -H "Authorization: Bearer $ATO_MCP_ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}"

# 2) K candidate formation from files (hosted) -> get_formation_job.result
#    expect result.contract_ref, result.derivation_ref, result.contract_verification, result.verification_receipt
#    (tools: create_app_schema -> wait_formation_job -> get_formation_job)

# 3) Receipt confirmation (import path, fully_satisfied=true)
#    tools: export_app -> get_export (bundle_sha256/contract_ref/derivation_refs) ->
#           import_app(derivation_ref=...) -> get_import (verification_status, verification_receipt)

# 4) Negative: multi-D bundle imported without derivation_ref must be refused
```

Any row still marked PARTIAL/MISSING in section 5 is **not** a verified capability until such a call is executed and its output is recorded.
