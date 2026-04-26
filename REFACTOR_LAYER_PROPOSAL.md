# ato-cli/core レイヤー構造リファクタ提案

**作成日**: 2026-04-24  
**根拠**: research_orchestrated_layer_structure_20260424/REPORT.md  
**スタイル**: REFACTOR_TODO.md に準拠（完了済み `[x]`、未着手 `[ ]`）

---

## 背景と設計原則

調査の結果、以下が確認された:

1. **"capability" は security token の語** — network/fs の実装置き場ではない。`schema/capabilities.rs` の現在の命名は正しい
2. **"sandbox" はモジュール名として曖昧** — `runtime/` が Rust エコシステムの標準
3. **network/fs は3層にまたがる** — `policy/`（ルール評価）+ `resource/`（I/O 実装）+ `runtime/`（実行時制約）に自然分散する
4. **`tsnet/` は Adapter 層** — Layer 8（設定・周辺）ではなく Layer 7（リソース管理）
5. **`lib.rs` の44モジュール宣言は層構造が不可視** — 読み手がどの層のモジュールかを判断できない

---

## 優先度と段階

- **P0（構造的正確性）**: 論理的な誤配置の修正（tsnet の位置づけ等）
- **P1（可読性）**: lib.rs への層コメント付与、欠落モジュールの明示
- **P2（整理）**: lockfile 系のサブモジュール統合
- **非ゴール**: ディレクトリ大移動、`pub mod` パスの公開 API 変更

---

## Phase 0: 設計ドキュメント（ゼロリスク）

### Step 1 — 確定レイヤー定義の文書化

以下の8層を正式な設計として確定する。現在の提案から**修正が必要な箇所**を含む:

```
Layer 1: 型・共通ユーティリティ
  error  types  common  hardware  metrics  reporter

Layer 2: マニフェスト & ロックファイル（契約層）
  manifest  lockfile  lock_runtime  lockfile_runtime  lockfile_support  ato_lock
  ※ lockfile_runtime.rs / lockfile_support.rs が元提案に欠落

Layer 3: ルーティング & 解決
  router  input_resolver  handle  handle_store  launch_spec  importer  discovery

Layer 4: 実行エンジン
  engine  executors  orchestration  execution_plan  runner  runtime  lifecycle  share
  ※ share/ が元提案に欠落（share/executor.rs はsandbox実行隣接）

Layer 5: パッキング
  packers/{capsule,bundle,lockfile,source,web,wasm,oci}
  packers/{payload,pack_filter,runtime_fetcher,sbom}

Layer 6: セキュリティ & ポリシー（Domain層）
  security  signing  isolation  policy  validation  trust_store
  ※ validation/ と trust_store.rs が元提案に欠落

Layer 7: リソース管理（Adapter層）
  resource/{cas,artifact,ingest,storage}  capsule  tsnet
  ※ tsnet/ が元提案の Layer 8 から移動すべき

Layer 8: 設定 & 周辺
  config  runtime_config  python_runtime  schema  schema_registry
  bootstrap  discovery  diagnostics  smoke
  ※ schema/ と smoke.rs が元提案に欠落
```

- [ ] `docs/architecture/LAYERS.md` を新設し、上記8層を定義・記載

---

## Phase 1: lib.rs の可読性改善（ゼロリスク）

### Step 2 — lib.rs にレイヤーセクションコメントを追加

現在の `lib.rs` は44モジュールがアルファベット順にフラット列挙されており、どの層に属するかが不明。
`pub mod` 宣言をレイヤーごとに整理し、セクションコメントで区切る。

**変更前（現状・抜粋）:**
```rust
pub mod ato_lock;
pub mod bootstrap;
pub mod capsule;
pub mod common;
// ... 44行フラット
```

**変更後（提案）:**
```rust
// ── Layer 1: 型・共通ユーティリティ ──────────────────────────────────────────
pub mod common;
pub mod error;
pub mod hardware;
pub mod metrics;
pub mod reporter;
pub mod types;

// ── Layer 2: マニフェスト & ロックファイル（契約層）──────────────────────────
pub mod ato_lock;
pub mod lock_runtime;
pub mod lockfile;
pub mod lockfile_runtime;   // ← 欠落していた明示
pub mod lockfile_support;   // ← 欠落していた明示
pub mod manifest;

// ── Layer 3: ルーティング & 解決 ─────────────────────────────────────────────
pub mod discovery;
pub mod handle;
pub mod handle_store;
pub mod importer;
pub mod input_resolver;
pub mod launch_spec;
pub mod router;

// ── Layer 4: 実行エンジン ────────────────────────────────────────────────────
pub mod engine;
pub mod execution_plan;
pub mod executors;
pub mod lifecycle;
pub mod orchestration;
pub mod runner;
pub mod runtime;
pub mod share;              // ← 欠落していた明示（sandbox実行隣接）

// ── Layer 5: パッキング ───────────────────────────────────────────────────────
pub mod packers;

// ── Layer 6: セキュリティ & ポリシー（Domain層）──────────────────────────────
pub mod isolation;
pub mod policy;
pub mod schema;             // ← capabilities schema
pub mod security;
pub mod signing;
pub mod trust_store;        // ← 欠落していた明示
pub mod validation;         // ← 欠落していた明示

// ── Layer 7: リソース管理（Adapter層）────────────────────────────────────────
pub mod capsule;
pub mod resource;
pub mod tsnet;              // ← Layer 8から移動（Tailscaleはadapter）

// ── Layer 8: 設定 & 周辺 ────────────────────────────────────────────────────
pub mod bootstrap;
pub mod config;
pub mod diagnostics;
pub mod python_runtime;
pub mod runtime_config;
pub mod schema_registry;
pub mod smoke;              // ← 欠落していた明示
```

- [ ] `lib.rs` の `pub mod` 宣言をレイヤーセクションで整理（機能変更なし、アルファベット順 → 層順）
- [ ] lib.rs 冒頭の `//!` doc コメントの `## Responsibilities` セクションを8層に合わせて更新

---

## Phase 2: 論理的誤配置の修正（低リスク）

### Step 3 — `lockfile_runtime.rs` / `lockfile_support.rs` の Layer 2 への明示的統合

**現状の問題:**
```
src/
├── lockfile.rs           ← Layer 2（明示済み）
├── lock_runtime.rs       ← Layer 2（明示済み）
├── lockfile_runtime.rs   ← Layer 2 だが名前の一貫性が低い
├── lockfile_support.rs   ← Layer 2 だが名前の一貫性が低い
└── lockfile_tests.rs     ← テストファイルが src/ 直下にある
```

**修正案A（低リスク）:** `lib.rs` のコメントで Layer 2 として明示するのみ（Phase 1で対応済み）

**修正案B（中リスク）:** `lockfile/` サブモジュールとして統合
```
src/lockfile/
├── mod.rs          ← 現 lockfile.rs の内容
├── runtime.rs      ← 現 lockfile_runtime.rs の内容
├── support.rs      ← 現 lockfile_support.rs の内容
└── tests.rs        ← 現 lockfile_tests.rs の内容
```

- [ ] 修正案A/Bのどちらにするかを決定（`pub mod lockfile_runtime` が外部から参照されているか確認）
- [ ] 案B採用の場合: `grep -rn "use capsule_core::lockfile_runtime\|use capsule_core::lockfile_support"` で外部利用を確認

### Step 4 — `schema/` の Layer 6 への帰属明示

現在 `schema/capabilities.rs` は Capsule が宣言する許可セット（Network, FsWrites 等）を定義しており、セキュリティ Domain の一部。`schema_registry.rs`（Layer 8: 設定）とは別物。

- [ ] `lib.rs` で `pub mod schema` を Layer 6 セクションに配置（Step 2 で対応済み）
- [ ] `schema/mod.rs` の doc コメントに「Layer 6: capability declarations（セキュリティドメイン）」旨を追記

### Step 5 — `validation/` と `trust_store.rs` の Layer 6 への帰属明示

```
validation/source_policy.rs  ← build/pack時のソース検証（policy 隣接）
trust_store.rs               ← 署名・信頼チェーン管理（signing 隣接）
```

- [ ] `lib.rs` で両者を Layer 6 セクションに配置（Step 2 で対応済み）
- [ ] `validation/mod.rs` の doc コメントを更新: "Layer 6: Source validation policy"
- [ ] `trust_store.rs` の doc コメントを更新: "Layer 6: Trust chain management"

---

## Phase 3: tsnet の Adapter 層への再配置（低〜中リスク）

### Step 6 — `tsnet/` を Layer 7（Adapter層）に配置変更

**現在の誤認識:** tsnet は「設定・周辺」ではなく Tailscale ネットワーク sidecar の**adapter 実装**。
Clean Architecture では外部サービス統合は Adapter 層（Layer 7）に属する。

**現状の lib.rs の pub use:**
```rust
pub use tsnet::{
    discover_sidecar, spawn_sidecar, wait_for_ready,
    SidecarBaseConfig, SidecarRequest, SidecarSpawnConfig,
    TsnetClient, TsnetConfig, TsnetEndpoint, TsnetHandle,
    TsnetServeStatus, TsnetState, TsnetStatus, TsnetWaitConfig,
};
```

tsnet は `resource/` と並ぶ adapter として、将来的には `resource/network/` または独立した `network/` モジュールとしての整理も検討に値する。

- [ ] `lib.rs` で `pub mod tsnet` を Layer 7 セクションに移動（Step 2 で対応済み）
- [ ] `tsnet/mod.rs` の doc コメントを更新: "Layer 7: Tailscale network adapter（外部ネットワークサービスとのAdapter）"

---

## Phase 4: Port Traits の形式化（中リスク、将来の拡張）

### Step 7 — Adapter 境界の Port traits を `types/` または `common/` に定義

調査によると Hexagonal Architecture の Rust 実装では `trait` が Port（インターフェース）を担い、`struct impl` が Adapter を担う。現在は暗黙的なインターフェースが多い。

**提案する trait（優先度順）:**

```rust
// types/ または common/ に配置（Layer 1）
pub trait ArtifactStore: Send + Sync {
    async fn fetch(&self, digest: &str) -> Result<Vec<u8>>;
    async fn store(&self, data: &[u8]) -> Result<String>;
}

pub trait NetworkTransport: Send + Sync {
    // tsnet と resource/ingest の共通インターフェース
}
```

- [ ] 現在の `resource/artifact/` が暗黙的に実装しているインターフェースを trait として抽出する価値があるか調査
- [ ] `resource/storage/` の Port trait を `types/` に定義するか検討（実装前に ROI 評価）

---

## 非ゴール

- ディレクトリの物理的な大移動（`pub mod` パスが公開 API に影響するため）
- `lockfile_runtime.rs` / `lockfile_support.rs` を強制的に `lockfile/` に統合（外部参照確認前）
- `tsnet/` を `resource/network/` に移動（現時点では `pub use tsnet::*` が多数あるため）
- network/fs を `capability/` にまとめる（調査により意味的に誤りと確認）
- `sandbox/` というモジュール名の導入（Rust エコシステムで非標準）

---

## 実施順序と推定コスト

| Phase | 内容 | リスク | 工数目安 |
|---|---|---|---|
| 0 | LAYERS.md 作成 | ゼロ | 30分 |
| 1 / Step 2 | lib.rs レイヤーセクション化 | ゼロ | 30分 |
| 2 / Step 3-5 | doc コメント更新 3箇所 | ゼロ | 30分 |
| 3 / Step 6 | tsnet doc コメント更新 | ゼロ | 10分 |
| 2 / Step 3B | lockfile サブモジュール統合 | 中（要確認） | 2時間 |
| 4 / Step 7 | Port traits 形式化 | 中〜高 | 別タスク |

**Phase 0〜3（doc/コメントのみ）は合計約100分、コンパイルエラーゼロで完遂可能。**

---

## レイヤー導入後のファイルツリー

`src/` の物理構造は変えない（非ゴール）。ただし `lib.rs` のモジュール宣言順と、
将来の Phase 2B（lockfile サブモジュール統合）を適用した場合の **論理的ファイルツリー**。

凡例: `●` = 現状から変化なし、`→` = 帰属層が変わる（ファイル移動なし）、`▲` = Phase 2B で統合（ファイル移動あり）

```
core/src/
│
│  ── Layer 1: 型・共通ユーティリティ ──────────────────────────────────────────
│
├── error.rs                         ● CapsuleError / AtoError / AtoErrorPhase
├── types/
│   ├── mod.rs                       ●
│   ├── bridge.rs                    ●
│   ├── error.rs                     ●
│   ├── identity.rs                  ●
│   ├── license.rs                   ●
│   ├── manifest.rs                  ●
│   ├── manifest_tests.rs            ●
│   ├── manifest_v03.rs              ●
│   ├── manifest_validation.rs       ●
│   ├── orchestration.rs             ●
│   ├── profile.rs                   ●
│   ├── runplan.rs                   ●
│   ├── signing.rs                   ●
│   └── utils.rs                     ●
├── common/
│   ├── mod.rs                       ●
│   ├── hash.rs                      ●
│   ├── paths.rs                     ●
│   └── platform.rs                  ●
├── hardware.rs                      ●
├── metrics.rs                       ●
└── reporter.rs                      ●

│  ── Layer 2: マニフェスト & ロックファイル（契約層）──────────────────────────
│
├── manifest.rs                      ●
├── ato_lock/
│   ├── mod.rs                       ●
│   ├── canonicalize.rs              ●
│   ├── closure.rs                   ●
│   ├── hash.rs                      ●
│   ├── schema.rs                    ●
│   └── validate.rs                  ●
│
│   [現状: フラット4ファイル]
├── lockfile.rs                      ●  (Phase 2B前)
├── lock_runtime.rs                  ●  (Phase 2B前)
├── lockfile_runtime.rs              →  Layer 2 帰属を lib.rs で明示 (Phase 2B前)
├── lockfile_support.rs              →  Layer 2 帰属を lib.rs で明示 (Phase 2B前)
├── lockfile_tests.rs                →  Layer 2 帰属を lib.rs で明示 (Phase 2B前)
│
│   [Phase 2B 適用後: lockfile/ サブモジュールに統合]
└── lockfile/                        ▲  (Phase 2B)
    ├── mod.rs                       ▲  ← 旧 lockfile.rs
    ├── runtime.rs                   ▲  ← 旧 lock_runtime.rs + lockfile_runtime.rs
    ├── support.rs                   ▲  ← 旧 lockfile_support.rs
    └── tests.rs                     ▲  ← 旧 lockfile_tests.rs

│  ── Layer 3: ルーティング & 解決 ─────────────────────────────────────────────
│
├── router.rs                        ●
├── router/
│   ├── lock_routing.rs              ●
│   ├── manifest_routing.rs          ●
│   └── services.rs                  ●
├── input_resolver.rs                ●
├── handle.rs                        ●
├── handle_store.rs                  ●
├── launch_spec.rs                   ●
├── importer/
│   └── mod.rs                       ●
└── discovery.rs                     ●

│  ── Layer 4: 実行エンジン ─────────────────────────────────────────────────────
│
├── engine.rs                        ●
├── execution_plan/
│   ├── mod.rs                       ●
│   ├── canonical.rs                 ●
│   ├── derive.rs                    ●
│   ├── error.rs                     ●
│   ├── guard.rs                     ●
│   └── model.rs                     ●
├── executors/
│   ├── mod.rs                       ●
│   ├── oci.rs                       ●
│   ├── source.rs                    ●
│   └── wasm.rs                      ●
├── orchestration.rs                 ●
├── runner.rs                        ●
├── runtime/
│   ├── mod.rs                       ●
│   ├── native.rs                    ●
│   ├── oci.rs                       ●
│   └── wasm.rs                      ●
├── lifecycle.rs                     ●
└── share/                           →  Layer 4 帰属を lib.rs で明示（現 lib.rs は未セクション）
    ├── mod.rs                       ●
    ├── executor.rs                  ●  share URL のsandbox実行
    └── types.rs                     ●

│  ── Layer 5: パッキング ───────────────────────────────────────────────────────
│
└── packers/
    ├── mod.rs                       ●
    ├── bundle.rs                    ●
    ├── capsule.rs                   ●  ※ capsule/ モジュールとは別物
    ├── lockfile.rs                  ●  ※ lockfile.rs とは別物
    ├── oci.rs                       ●
    ├── pack_filter.rs               ●
    ├── payload.rs                   ●
    ├── sbom.rs                      ●
    ├── source.rs                    ●
    ├── wasm.rs                      ●
    ├── web.rs                       ●
    └── runtime_fetcher/
        ├── mod.rs                   ●
        ├── fetcher.rs               ●
        └── verifier.rs              ●

│  ── Layer 6: セキュリティ & ポリシー（Domain層）──────────────────────────────
│
├── security/
│   ├── mod.rs                       ●
│   └── path.rs                      ●
├── signing/
│   ├── mod.rs                       ●
│   ├── sign.rs                      ●
│   └── verify.rs                    ●
├── isolation.rs                     ●
├── policy/
│   ├── mod.rs                       ●
│   └── egress_resolver.rs           ●
├── schema/                          →  Layer 6 帰属を lib.rs で明示（capabilityはセキュリティDomain）
│   ├── mod.rs                       ●
│   └── capabilities.rs              ●  Capsule の許可セット宣言
├── validation/                      →  Layer 6 帰属を lib.rs で明示
│   ├── mod.rs                       ●
│   └── source_policy.rs             ●
└── trust_store.rs                   →  Layer 6 帰属を lib.rs で明示

│  ── Layer 7: リソース管理（Adapter層）────────────────────────────────────────
│
├── resource/
│   ├── mod.rs                       ●
│   ├── artifact/
│   │   ├── mod.rs                   ●
│   │   ├── cache.rs                 ●
│   │   ├── manager.rs               ●
│   │   ├── registry.rs              ●
│   │   └── tests.rs                 ●
│   ├── cas/
│   │   ├── mod.rs                   ●
│   │   ├── bloom.rs                 ●
│   │   ├── chunker.rs               ●
│   │   ├── client.rs                ●
│   │   └── index.rs                 ●
│   ├── ingest/
│   │   ├── mod.rs                   ●
│   │   ├── fetcher.rs               ●
│   │   └── http.rs                  ●
│   └── storage/
│       ├── mod.rs                   ●
│       ├── error.rs                 ●
│       └── manager.rs               ●
├── capsule/                         ●  ※ packers/capsule.rs とは別物（CAS store実装）
│   ├── mod.rs                       ●
│   ├── cas_store.rs                 ●
│   ├── fastcdc_writer.rs            ●
│   ├── hash.rs                      ●
│   ├── manifest.rs                  ●
│   ├── provider.rs                  ●
│   └── reconstruct.rs               ●
└── tsnet/                           →  Layer 7 に移動（現 lib.rs では未セクション / 元提案 Layer 8）
    ├── mod.rs                       ●
    ├── client.rs                    ●
    ├── integration_test.rs          ●
    ├── ipc.rs                       ●
    ├── sidecar.rs                   ●
    └── tsnet.v1.rs                  ●  (protobuf 生成ファイル)

│  ── Layer 8: 設定 & 周辺 ──────────────────────────────────────────────────────
│
├── config.rs                        ●
├── runtime_config.rs                ●
├── runtime_config/
│   └── builder.rs                   ●
├── python_runtime.rs                ●
├── schema_registry.rs               ●  ※ schema/ とは別物（JSON Schema レジストリ）
├── bootstrap.rs                     ●
├── diagnostics/
│   ├── mod.rs                       ●
│   └── manifest.rs                  ●
└── smoke.rs                         →  Layer 8 帰属を lib.rs で明示
```

### 変化サマリ

| モジュール | 元の位置づけ | 修正後の位置づけ | 物理変更 |
|---|---|---|---|
| `tsnet/` | 元提案 Layer 8 | **Layer 7**（Adapter） | なし |
| `share/` | 元提案に欠落 | **Layer 4**（実行エンジン） | なし |
| `validation/` | 元提案に欠落 | **Layer 6**（セキュリティ） | なし |
| `trust_store.rs` | 元提案に欠落 | **Layer 6**（セキュリティ） | なし |
| `schema/` | 元提案に欠落 | **Layer 6**（セキュリティ Domain） | なし |
| `schema_registry.rs` | 元提案に記載あり | **Layer 8**（設定・周辺） | なし |
| `smoke.rs` | 元提案に欠落 | **Layer 8**（周辺） | なし |
| `lockfile_runtime.rs` | 元提案に欠落 | **Layer 2**（契約層） | Phase 2B で任意統合 |
| `lockfile_support.rs` | 元提案に欠落 | **Layer 2**（契約層） | Phase 2B で任意統合 |
| `lockfile_tests.rs` | 元提案に欠落 | **Layer 2**（契約層） | Phase 2B で任意統合 |

---

## 参考: 今回の調査で否定された選択肢

| 案 | 否定の理由 |
|---|---|
| `capability/network.rs` 等 | capability はセキュリティ token の語（WASI/cap-std が確立）。I/O 実装置き場ではない |
| `sandbox/` モジュール | Rust 標準では `runtime/` が実行隔離の置き場。sandbox は概念名 |
| tsnet を Layer 8 に置く | tsnet は外部サービス adapter。設定・周辺（bootstrap, config）とは性質が異なる |
| network/fs を単一層に集約 | policy + adapter + runtime の3層にまたがるのが構造的に正しい |
