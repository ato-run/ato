# Unified Execution Model — Pipeline × Runtime (Draft)

> Status: **Draft** (v0.5.x 目標) — 議論中
> Scope: `ato-cli` の pipeline 層 (`run` / `publish` / `encap` / `decap`) と execute 層 (Node/Python/Deno/Bun...) を **1 枚の状態マシン + 1 枚の runtime 抽象**で扱うための統一仕様。
> Non-goal: narrative (Try / Keep / Share) の再定義。語り方は [`ATO_CLI_SPEC.md §3.1`](../accepted/ATO_CLI_SPEC.md) のまま維持する。

---

## 1. 背景

現在の `ato-cli` には **2 本の未統合な基盤抽象**がある。

### 1.1 Pipeline 層の分断

| コマンド | 実装 | `HourglassFlow` 使用 |
|---------|------|-------------------|
| `run`     | `pipeline/consumer.rs` → `HourglassPipeline` | ✅ `ConsumerRun` |
| `publish` | `pipeline/producer.rs` → `ProducerPipeline` | ✅ `ProducerPublish` / `ProducerPublishFinalize` |
| `encap`   | `share/mod.rs::execute_encap` | ❌ 独立 |
| `decap`   | `share/mod.rs::execute_decap` | ❌ 独立 |

`encap` / `decap` が `share/mod.rs` 内で ad-hoc に組まれており、`run` / `publish` と
エラー分類 (§14 taxonomy)・rollback (§3.3)・進捗 UI・capability gate が**二重実装**になっている。

### 1.2 Execute 層の host リーク

`provider-backed scheme` (`ato run npm:pkg`, `pypi:pkg`) や lifecycle shell 実行で、
**host の `PATH` が暗黙継承**されている。
PDF §5.4 の default-deny 原則が PATH に適用されておらず、ato-managed Node が
あっても host Node / host npm が先勝ちする構造 (#282, #146, #294)。

### 1.3 2 つは同じ型の問題

- pipeline の分断 → 同じ verify/install ロジックを 2 箇所で書く → §04 sandbox enforcement 追加時に drift する
- execute の host リーク → 同じ PATH 合成バグを言語ごとに 5 回書く → Python/Deno/Bun で再発する

**抽象化ポイントが違うだけで、本質は「共通 spine を切り出していない」ことに帰着する。**

---

## 2. 目標

### 2.1 統一する対象

1. **Pipeline Spine**: 全コマンドが `HourglassFlow` の variant として 1 つの `HourglassPipeline` で動く。phase の意味論は variant ごとに可変、machinery は共通。
2. **Runtime Spine**: 全言語が `RuntimeProvisioner` trait を満たす。PATH/env 合成は `ManagedRuntimePath` の 1 箇所を通る。
3. **接続点**: Pipeline の `Install` phase が `RuntimeProvisioner::ensure` を呼び、`Execute` phase が `ManagedRuntimePath` で spawn する。

### 2.2 非目標

- narrative (Try/Keep/Share) の再編: そのまま維持
- 新 `HourglassPhase` の追加: 既存 `[Install, Prepare, Build, Finalize, Verify, DryRun, Execute, Publish]` を再利用
- shim 生成 / rc-file 改変 / `cd` hook: ato は「1 回の実行を管理する」スコープを崩さない
- host tool 自動検出 (nvm / pyenv): zero-trust に反する

---

## 3. Pipeline Spine

### 3.1 HourglassFlow variant 表

| Variant                    | 使用する Phases (順序固定)               | 使用しない Phases | Owner コマンド |
|----------------------------|------------------------------------------|-------------------|-----------------|
| `ConsumerRun`              | Install → Prepare → Build → Verify → DryRun → Execute | Finalize, Publish | `run` |
| `ProducerPublish`          | Prepare → Build → Verify → Install → DryRun → Publish | Finalize, Execute | `publish` |
| `ProducerPublishFinalize`  | Prepare → Build → Install → Finalize → Verify → DryRun → Publish | Execute | `publish --finalize` |
| **`WorkspaceMaterialize`** | **Install → Verify** | Prepare, Build, Finalize, DryRun, Execute, Publish | **`decap`** |
| **`WorkspaceCapture`**     | **Prepare → Verify → Publish** | Install, Build, Finalize, DryRun, Execute | **`encap`** |

**`allowed_phases()` invariant**: 各 variant の「使用しない Phases」は
`HourglassPipeline::run` が呼び出しを**拒否**する（fail-closed）。
オプショナル扱いではなく、variant 定義の一部として enumerate される。

Phase の意味論は variant ごとに可変 — `WorkspaceMaterialize::Install` は
「share spec の install_steps 実行 + source materialization」であり、
`ConsumerRun::Install` の「capsule artifact unpack」とは別概念。
**Phase 名は役割タグであり、実装は variant が決める**。

**Phase 順序の justification**:
- `ConsumerRun`: artifact を持ってくる (Install) → workspace 整える (Prepare) → build → 検証 → dry-run → 実行。依存の流れ通り
- `ProducerPublish`: 手元で build → verify → registry に登録 (Install 意味論: "registry に入れる") → dry-run → publish
- `ProducerPublishFinalize`: `Install` (registry 登録) → `Finalize` (native delivery の codesign / notarization 等、post-install な後処理) → `Verify` (post-finalize 状態の検証) → publish。Finalize は Install 後・Verify 前に来るのが意味論として必然
- `WorkspaceMaterialize`: source を展開して install_steps を走らせる (Install) → tool/env 検証 (Verify)
- `WorkspaceCapture`: capture 準備 (Prepare) → filter/summary (Verify) → upload (Publish)

### 3.2 共通 machinery (variant 非依存)

1. **Phase 遷移ルール**: `HourglassPhaseSelection` が `start`/`stop` を制御
2. **エラー分類**: `PipelineAttemptError(phase, source, cleanup_report)` → §14 taxonomy の `phase` が自動で埋まる
3. **Rollback**: `CleanupJournal` → phase 境界で commit、失敗時 LIFO で unwind
4. **進捗表示**: `HourglassPhaseState { Run / Ok / Fail / Skip }` + `print_phase_line` (JSON mode 対応済)
5. **Capability gate**: 各 phase 開始前に `SessionGrants` チェック（ §3.4 の Tier2 opt-in）
6. **Audit log**: `HourglassPhaseResult { elapsed_ms, result_kind, ... }` の JSON ストリーム

### 3.3 Variant-specific な semantics + 共通 invariant の適用範囲

| Variant | Install の意味 | Verify の意味 | §04 network enforcement | Signature 検証 |
|---------|-------------|-------------|:------------------------:|:---------------:|
| `ConsumerRun` | capsule artifact fetch + unpack | signature + payload_hash | ✅ Verify で適用 | ✅ |
| `ProducerPublish` | registry 登録 | manifest hash | ✅ Verify で適用 | ✅ |
| `WorkspaceMaterialize` (decap) | source checkout + `install_steps` 実行 | tool 検証 + env 検査 + strict ゲート | ✅ Verify で適用 (strict ゲート含む) | ✅ |
| `WorkspaceCapture` (encap) | (使わない) | `summarize_and_filter_capture` (local-only) | ❌ **適用しない**（network を触らない phase） | ❌ (local capture) |

**Phase 名は tag、意味論は variant のドキュメントで定義する**。
README / spec では `"ConsumerRun の Install"` のように必ず variant 名で修飾する。

**共通 invariant の適用原則**:
`Verify` phase は variant ごとに意味論が違うため、共通 invariant の有無も variant ごとに
表で明示する。「Verify phase に書けば全 flow に自動適用」ではない。
`WorkspaceCapture::Verify` のような local-only phase に network enforcement を走らせることは
**machinery の濫用**であり、fail-closed 原則に反する。

### 3.4 エスカレーションパス（v0.5 → v0.5.x）

- **v0.5 (Pipeline は現状維持 + pinpoint fix のみ)**: `encap` / `decap` は `share/mod.rs` 独立実装のまま。spec で narrative vs pipeline 分離を明記済 ([ATO_CLI_SPEC.md §3.1](../accepted/ATO_CLI_SPEC.md))。
- **v0.5.x minor 2 (Pipeline Spine 統一)**:
  - `HourglassFlow` に `WorkspaceMaterialize` / `WorkspaceCapture` 追加
  - `materialize_loaded_share` を Install phase / Verify phase に分割
  - `WorkspaceMaterializeRunner` / `WorkspaceCaptureRunner` を `pipeline/phases/` に配置
  - `execute_decap` / `execute_encap` を Pipeline 経由に書き換え
  - 同時に §04 sandbox network enforcement を **ConsumerRun / ProducerPublish / WorkspaceMaterialize の Verify phase に追加** (上表参照)

**トリガー**: §04 enforcement を書く瞬間が pipeline 統一の最終期限。
network enforcement が必要な variant は 3 つなので、共通 machinery 化しない限り同じ実装が 3 箇所に書かれる。

---

## 4. Runtime Spine

### 4.1 `RuntimeProvisioner` trait

```rust
pub trait RuntimeProvisioner {
    type Version: ExactVersion;

    /// range + pref + lock + project file + default LTS → exact version を 1 つ
    fn resolve(&self, req: &RuntimeRequest) -> Result<Self::Version>;

    /// ~/.ato/runtimes/<tool>/<version>/ に binary を配置（CAS hit なら no-op）
    fn ensure(&self, version: &Self::Version) -> Result<PathBuf>;

    /// PATH に積むべきディレクトリ（複数あり得る: node + npm global bin 等）
    fn bin_dirs(&self, version: &Self::Version) -> Vec<PathBuf>;

    /// tool 固有の env (PYTHONHOME, NODE_PATH, GOPATH ...)
    fn env(&self, version: &Self::Version) -> HashMap<String, OsString>;

    /// provider-backed scheme 時の synthetic workspace 合成
    /// (npm:, pypi:, cargo:, gomod: ...)
    fn synthetic_workspace(&self, spec: &PackageSpec) -> Result<PathBuf>;
}
```

実装:

- **`v0.5.x minor 1`**: `NodeProvisioner` — #294 Acceptance criteria を満たす。同時に既存 `src/application/engine/install/provider_target/` の `PROVIDER_NODE_RUNTIME_VERSION` 等 Node 関連 constant を **NodeProvisioner に吸収完了**させ、managed point を 1 箇所に集約する (v0.5 中は並存 OK だが、minor 1 までに移行を終える)
- **`v0.5.x minor 3`**: `PythonProvisioner` — uv を subprocess として呼ぶ
- **`v0.6+`**: `DenoProvisioner` / `BunProvisioner` / `RustProvisioner` / `GoProvisioner`

### 4.2 `ManagedRuntimePath` — PATH 合成の単一責任

```rust
pub struct ManagedRuntimePath {
    entries: Vec<PathBuf>,  // 先頭が最優先
}

impl ManagedRuntimePath {
    pub fn new() -> Self;
    pub fn with_runtime<P: RuntimeProvisioner>(self, p: &P, v: &P::Version) -> Self;
    pub fn with_system_baseline(self) -> Self;  // /usr/bin のみ (coreutils)
    pub fn compose(self) -> OsString;           // PATH env value
}
```

**不変条件**:

1. `ManagedRuntimePath` を通さない PATH 構築は禁止。既存の `apply_allowlisted_env` を本抽象経由に書き換える
2. `with_system_baseline` は POSIX coreutils を含むディレクトリ**のみ**。`/usr/local/bin`・`$HOME/.nvm`・`$HOME/.pyenv` は一切含まない
3. host PATH 継承は明示 opt-in (`ATO_HOST_TOOL_PASSTHROUGH=node,python`) のみ
4. `sh -lc` は**禁止**。login shell は `/etc/profile.d/*` を読み込み nvm 等を復活させる → zero-trust に反する。`/bin/sh -c` または直接 exec

**Escape hatch の正式仕様** (v0.5 で固定):

- 環境変数名: `ATO_HOST_TOOL_PASSTHROUGH`
- 値: カンマ区切りの tool 名 list (例: `node`, `python`, `node,python`)
- wildcard (`*`) は **v0.5 では未対応**。将来 `ATO_HOST_PATH_PASSTHROUGH=*` のような別名の env を予約しておき、list 形式との後方互換を保つ
- 未定義 / 空文字列 = 何も継承しない（既定 = default-deny）
- 解釈責任: `ManagedRuntimePath::with_host_tool()` が tool 名を受け取り、その tool だけ host `which` 結果を PATH 末尾に追加。**先頭には絶対に置かない**（managed が常に先勝ち）

### 4.3 Synthetic Workspace

provider-backed scheme (`npm:`, `pypi:`) 専用の作業領域:

```
~/.ato/cache/synthetic/
  <provider>/<pkg>@<exact_version>/
    ├── capsule.toml        (生成)
    ├── <package file>       (npm: package.json / pypi: pyproject.toml)
    ├── <lock file>          (npm: package-lock.json / pypi: uv.lock)
    ├── <deps dir>           (npm: node_modules / pypi: .venv)
    └── ato.lock.json        (exact pin + integrity)
```

**用語の定義** (#282 系で繰り返し出てきた混乱を避けるため、本仕様では厳密に区別する):

- **User cwd**: ユーザーが ato コマンドを起動した時の current working directory。ato は **一切触らず、artifact も残さない**
- **Execution cwd**: 実行対象の child process が spawn される cwd。provider-backed scheme の場合は synthetic workspace、local path の場合は manifest のあるディレクトリ

**不変条件**:

1. Content-addressed: `(provider, pkg, exact_version)` が cache key
2. **User cwd** は読み書きしない。artifact を残さない
3. **Execution cwd** = synthetic workspace。child process は自分が独立 project 内と認識
4. Cache 2 回目以降は `Install` phase を skip して Execute へ（=「毎回の `npm install`」が消える）

**GC / Retention policy** (v0.5.x minor 1 実装目標):

- Metadata: 各 synthetic workspace に `.ato-access` ファイル (最終実行の unix timestamp) を持たせる
- 既定の retention: **30 日間 access されなかった workspace を削除対象**
- 設定: `config.toml` の `[cache] synthetic_retention_days = 30`
- `ato gc` コマンド (v0.5.x minor 1 で新設): `--synthetic` / `--all` / `--dry-run` / `--older-than <days>` を受ける
- 自動 GC: `ato run` 実行時に確率 `p=0.01` で `synthetic_gc_probe()` を non-blocking で呼ぶ (pnpm の store GC と同様のモデル)。fail は log のみで握り潰す

**v0.5 の暫定対応**: GC は未実装。リリースノートに「`~/.ato/cache/synthetic/` は現時点で手動削除が必要、使用量は `du -sh` で確認可能、v0.5.x minor 1 で自動 GC 導入予定」と明記。

### 4.4 Lockfile integrity（nix モデル拡張）

`ato.lock.json` の `runtime_tools` 層は version だけでなく artifact hash を持つ:

```json
{
  "runtime_tools": {
    "node": {
      "version": "20.11.0",
      "artifact_url": "https://nodejs.org/dist/v20.11.0/node-v20.11.0-linux-x64.tar.xz",
      "integrity": "sha256-..."
    },
    "python": {
      "version": "3.12.7",
      "artifact_url": "https://github.com/astral-sh/python-build-standalone/...",
      "integrity": "sha256-..."
    }
  }
}
```

PDF §2.8 Foundation Profile `lock` 層と整合。
mise / asdf はこのレベルまで追わない。uv は Python でやっている。**ato は全 provisioner で必須**。

---

## 5. Pipeline × Runtime の接続

### 5.1 Phase ⇔ Provisioner のマッピング

`ConsumerRun` を例にとると、`npm:pkg` 実行時の各 phase の仕事:

| Phase    | Provisioner 呼び出し | 出力 |
|----------|----------------------|------|
| Install  | `ensure(node_version)` + `npm ci` in synthetic workspace | `node_modules/` |
| Prepare  | `synthetic_workspace(spec)` path 解決, bin path 特定 | `PreparedRunContext` |
| Build    | (npm では no-op; TS なら `tsc` ここ) | build artifact or skip |
| Verify   | lockfile integrity 検証 + signature (§3.3) | Verified |
| Execute  | `ManagedRuntimePath::new().with_node(v).compose()` で PATH 固定 → `<managed_node>/bin/node <bin>` を sandbox spawn | exit code |

**`ato run npm:pkg` と `ato run ./local-path` は同じ `ConsumerRun` 5 stage**。
違いは `Install` の入力 (`PackageSpec` vs `LocalManifest`) だけ。
実装は `InstallPhase` trait で抽象化される。

### 5.2 SharedPort による DI

既存の `SharedSourcePort` / `SharedTargetPort` (`pipeline/phases/install.rs`) を拡張:

```rust
pub struct InstallPhaseRequest {
    pub source_spec: SourceSpec,       // LocalArtifact | ProviderBacked | ShareSpec
    pub target_spec: TargetSpec,       // Capsule | Synthetic | Workspace
    pub runtime: Option<RuntimeRequest>, // Provisioner に渡す要件
}
```

`SourceSpec::ProviderBacked` と `TargetSpec::Synthetic` が追加されることで、
`run npm:pkg` が `ConsumerRun` pipeline にそのまま乗る。

### 5.3 Sandbox との接続 — 責務境界

Execute phase の spawn は **必ず nacelle 経由**。
env / PATH の最終決定権は **nacelle 側が持つ** (PDF §5.2 "Sandbox Enforcer 専任" と整合)。

**責務 matrix**:

| 責務 | ato-cli 側 | nacelle 側 |
|------|:----------:|:----------:|
| `ManagedRuntimePath` の組み立て (entries の順序決定) | ✅ | |
| 許可する env 名の allowlist 計算 | ✅ | |
| nacelle への引き渡し (IPC / argv) | ✅ | |
| `env_clear()` / `unsetenv()` による host env の剥奪 | | ✅ |
| 許可 env の再注入 | | ✅ |
| 違反時の fail-closed (予期せぬ env が残っていたら abort) | | ✅ |
| PATH resolution (`execvp`) | | ✅ |

ato-cli は "**何を注入すべきか**" を決め、nacelle は "**それ以外が残っていないことを保証**" する。
`env_clear` を 2 度やる / どちらもやらない事故を防ぐため、**ato-cli は `env_clear()` 相当の処理を自分では走らせない**。
nacelle が受け取った env は append-only （ato-cli が指定したもの以外は残らない） と仮定する。

---

## 6. Error Taxonomy (§14 連携)

### 6.1 JSON envelope

統一された失敗 envelope:

```json
{
  "status": "error",
  "phase": "install",
  "flow": "consumer_run",
  "error_code": "E204_RUNTIME_PROVISION_FAILED",
  "message": "Failed to ensure node 20.11.0: network error",
  "actionable_fix": "retry after checking network",
  "cleanup": {
    "status": "complete",
    "actions": [...]
  }
}
```

- `flow` = `HourglassFlow` の snake_case 名（全 variant で同じ schema）
- `phase` = `HourglassPhase::as_str()`
- `cleanup` = `PipelineAttemptError::cleanup_report`

### 6.2 Error code の配分

| Range | 意味 | 担当 |
|-------|------|------|
| `E100-E199` | Pipeline 層（phase 進行・gate 失敗） | `HourglassPipeline` |
| `E200-E299` | Runtime 層（provision・PATH 合成・synthetic workspace） | `RuntimeProvisioner` |
| `E300-E399` | Share 層（encap/decap specific: spec digest, signature, ...） | `WorkspaceCapture/Materialize` runners |
| `E400-E499` | Network enforcement（§04, v0.5.x で追加） | Verify phase shared |
| `E999` | unclassified | fallback |

既存の `E102` / `E999` (#165) はこの枠で再分類される。

---

## 7. 実装ロードマップ

> **Scope 原則**: v0.5 リリースを後ろ倒しにしない。大きな refactor は v0.5.x minor で段階化する。

### 7.1 v0.5（RFC merge + pinpoint fix のみ）

- [x] ATO_CLI_SPEC §3.1 に narrative vs pipeline 分離追記
- [x] `HourglassFlow` enum に v0.5.x variant の TODO コメント
- [ ] 本ドラフトを RFC (draft) として merge
- [ ] **#294 の pinpoint fix**: 既存 `apply_allowlisted_env` で ato-managed Node の bin path を PATH 先頭に prepend。`ManagedRuntimePath` 抽象化・`RuntimeProvisioner` trait 化は**行わない** (v0.5 ブロッカーを最小 surface area で閉じる)
- [ ] リリースノートに「`~/.ato/cache/synthetic/` の自動 GC は v0.5.x minor 1 で導入予定」を明記

### 7.2 v0.5.x minor 1: Runtime Spine 導入

- [ ] `RuntimeProvisioner` trait 定義
- [ ] `NodeProvisioner` 実装 (既存 `provider_target::PROVIDER_NODE_RUNTIME_VERSION` を吸収)
- [ ] `ManagedRuntimePath` 抽象化 (§4.2)
- [ ] 既存 `apply_allowlisted_env` を `ManagedRuntimePath` 経由に書き換え (v0.5 pinpoint fix の置き換え)
- [ ] `ATO_HOST_TOOL_PASSTHROUGH` 実装 (§4.2 escape hatch)
- [ ] `ato gc --synthetic` + `config.toml [cache] synthetic_retention_days` + 確率的自動 GC (§4.3)
- [ ] `ato.lock.json` に `runtime_tools.<tool>.integrity` 拡張 (§4.4)

### 7.3 v0.5.x minor 2: Pipeline Spine 統一

- [ ] `HourglassFlow::WorkspaceMaterialize` / `WorkspaceCapture` variant 追加
- [ ] `allowed_phases()` invariant 実装 (§3.1)
- [ ] `materialize_loaded_share` を `Install` / `Verify` 2 phase に分割
- [ ] `pipeline/phases/{encap,decap}.rs` 新設
- [ ] `pipeline/workspace.rs` に pipeline struct
- [ ] `execute_decap` / `execute_encap` を Pipeline 経由に書き換え
- [ ] §04 sandbox network enforcement を **ConsumerRun / ProducerPublish / WorkspaceMaterialize** の Verify phase に実装 (§3.3 表の通り `WorkspaceCapture` は除外)

### 7.4 v0.5.x minor 3+: Runtime 拡張

- `PythonProvisioner` (uv を subprocess として呼ぶ)
- `DenoProvisioner` / `BunProvisioner`
- `RustProvisioner` / `GoProvisioner`

各追加は `RuntimeProvisioner` trait を埋めるだけの mechanical work (2〜3 日 / 言語)。

### 7.5 Error taxonomy confirmation

- #165 (E102/E999 細分化) を本仕様の §6.2 で閉じる
- §14 docs (`docs.ato.run/errors`) を `flow` × `phase` でクロスリンク

---

## 8. テスト戦略

### 8.1 Pipeline 層

- `Recorder` パターン（`consumer.rs`/`producer.rs` のテスト流用）で全 variant のフェーズ順を固定
- `PipelineAttemptError` の phase 名・cleanup 動作を全 variant で検証

### 8.2 Runtime 層

- `NodeProvisioner`: (a) provider-backed `npm:pkg`, (b) local manifest with `engines.node`, (c) **PATH から node を抜いた subprocess 環境** の 3 ケース
- `ManagedRuntimePath`: `with_system_baseline` が coreutils のみ・`/usr/local/bin` を含まないことを OS 別に検証
- `SyntheticWorkspace`: CAS key が `(provider, pkg, exact_version)` で一意になることの invariant テスト

### 8.3 Integration

- `ato run npm:mintlify -- dev` が host Node 無し環境で動くこと（ CI で PATH から node を抜くフィクスチャ）
- `decap` の Verify phase で §04 enforcement が効くこと（v0.5.x minor 1 以降）

---

## 9. 既存仕様との関係

| 参照 | 関係 |
|------|------|
| [ATO_CLI_SPEC §3.1](../accepted/ATO_CLI_SPEC.md) | narrative 階層の定義は維持、pipeline 実装層は本 RFC が定義 |
| [NACELLE_SPEC](../accepted/NACELLE_SPEC.md) | Execute phase の spawn 契約を本 RFC から参照 |
| [RUNTIME_AND_BUILD_MODEL](../accepted/RUNTIME_AND_BUILD_MODEL.md) | `RuntimeProvisioner` trait を本 RFC が定義する上位層 |
| [EXECUTIONPLAN_ISOLATION_MODEL](../accepted/EXECUTIONPLAN_ISOLATION_MODEL.md) | PATH / env の default-deny を本 RFC が PATH にも拡張 |
| [SECURITY_AND_ISOLATION_MODEL](../accepted/SECURITY_AND_ISOLATION_MODEL.md) | PDF §5.4 の env default-deny を PATH に適用する実装根拠 |
| [CAPSULE_FORMAT_V2](../accepted/CAPSULE_FORMAT_V2.md) | `ato.lock.json::runtime_tools` スキーマ拡張を本 RFC が規定 |

---

## 10. Open Questions

1. **`HourglassPhase` 名前空間の衝突**: `WorkspaceCapture::Publish` と `ProducerPublish::Publish` は別セマンティクス。README / エラーメッセージで必ず `flow.phase` 表記にするべきか、`PublishKind` のような sub-tag を入れるべきか。
2. **`WorkspaceMaterialize::Install` の security 境界**: `install_steps` は share spec 作成者が書いた任意のシェルコマンドであり、decap 実行時に展開される。これを走らせる際の capability gate / sandbox 境界 / `ManagedRuntimePath` 適用を明文化する必要がある。現状は PDF §3.4 の拒否リストに暗黙依存。v0.5.x minor 2 の実装開始前に §5 の接続表に `WorkspaceMaterialize::Install` 行を追加するかたちで決着させる。
3. **Provisioner の Wasm 化**: `lock-draft-engine` と同じく `RuntimeProvisioner::resolve` を pure function として切り出し、`ato-web` / `ato-play-edge` から呼べるようにするか (v0.6 以降)。
4. **Cache GC の aggressive さ**: 30 日 LRU は npm (無期限) と pnpm (7 日) の中間。実ユーザーのディスク使用を v0.5.x minor 1 で telemetry して調整するか、設定可能にするだけで固定は避けるか。

> 過去の Open Questions (§10 Q2 "escape hatch 粒度", Q4 "ProviderKind 合流タイミング") は §4.2 / §4.1 本文で決着済みのため削除。

---

## 11. 参考資料

- [docs/chats/ato-cli v0.5 リリース前レビューと ToDo.md](../../chats/ato-cli%20v0.5%E3%83%AA%E3%83%AA%E3%83%BC%E3%82%B9%E5%89%8D%E3%83%AC%E3%83%93%E3%83%A5%E3%83%BC%E3%81%A8ToDo.md)
- Issue #282 (Node/TS entrypoint inference) / #146 (GitHub install) / #294 (npm: LTS auto-provision)
- Issue #165 (E102/E999 error taxonomy 細分化)
