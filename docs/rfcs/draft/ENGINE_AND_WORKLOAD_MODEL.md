---
title: "Engine & Workload Model (Draft)"
status: draft
date: "2026-04-23"
author: "@egamikohsuke"
ssot:
  - "apps/ato-cli/core/src/engine.rs"
  - "apps/ato-cli/core/src/executors/"
  - "apps/ato-cli/core/src/runtime/"
related:
  - "NACELLE_SPEC.md"
  - "RUNTIME_AND_BUILD_MODEL.md"
  - "UNIFIED_EXECUTION_MODEL.md"
  - "../../GLOSSARY.md"
---

# Engine & Workload Model (Draft)

> Status: **Draft** — `ato-cli` における "Engine" と "Workload" の概念を正式化し、nacelle 以外の実行バックエンド（OCI / WASM / 将来の候補）を同じ抽象に載せるための下地となる仕様。
>
> Scope: 用語定義 + Engine 抽象の最小契約 + 現行実装ステータス整理 + `UNIFIED_EXECUTION_MODEL` への接続点。
>
> Non-goal: narrative 再編、新フェーズ追加、nacelle の内部仕様変更（それは `NACELLE_SPEC.md` の責務）。

---

## 1. 背景

### 1.1 現状の曖昧さ

`ato-cli` 内で "engine" という語は複数の意味で使われている。

| 呼称 | 指している対象 | 実体 |
|------|---------------|------|
| "Engine" (環境変数 `NACELLE_PATH`, CLI `--nacelle`) | nacelle バイナリ | `apps/nacelle/` |
| "Engine" (`core/src/engine.rs::run_internal`) | `<engine> internal <subcmd>` を呼ぶ外部プロセス契約 | 抽象インターフェース |
| `RuntimeKind::{Source, Oci, Wasm, Web}` | 実行形態（= 実行バックエンドの選択軸） | `core/src/execution_plan/model.rs` |
| `executors/{oci,wasm,source}.rs` | `ato run` から呼ばれる dispatcher | 実装モジュール |

これらは **4 つの異なるレイヤ**を指しているが、コード・ドキュメント双方で区別されずに「エンジン」と呼ばれ、以下の混乱を生んでいる:

1. nacelle 以外のバックエンド（OCI, WASM）が「engine」なのか「executor」なのか不明瞭
2. `RuntimeKind::Source` が「エンジンは nacelle」を暗黙に前提しており、代替 Source エンジンを差し込む余地が型レベルで見えない
3. GLOSSARY の "Engine" 定義は `engine internal` 契約に寄せているが、OCI/WASM はその契約を満たしていない（Bollard / wasmtime CLI への直接ディスパッチ）

### 1.2 関連する既存 RFC との差分

- [`NACELLE_SPEC.md`](../accepted/NACELLE_SPEC.md) — nacelle **単体**の仕様。`engine internal exec` 契約は nacelle 固有として定義されている。
- [`RUNTIME_AND_BUILD_MODEL.md`](../accepted/RUNTIME_AND_BUILD_MODEL.md) — Smart Build / Dumb Runtime の責務分離。「実行時 = nacelle」を前提にしている。
- [`UNIFIED_EXECUTION_MODEL.md`](./UNIFIED_EXECUTION_MODEL.md) — Pipeline × Runtime 統一。Runtime 側は `RuntimeProvisioner` trait（**Provider Toolchain 層**の抽象）で、Engine 層の抽象には踏み込んでいない。

→ **本 RFC は "Engine 層の抽象" を埋める**。Provider Toolchain でも Pipeline でもない、`ato-cli` ↔ サンドボックス実行基盤の境界を定義する。

---

## 2. 定義

### 2.1 4 層モデル

```
┌─────────────────────────────────────────────────────────────┐
│ ato-cli                    — オーケストレータ（プロセス管理外）│
└──────────┬──────────────────────────────────────────────────┘
           │ 実行を委譲
┌──────────▼──────────────────────────────────────────────────┐
│ Engine                     — サンドボックス + プロセス管理     │
│   例: nacelle / OCI runtime / wasmtime                       │
└──────────┬──────────────────────────────────────────────────┘
           │ spawn
┌──────────▼──────────────────────────────────────────────────┐
│ Provider Toolchain         — 言語ランタイム（任意）           │
│   例: uv / node / pnpm / deno                               │
└──────────┬──────────────────────────────────────────────────┘
           │ interpret / exec
┌──────────▼──────────────────────────────────────────────────┐
│ Workload                   — ユーザーコード本体              │
│   例: app.py / server.js / a.out / main.wasm                 │
└─────────────────────────────────────────────────────────────┘
```

各層の責務境界:

| 層 | 入力 | 出力 | 置換可能性 |
|----|------|------|-----------|
| ato-cli | user コマンド / `capsule.toml` | `RunPlan`（確定済み実行定義） | 不可（単一バイナリ） |
| **Engine** | `RunPlan` + Sandbox Policy | Workload プロセス（隔離済み）+ 観測イベント | **本 RFC のスコープ** |
| Provider Toolchain | Workload ソース + 解決済み依存 | 解釈実行 | ランタイム選択で差し替え |
| Workload | user input | side-effects / logs / IPC | user が所有 |

### 2.2 Engine の最小契約

Engine は以下を満たす実行基盤:

1. **入力**: `ato-cli` が計算した `RunPlan`（エントリポイント・env・マウント・ポート・サンドボックスポリシー）
2. **出力**:
   - 起動成功時: Workload プロセスの識別子（pid / container id / instance id）
   - 実行中: 構造化イベント（`ipc_ready`, `service_exited` 等）
   - 終了時: exit code
3. **保証**:
   - Workload は Engine のサンドボックス境界を越えられない（fail-closed）
   - Engine が提供する観測点（stdout/stderr/exit/metrics）以外から Workload 状態を取得しない
   - Engine 自身は `RunPlan` を **解釈せず愚直に実行**する（Smart Build, Dumb Runtime 原則）

### 2.3 Workload の定義

Capsule が実行する **ユーザーコード本体**。以下の性質を持つ:

- **発生源**: `capsule.toml` の `entrypoint`（または `runtime = "oci"` の場合は image 内コマンド）
- **観測対象**: Workload の stdout / stderr / exit code / IPC メッセージが Capsule の挙動そのもの
- **非スコープ**: Provider Toolchain のセットアップ、依存解決、lockfile 生成は Workload の責務ではない
- **ライフサイクル**: Engine が起動し、Engine または `ato-cli` のシグナル送信で停止する

Workload は Engine と **直接のプロトコル契約を持たない**。Provider Toolchain や OCI image の規約に従って動くだけ。

---

## 3. Engine Interface 契約（提案）

### 3.1 現状の nacelle 契約（`engine internal` プロトコル）

nacelle のみが実装している JSON-over-stdio 契約（[NACELLE_SPEC.md §3](../accepted/NACELLE_SPEC.md) 参照）:

```
ato-cli ──► exec <engine> internal features ──► JSON response（ケイパビリティ）
ato-cli ──► exec <engine> internal exec     ──► NDJSON stream（initial + events）
```

この契約を **Engine Interface v1** として昇格させ、以下を `ato-cli` 側で型として定義する:

```rust
// core/src/engine/interface.rs（新規・提案）
pub trait EngineInterface {
    fn features(&self) -> Result<EngineFeatures>;
    fn exec(&self, plan: &RunPlan) -> Result<EngineExecHandle>;
}

pub struct EngineFeatures {
    pub spec_version: String,      // "1.0" | "2.0" | ...
    pub sandboxes: Vec<String>,    // ["landlock", "seatbelt", ...]
    pub capabilities: Vec<String>, // ["socket_activation", "gpu", ...]
}
```

### 3.2 OCI / WASM バックエンドの位置づけ

現状、`executors/oci.rs` / `executors/wasm.rs` は `engine internal` 契約**を介さず**、`ato-cli` プロセス内で Bollard（Docker SDK）/ `wasmtime` CLI を直接叩いている。

これは以下の理由で **Engine ではなく Executor** と分類する:

| 観点 | nacelle (Engine) | OCI / WASM (Executor) |
|------|------------------|----------------------|
| プロセス境界 | 外部バイナリ（`engine internal` 経由） | `ato-cli` プロセス内で直接呼び出し |
| サンドボックス実装者 | nacelle が Landlock/Seatbelt を適用 | Docker/Podman daemon / wasmtime 組み込み |
| ケイパビリティ発見 | `engine features` で動的 | コンパイル時に固定 |
| 置換可能性 | nacelle バイナリを差し替え可能 | `ato-cli` ビルドに組み込み |

→ **用語の明確化**: "Engine" は `engine internal` 契約を満たす外部実行基盤のみ。現状は nacelle が唯一。OCI / WASM は "Runtime Executor"（`RuntimeKind` に紐づく内部実行モジュール）と呼ぶ。

### 3.3 将来の Engine 候補

| 候補 | 動機 | 契約適合性 |
|------|------|-----------|
| nacelle (current) | Linux/macOS ネイティブサンドボックス | ✅ 既に v1 契約を実装 |
| `firecracker-engine` (hypothetical) | micro-VM 隔離 | 要 v1 契約実装（wrapper バイナリ） |
| `gvisor-engine` (hypothetical) | user-space kernel | 同上 |
| OCI runtime を Engine 化 | Bollard 依存を剥がす | 要: `ato-oci-engine` wrapper が `engine internal` を喋る |

**OCI を Engine 化する価値**: `ato-cli` から Docker SDK 依存を取り除ける。現状、ato-cli バイナリに Bollard がリンクされており、OCI を使わないユーザーにもコスト発生。

---

## 4. RuntimeKind と Engine の対応関係（現状）

```
RuntimeKind          Engine/Executor            Provider Toolchain   Workload
──────────────       ──────────────────         ──────────────────   ──────────
Source (Python)  →   nacelle (Engine)       →   uv              →   app.py
Source (Node)    →   nacelle (Engine)       →   node / pnpm     →   server.js
Source (Deno)    →   nacelle (Engine)       →   deno            →   mod.ts
Oci              →   OCI Executor (in-proc) →   (image内)        →   image CMD
Wasm             →   WASM Executor (in-proc)→   wasmtime CLI    →   main.wasm
Web              →   Static Executor        →   (なし)           →   index.html
```

**観察**:

- nacelle は `Source` 系の 4 言語すべてで共通の Engine
- `Oci` / `Wasm` / `Web` は Engine 抽象の外で動いており、サンドボックス責務が実行基盤ごとに分散している
- v0.5 以降、これらを `EngineInterface` 経由に揃えるか、明示的に "Executor" カテゴリとして分離するかが論点

---

## 5. 現行実装ステータス

### 5.1 nacelle（Engine）

- **ステータス**: 🟢 production
- **契約**: `engine internal features` / `engine internal exec` v1.0 実装済み
- **サンドボックス**: Landlock (Linux) / Seatbelt (macOS) / eBPF（オプション）
- **SSOT**: `apps/nacelle/`
- **参照**: [`NACELLE_SPEC.md`](../accepted/NACELLE_SPEC.md)

### 5.2 OCI Executor

- **ステータス**: 🟢 動作するが Engine 契約外
- **実装**: `core/src/runtime/oci.rs` (636 行) + `core/src/executors/oci.rs`
- **バックエンド**: Bollard (Docker SDK)、Docker/Podman を `which` で自動判別
- **機能**: pull → create → start → logs → wait → stop → remove の全ライフサイクル、network/port/mount/env/GPU
- **CLI 到達性**: `ato run` の `runtime = "oci"` で `executors::oci::execute()` にディスパッチ

### 5.3 WASM Executor

- **ステータス**: 🟡 最小限（PoC レベル）
- **実装**: `core/src/runtime/wasm.rs` に「**暫定スタブ**」コメント（line 6）
- **バックエンド**: `wasmtime` CLI を外部プロセス起動（ライブラリ統合なし）
- **制約**: メトリクス収集なし、ライフサイクル管理なし、env はシェル渡し、wasmtime 外部インストール必須
- **CLI 到達性**: 一応可（`runtime = "wasm"`）

### 5.4 Web Executor

- **ステータス**: 🟢 用途限定（ato-desktop 向け静的配信）
- **実装**: `core/src/executors/` 配下
- **備考**: サンドボックス責務は薄い（静的ファイル配信のみ）

---

## 6. 提案（v0.5.x 目標）

### 6.1 用語整理（ドキュメント）

- `Engine` = `engine internal` 契約を満たす外部実行基盤（現状 nacelle のみ）
- `Runtime Executor` = `RuntimeKind` ごとの内部ディスパッチモジュール（OCI/WASM/Web）
- `Provider Toolchain` = 言語ランタイム（uv/node/deno 等）
- `Workload` = ユーザーコード本体

GLOSSARY と `NACELLE_SPEC` の "Engine" 定義を本 RFC の定義に揃える。

### 6.2 Engine Interface v1 の型化

- `core/src/engine/interface.rs` を新設し、`EngineInterface` trait を切り出す
- nacelle 以外のエンジンを追加する際の適合条件を明示
- `engine internal` の JSON スキーマを OpenAPI 相当で固定（現状は `NACELLE_SPEC` に散在）

### 6.3 OCI / WASM の Engine 化は保留

- OCI を Engine 化するメリット（Bollard 剥がし）は認めるが、`ato-oci-engine` wrapper バイナリのメンテコストと釣り合うかは未検証
- 少なくとも v0.5 では **Executor カテゴリのまま**にし、Engine とは別レイヤとして扱う
- 再評価タイミング: nacelle 以外の Engine 候補（firecracker 等）が具体化したとき

### 6.4 `UNIFIED_EXECUTION_MODEL` との接続

`UNIFIED_EXECUTION_MODEL` の `RuntimeProvisioner` trait は **Provider Toolchain 層** の抽象であり、本 RFC の `EngineInterface` とは**直交**する:

```
HourglassPipeline
  └─ Install phase  → RuntimeProvisioner::ensure  (Provider Toolchain 層)
  └─ Execute phase  → EngineInterface::exec       (Engine 層)  ← 本 RFC
                        └─ ManagedRuntimePath    (PATH 合成)
                             └─ Workload spawn
```

両 RFC は独立に accepted 化できるが、`EngineInterface::exec` のシグネチャは `UNIFIED_EXECUTION_MODEL §2.1` の Pipeline 接続点と整合させる必要がある。

---

## 7. 非ゴール

- nacelle 内部仕様の変更（`NACELLE_SPEC` の責務）
- `RuntimeKind` enum のバリアント追加・削除
- OCI / WASM のサンドボックス強化（別 RFC）
- Plugin system / 動的ロード（本 RFC は静的に enum で管理する前提）
- narrative (Try / Keep / Share) への影響

---

## 8. 未解決論点

1. **Engine バイナリの配布形態**: nacelle は `~/.ato/engines/` に JIT ダウンロードされるが、代替 Engine をどう配布するか（registry? capsule 経由?）
2. **Engine ケイパビリティの capability-gate との関係**: `engine features` が返す能力と、Capsule の `[capabilities]` マニフェストをどう突き合わせるか
3. **Executor → Engine 昇格の判定基準**: OCI/WASM を Engine 化する条件を ADR として切り出すか
4. **Engine の signing / trust model**: 現状 nacelle のみを `trust_store` で検証しているが、複数 Engine 時代の trust anchor 設計
5. **Windows サポート**: nacelle は Unix 前提。Windows 向け Engine（WSL2 内 nacelle? 別実装?）の扱い

---

## 9. 実装着手順序（tentative）

1. **Phase 1**: 用語整理（本 RFC + GLOSSARY 更新） — 本 PR スコープ
2. **Phase 2**: `EngineInterface` trait 切り出し（現 `engine.rs` のリファクタ）
3. **Phase 3**: Engine features の JSON スキーマ固定 + schema registry 登録
4. **Phase 4**: Executor/Engine 境界のドキュメント化（`ARCHITECTURE_OVERVIEW.md` 更新）
5. **Phase 5**: accepted 化判断（nacelle 以外の Engine 候補が具体化したタイミング）

---

## 10. 参考

- [NACELLE_SPEC.md](../accepted/NACELLE_SPEC.md) — nacelle 単体仕様
- [RUNTIME_AND_BUILD_MODEL.md](../accepted/RUNTIME_AND_BUILD_MODEL.md) — Smart Build / Dumb Runtime
- [UNIFIED_EXECUTION_MODEL.md](./UNIFIED_EXECUTION_MODEL.md) — Pipeline × Runtime 統一（Provider Toolchain 層）
- [GLOSSARY.md](../../GLOSSARY.md) — Engine / Workload / Provider Toolchain 用語定義
- SSOT: `apps/ato-cli/core/src/engine.rs`, `apps/ato-cli/core/src/executors/`, `apps/ato-cli/core/src/runtime/`
