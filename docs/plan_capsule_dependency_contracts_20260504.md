---
title: "Implementation Plan: Capsule Dependency Contracts (RFC v1.5)"
status: planning
date: "2026-05-04"
related:
  - "docs/rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md"
  - "docs/plan_phase13b9_guest_jsonrpc_migration_20260429.md"
---

# Implementation Plan: Capsule Dependency Contracts

RFC `CAPSULE_DEPENDENCY_CONTRACTS.md` (accepted v1.5) を実装に落とすための段取り。

**Source of truth**: `docs/rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md` (accepted v1.5)。draft 版ではなく accepted RFC を実装対象にする。

**目標 (Definition of Done)**: WasedaP2P を新 grammar に書き換え、`ato run` で Postgres dependency が自動起動・接続して FastAPI が serving に入るまでを E2E で通す。Identity invariant 9 項目 (RFC §3 末尾) は CI で動く mock-service E2E / unit / property tests で守り、実 Postgres/WasedaP2P は host-bound manual E2E として別 gate にする。

## 0. Scope と非Scope

**Scope (v1)**: RFC §3 の 11 項目 + invariant 9 項目。
**非Scope (follow-up)**: RFC §2 / §12 で別 RFC に切り出した全項目 (`tool@1`, shared state, transitive identity, refcount, supply chain, secret-identity, credential rotation, credential defaults, observability)。

## 1. 段取り全体像

P0 + 7 phase、依存順。各 phase は単独でビルド可能・テスト可能・PR 1 本に収まる粒度を目標とする。

| Phase | 概要 | 予定 PR 数 | 主要 crate |
| --- | --- | --- | --- |
| **P0** | spec closure: template grammar / parser boundary の実装前確定 | 0–1 (docs only) | RFC / plan |
| **P1** | parser: `[dependencies.*]` / `[contracts.*]` の AST | 1 | `capsule-core` |
| **P2** | env capture model 拡張 (`EnvOrigin`) | 1 | `capsule-core` + `ato-cli` |
| **P3** | lock-time verification (§9.1 13 項目) | 1 | `capsule-core` |
| **P4** | credential resolver + materialization channels (Rule M1–M5) | 1 | `ato-cli` |
| **P5** | runtime orchestration (existing `managed_services` を deps graph 駆動に置換) | 1–2 | `ato-cli` |
| **P6** | identity 畳み込み: receipt の `dependency_derivation_hash` に `(parameters, identity_exports)` を入れる | 1 | `ato-cli` |
| **P7** | E2E: CI mock provider + manual `ato/postgres` / WasedaP2P path + invariant regression test | 1 | `crates/ato-cli/tests/`, registry |

**Critical path**: P0 → P1 → P2 → P3 → P5 → P7。P4 (credential) は P5 と並行可だが P5 の orchestration が credential 経路を呼ぶため、P5 開始時には P4 の channel API が固まっていること。

## 2. Phase 詳細

### P0: Spec Closure Gate (docs only)

**目的**: P1 parser が仮 grammar を実装して後で壊れるのを避ける。RFC §13 のうち parser/AST に直結する open question を、実装前に v1 の最小 subset として閉じる。

**決めること**:
- template namespace の v1 grammar:
  - `{{params.*}}`, `{{credentials.*}}`, `{{env.*}}`, `{{host}}`, `{{port}}`, `{{state.dir}}`, `{{deps.<name>.runtime_exports.*}}`, `{{deps.<name>.identity_exports.*}}`
  - `{{socket}}` は v1 parser では予約 token として扱うが、lock fail-closed 対象
- escape ルール: v1 は raw `{{...}}` のみを template とし、escape は未実装なら parser error にする
- undefined key: parser は token 化のみ、P3 lock verification で fail-closed
- evaluation order: lock 時に parameters / identity_exports、runtime 時に credentials / runtime_exports

**Acceptance**:
- accepted RFC または本 plan に上記 subset が明記され、P1 がその範囲だけを AST として実装する
- P1 の round-trip test は「未確定 grammar を許す」ではなく「v1 subset 以外を reject / reserved として表現する」ことを検証する

### P1: Parser (`capsule-core`)

**目的**: TOML から AST (`DependenciesSpec`, `ContractsSpec`) を構築。lock 検証は P3 で別実装。

**変更**:
- `crates/capsule-core/src/foundation/types/manifest.rs`
  - 新規 struct `DependencySpec { capsule: CapsuleUrl, contract: ContractRef, parameters: BTreeMap<String, ParamValue>, credentials: BTreeMap<String, CredentialTemplate>, state: Option<DepStateSpec> }`
  - 新規 struct `ContractSpec { name: String, major: u32, target: TargetLabel, ready: ReadyProbe, parameters: BTreeMap<String, ParamSchema>, credentials: BTreeMap<String, CredentialSchema>, identity_exports: BTreeMap<String, String>, runtime_exports: BTreeMap<String, RuntimeExportSpec>, state: Option<ContractStateSpec> }`
  - 新規 enum `ReadyProbe { Tcp{...}, Probe{...}, Http{...} /* reserved */, UnixSocket{...} /* reserved */ }`
  - 新規 enum `EndpointSpec { Auto, Fixed(u16), AutoSocket /* reserved */, None }`
  - `Manifest` に `pub dependencies: BTreeMap<String, DependencySpec>` 追加 (top-level)
  - `Manifest` に `pub contracts: BTreeMap<ContractId, ContractSpec>` 追加 (provider 側)
  - **テンプレ文字列の解析**: `{{params.X}}`, `{{credentials.X}}`, `{{env.X}}`, `{{host}}`, `{{port}}`, `{{state.dir}}`, `{{deps.<name>.runtime_exports.X}}`, `{{deps.<name>.identity_exports.X}}` を AST に分解した `TemplatedString` 型を導入
- `crates/capsule-core/src/foundation/types/manifest_v03.rs`
  - normalization で `dependencies` / `contracts` を passthrough
- 新規 `crates/capsule-core/src/foundation/types/dependency_grammar.rs` (大きくなるので分離)

**Out of scope for P1**:
- 値の妥当性検証 (型一致 / required / cycle 等は P3)
- 値の materialization (P4)

**Acceptance**:
- `cargo test -p capsule-core` で AST round-trip テスト追加 (TOML → AST → JSON serialize → 再 parse 一致)
- 既存の v0.3 manifest が壊れない (compat regression)
- P0 で固定した v1 template subset 以外は parser error または reserved token として AST 化され、実装者が runtime で独自解釈できない

### P2: Env Capture Model 拡張

**目的**: RFC §7.4.1 の `EnvOrigin` enum を導入。`runtime_exports` 由来 entry を `intrinsic_keys` から無条件除外する path を作る。

**変更**:
- 新規 `crates/capsule-core/src/execution_identity/env_origin.rs`
  ```rust
  pub enum EnvOrigin {
      Host,
      ManifestStatic,
      ManifestRequiredEnv,
      DepIdentityExport(DepLocalName),
      DepRuntimeExport(DepLocalName),
      DepCredential(DepLocalName, CredKey),
  }
  ```
- `crates/ato-cli/src/application/execution_observers_v2.rs`
  - 既存の `intrinsic_keys` 構築を `Vec<EnvEntry { key: String, value: String, origin: EnvOrigin }>` に拡張
  - `intrinsic_keys` 計算で `DepRuntimeExport(_)` と `DepCredential(_,_)` を `.filter()` で **無条件除外**
  - env_allowlist との関係: allowlist は `Host` / `ManifestStatic` / `ManifestRequiredEnv` / `DepIdentityExport` 由来にのみ適用。`DepRuntimeExport` / `DepCredential` は origin で先に除外され、allowlist が override できない
- `crates/ato-cli/src/application/execution_receipts.rs`
  - receipt schema は変えない (origin は internal model only)。serialize 時に既存形式へ落とす

**Acceptance**:
- 単体テスト: `EnvOrigin::DepRuntimeExport` 由来 entry が `intrinsic_keys` 出力に **絶対に出ない** ことを property test
- 単体テスト: env_allowlist に `FOO` を入れても `EnvOrigin::DepRuntimeExport` 由来 `FOO` は除外される
- 既存 v2 receipt regression が通る

### P3: Lock-Time Verification

**目的**: RFC §9.1 の 13 項目を全て実装。`lock` を生成する path で fail-closed 検証を入れる。

**Brick boundary**:
- `ato-cli` が authority resolution / provider fetch / cache lookup を担当する。
- `capsule-core` は **pure verifier** のみを担当する。network / filesystem fetch / registry policy を持たない。
- `capsule-core` の verifier は、consumer manifest と、ato-cli が既に materialize した provider manifests / resolved refs を入力として受け取る。

**変更**:
- 新規 `crates/capsule-core/src/foundation/dependency_contracts/lock.rs`
  - `pub fn verify_and_lock(input: DependencyLockInput) -> Result<DependencyLock, LockError>`
  - `DependencyLockInput` は `consumer: Manifest` と `providers: BTreeMap<DepLocalName, ResolvedProviderManifest>` を持つ
  - `ResolvedProviderManifest` は `requested`, `resolved`, `manifest` を含む pure data。provider fetch は含まない
  - 13 verification を順番に実行
  - `instance_hash` 計算 (`blake3-128(JCS({resolved, contract, parameters}))[:16]`)
  - reserved variant 検出 → `LockError::ReservedVariantNotImplemented`
  - credentials の literal 検出 → `LockError::CredentialLiteralForbidden`
  - credentials の `default` 宣言検出 → `LockError::CredentialDefaultForbidden`
  - `(resolved, contract, parameters)` uniqueness 検証
  - cycle detection
  - needs ⊆ dependencies 検証
- `crates/ato-cli` 側の lock path:
  - dependency URL を authority に問い合わせて immutable `resolved` に固定
  - provider capsule を fetch/materialize
  - `DependencyLockInput` を組み立てて capsule-core の pure verifier を呼ぶ

**Acceptance**:
- 13 verification それぞれに対する failure テスト + happy path テスト
- `LockError` variants が Display で v1 invariant 番号を含む
- capsule-core unit tests は fake `ResolvedProviderManifest` のみで完結し、network / registry / filesystem provider fetch を必要としない

### P4: Credential Resolver + Materialization (Rule M1–M5)

**目的**: orchestration 直前に credential template を resolve、Rule M1 の materialization channel に流す。

**変更**:
- 新規 `crates/ato-cli/src/application/credential.rs` (既に空のディレクトリがあるのでそこに `mod.rs` を)
  ```rust
  pub struct CredentialResolver { /* host env reader */ }
  impl CredentialResolver {
      pub fn resolve_template(&self, tmpl: &TemplatedString, scope: &EnvScope) -> Result<ResolvedSecret>;
  }

  pub enum MaterializationChannel {
      Stdin { reader: ChildStdinHandle },
      TempFile { path: PathBuf, owner: ChildPid },
      EnvVar { key: String, child_only: true },
  }

  pub fn materialize_credential(secret: ResolvedSecret, channel: MaterializationChannel) -> Result<MaterializedRef>;
  ```
- `MaterializedRef` は temp file path / fd / env key の参照を持ち、provider の `[provision] run` / `[targets.<x>] run` 内の `{{credentials.X}}` を rewrite する
- redaction filter (Rule M3): `crates/ato-cli/src/application/credential/redaction.rs`
  - `RedactionRegistry::register(value)` → log writer / receipt builder の write path にフックされる Filter trait
- ゼロクリア (Rule M4-b): `zeroize` crate を `Cargo.toml` に追加 (努力義務、v1 で `String::zeroize_on_drop` 等を使える範囲で適用)

**Acceptance**:
- temp file channel の unit test: 値書き込み → 600 perm 確認 → child exit 後 unlink
- argv に credential が出ない property test (provision の AST に literal substitution されないこと)
- redaction filter の round-trip: log writer に "password=secret123" を流すと "password=***" で出る
- implementation docs / comments では credential 値の処理を `expand` ではなく `materialize` / `channel marker rewrite` と呼び、plaintext substitution と誤読される語彙を避ける

### P5: Runtime Orchestration

**目的**: 既存 `managed_services` 系を `dependencies` graph 駆動に置換。外部仕様には `managed_service` 語彙を出さない。

**変更**:
- `crates/ato-cli/src/app_control.rs`
  - `materialize_managed_services` → `materialize_dependency_graph` に rename (内部実装名はそのまま生き残ってもよいが、外部 API 名は dep grammar に揃える)
  - `orchestrate_managed_services` → `orchestrate_dependency_graph`
  - `start_service` → `start_dep_target` (provider target を contract.target binding 経由で start)
  - `port = "auto"` の TCP allocation を `EndpointAllocator::allocate_tcp(addr) -> u16`
  - ready probe runtime: `tcp` と `probe` を実装、`http` / `unix_socket` は P3 で lock fail なので runtime 到達不可
  - teardown: reverse-topological order
  - orphan detection: `<state.dir>/.ato-session` を読み、4-state (RFC §10.4) 表に従う
- 新規 `crates/ato-cli/src/application/dependency_runtime.rs`
  - 起動シーケンス (RFC §10.2): materialize → state.dir → credential resolve → orphan check → provision → target start → ready → runtime_exports resolve
- `crates/ato-cli/src/application/managed_service_receipt.rs`
  - ライフサイクル統合 (既存 receipt builder の入力に dependency runtime 出力を渡す)

**Acceptance**:
- E2E: in-tree mock provider で `port = "auto"` allocation 成功
- E2E: orphan 検出の 4 ケース (sentinel 不在 / dead pid / 同 session / 別 alive session) を session lifecycle test で確認
- 既存 managed-service test が新名で通る
- CI mock-service E2E: real Postgres を使わず、dependency graph orchestration / ready probe / runtime_exports injection / redaction / teardown を検証する

### P6: Identity 畳み込み

**目的**: v2 receipt の `dependency_derivation_hash` に直接依存先を畳み込む。

**変更**:
- `crates/capsule-core/src/execution_identity.rs`
  - `dependency_derivation_hash` 計算時に `(resolved, contract, parameters, identity_exports)` JCS canonical を blake3-256
  - credentials は **絶対に入力に入れない** (テストで invariant)
- `crates/ato-cli/src/application/execution_receipt_builder.rs`
  - dependency lock を receipt builder の入力に追加
  - `dependency_derivation_hash` を新 hash で計算

**Acceptance**:
- Identity invariant test (RFC §14 step 14): `PG_PASSWORD` rotate → lock / instance_hash / state.dir / `dependency_derivation_hash` 全て不変
- direct dep の identity 畳み込み: parameter 変更 → hash 変化、credential 変更 → hash 不変
- transitive dep は v1 では入れない (regression test として `nested provider` の hash 入力に transitive が出ない)

### P7: E2E with Mock Provider + `ato/postgres` Provider

**目的**: RFC §11 の worked example を実コードで動かす。

**変更**:
- 新規 `crates/ato-cli/tests/dependency_contracts/`
  - `mock_service_provider.toml` (CI 用。real DB なしで `port = "auto"`, `ready`, `runtime_exports`, redaction, teardown を検証)
  - `mock_postgres_provider.toml` (manual E2E 用。実 Postgres バイナリは host 依存)
  - `wasedap2p_consumer.toml` (RFC §11.1 を踏襲)
- 新規 (registry または開発 fixture) `ato/postgres@16` provider capsule
  - 最小実装: `provision` で `initdb` を呼び、`{{credentials.password}}` を Rule M1 temp file channel で受け取る
- WasedaP2P fork に PR: `capsule.toml` を新 grammar に移行

**Acceptance**:
- CI: mock-service provider で `ato run` → dependency 起動 → consumer ready → teardown 成功
- CI: `PG_PASSWORD` 相当の credential を rotate して再 `ato run` → lock / instance_hash / state.dir / `dependency_derivation_hash` が **変化しない** ことを assert
- Manual/host-bound: `ato run` で WasedaP2P が Postgres dep 経由で起動 → uvicorn が ready → `curl http://127.0.0.1:8000/health` 成功
- Manual/host-bound: 実 Postgres の password rotation 後に新 password で接続成功することは **v1 generic contract の DoD ではない**。provider fixture が明示的に rotation hook 相当を持つ場合のみ追加検証する

## 3. リスク・未確定事項

| Risk | Mitigation |
| --- | --- |
| `port = "auto"` allocation race (RFC §13 Open Question) | P5 で OS-assigned port (bind 0) パターンを採用、`<state.dir>/.port` を provider が書く方式と比較して prototype |
| credential template の resolution timing がコード経路で曖昧化 | P4 の `CredentialResolver` を **single entry point** にし、orchestration の `start_dep_target` から 1 箇所でしか呼ばない |
| template grammar が P1 実装後に変わる | P0 で v1 subset / reserved token / undefined-key handling を閉じてから parser 実装に入る |
| 既存 `managed_service` 内部名が leak する | P5 で grep + lint 規則 (`grep -r "managed_service" docs/ user-facing-strings/` が 0 件) を CI に追加 |
| transitive dep の hash 入力に間違って含めてしまう regression | P6 で property test: provider が更に dep を持つ fixture を読ませて、parent receipt hash の入力 JCS に transitive が **絶対に出ない** ことを assert |
| WasedaP2P provider 側で Postgres バイナリ取得方法が未決定 | P7 の CI は mock-service provider で完結させる。実 Postgres は homebrew/apt 前提の manual host-bound E2E として扱う。将来は `capsule://ato/postgres-bin@16` で binary capsule を作る (本 RFC 範囲外) |
| 実 Postgres の credential rotation を generic contract 保証と誤解する | v1 の CI は identity invariant のみを検証する。新 password 接続成功は credential rotation RFC または provider-specific fixture の範囲に限定する |

## 4. テスト戦略

CI gate と manual gate を分け、実 Postgres なしでも core invariant が落ちる構成にする:

**Layer A — Property tests (capsule-core)**: AST round-trip、invariant 不変性。値域全域での regression 防止。

**Layer B — Unit tests (ato-cli)**: 各 verification rule の failure path、credential channel の sandbox 動作、redaction filter。

**Layer C1 — CI E2E tests (`crates/ato-cli/tests/`)**: real Postgres なしで動く mock-service integration scenario:
- `port = "auto"` allocation 連続 5 回で衝突しない
- credential rotation 不変性 (重要 invariant)
- orphan detection の 4 ケース
- reserved variant が lock failure になる
- runtime_exports injection / redaction / teardown

**Layer C2 — Manual host-bound E2E**: 実 binary を起動した integration scenario:
- WasedaP2P + ato/postgres E2E
- Postgres バイナリが host に存在する環境だけで実行
- password rotation の接続成功は provider-specific 追加検証であり、v1 generic contract の必須 CI gate にはしない

CI gate: A + B + C1 が緑。C2 は手動 trigger。

## 5. PR 順序とマージ計画

| 順 | PR title | 依存 | 規模 |
| --- | --- | --- | --- |
| 0 | `docs: close v1 template grammar for dependency contracts` | — | 小 (docs only) |
| 1 | `feat(capsule-core): add [dependencies.*] / [contracts.*] AST` | 0 | 中 (parser + AST + tests) |
| 2 | `feat(capsule-core): add EnvOrigin and runtime_exports identity exclusion` | 1 | 小 |
| 3 | `feat(capsule-core): add pure lock verification for dependency contracts` | 1, 2 | 中 |
| 4 | `feat(ato-cli): credential resolver and materialization channels` | 1, 2 | 中 |
| 5a | `refactor(ato-cli): rename managed_service internals to dependency_graph` | — (独立) | 小 (rename only) |
| 5b | `feat(ato-cli): runtime orchestration for service@1 contracts` | 3, 4, 5a | 大 |
| 6 | `feat(ato-cli): include direct deps in dependency_derivation_hash` | 5b | 中 |
| 7 | `test(ato-cli): mock dependency-contract E2E and manual ato/postgres path` | 6 | 中 |

5a を独立先行で出すと、5b の diff が読みやすくなる。

## 6. 完了判定

以下が全て満たされたら本 plan 完了:

1. RFC §3 の v1 scope 11 項目が全て実装済 (PR 0–7)
2. RFC §3 末尾の invariant 9 項目が CI で守られる (Property/Unit test 緑)
3. mock-service dependency E2E が CI で緑
4. WasedaP2P が新 grammar で `ato run` から立ち上がる (manual host-bound E2E 成功)
5. Credential rotation identity invariant test が緑 (RFC §14 step 14)。新 password で実 DB に接続できることは v1 generic DoD に含めない
6. 外部公開 API / docs / error message に `managed_service` 語彙が残っていない (lint check)
7. `docs/rfcs/draft/` 配下の関連 RFC (transitive identity, credential rotation, defaults 等) が 0 件先行実装されていない (scope creep 防止)

## 7. 推定タイムライン (参考、実工数次第)

- P0: 0.5 日
- P1 + P2: 並行で 1 週
- P3: 1 週
- P4 + P5a: 並行で 1 週
- P5b: 1.5 週
- P6: 0.5 週
- P7: 1 週

合計 5–6 週。週 4 営業日換算で 20–25 営業日相当。

---

**次のアクション**: P0 から着手。template grammar の v1 subset / reserved token / undefined-key handling を accepted RFC または本 plan に固定してから、P1 の `capsule-core` parser PR に進む。最初の code PR は `[dependencies.*]` / `[contracts.*]` parser のみで AST round-trip テスト含めて出す。orchestration / credential / lock 検証はその上に乗せる。
