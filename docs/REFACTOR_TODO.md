# Refactor TODO

リファクタ系タスクを集約するファイル。完了済みは `[x]`、未着手は `[ ]`。

進行中の機能開発タスクは [TODO.md](./TODO.md) を参照。

---

## 🧹 core クレートのエラー設計リファクタ (P1, 2026-04-23 追加)

**背景**: `apps/ato-cli/core` はランタイム非依存のライブラリであるべきだが、公開 API で anyhow を露出している箇所が 24 ファイル・125 箇所あり、型安全性と層分離が崩れている。thiserror + 用途別 2 型構成は維持しつつ、anyhow 境界を段階的に整理する。

### 設計方針（合意済み）

- **2 型を統合しない**（役割が違うため）
  - 内部伝播用: `CapsuleError` (thiserror, `?` 用)
  - 診断出力用: `AtoError` (serde, code/phase/hint 付き JSON スキーマ)
  - → `error.rs` 先頭に両者の役割分担をドキュメントコメントで明記する
- **anyhow は core の公開 API から排除**。内部ヘルパーで使う場合も境界で `CapsuleError` に変換する。
- **`CapsuleError::Other(#[from] anyhow::Error)` は削除**（型安全性を殺す catch-all）。
- JSON 出力スキーマ (`ato_error.code`, `ato_error.phase` 等) は外部契約として維持する。

### Phase 1: 境界の型安全化（小さく価値が大きい）

- [x] `core/src/error.rs:63-64` — `Other(#[from] anyhow::Error)` バリアントを削除
- [x] `core/src/engine.rs` の `pub fn` 3 つを `crate::error::Result` に変更
  - [x] `discover_nacelle` — `anyhow!` を `CapsuleError::NotFound("nacelle engine")` などに
  - [x] `run_internal` — 失敗箇所を `CapsuleError::Execution` / `ProcessStart` に
  - [x] `run_internal_streaming` — 同上
- [x] `bail!` / `anyhow!` の engine.rs 内利用を `CapsuleError` バリアントへ置換
- [x] 呼び出し側 (ato-cli バイナリ) のコンパイルエラーを修正
- [x] `error.rs` の 2 型役割分担を doc-comment で明記

### Phase 2: anyhow 依存モジュールの段階的除去

- [x] `core/src/router.rs`
- [x] `core/src/reporter.rs`
- [x] `core/src/router/services.rs`
- [x] `core/src/types/signing.rs`
- [x] `core/src/launch_spec.rs`
- [x] `core/src/runtime_config/builder.rs`
- [x] `core/src/engine.rs`
- [x] `core/src/router/manifest_routing.rs`
- [x] `core/src/policy/egress_resolver.rs`
- [x] `core/src/resource/cas/index.rs`
- [x] `core/src/types/identity.rs`
- [x] `core/src/executors/oci.rs`
- [x] `core/src/orchestration.rs`
- [x] `core/src/executors/wasm.rs`
- [x] `core/src/resource/cas/bloom.rs`
- [x] `core/src/share/executor.rs`
- [x] `core/src/runtime_config.rs`
- [x] `core/src/hardware.rs`
- [x] `core/src/config.rs`
- [x] `core/src/common/paths.rs`
- [x] `core/src/executors/source.rs`
- [x] `core/src/validation/source_policy.rs`

### Phase 3: 仕上げ

- [x] `core/Cargo.toml` から `anyhow` 依存を削除（`bin/tar_pack_bench.rs` も `Box<dyn Error>` に移行）
- [x] bin クレート (`ato-cli/src/`) では anyhow を引き続き使用して OK
- [x] `cargo clippy -p capsule-core --all-targets -- -D warnings` green 化
- [x] `cargo test -p capsule-core` green 化

### 非ゴール

- `CapsuleError` / `AtoError` の統合（役割が異なるため）
- JSON スキーマのキー名変更（`ato_error` プレフィックスは外部契約）
- `bin` (ato-cli) 側からの anyhow 排除
- 型名の debrand リネーム（Capsule / Ato は製品名として許容）

---

## 🧹 core クレートのリーダブルコードリファクタ (P2, 2026-04-23 追加)

**背景**: コードの正確性は維持しつつ、「変更コスト」を下げるための可読性改善。公開 API シグネチャは一切変えない。

### Step 1 — `packers/capsule.rs`: 定数に WHY コメント追加（ゼロリスク）

- [x] `ZSTD_COMPRESSION_LEVEL: i32 = 19` にレベル選択理由のコメントを追記
  - 「pack は一度だけオフラインで実行するため、圧縮率優先（level 3 比 ~3× 小さく、~4× 遅い）」
- [x] `PAYLOAD_CHUNK_BYTES: usize = 64 * 1024` に選択理由のコメントを追記
  - 「syscall オーバーヘッドを抑えつつ一時アロケーションを小さく抑えるため」

### Step 2 — `isolation.rs`: `protects_key` → `is_key_protected` に改名（ゼロリスク）

- [x] `core/src/isolation.rs:118` — メソッド宣言を `is_key_protected` に変更
- [x] `core/src/isolation.rs:141` — 唯一の呼び出し箇所も合わせて変更
- `lib.rs` 非公開のため外部 API 影響なし

### Step 3 — `engine.rs`: `is_executable` ヘルパー抽出（ゼロリスク）

- [x] `(mode & 0o111) == 0` の判定を `is_executable(mode: u32) -> bool` に切り出し
- [x] `!is_executable(mode)` に書き換えて二重否定を排除
- [x] `run_internal_streaming` の `#[allow(dead_code)]` を削除し、代わりに「未接続の計画中 API」旨のドキュメントコメントに置換

### Step 4 — `manifest.rs`: デッドコード解消（低リスク）

- [x] `manifest_requires_cas_source` が workspace 全体で未使用であることを grep で確認
- [x] 未使用なら削除（同等のバリデーションは `TargetsConfig::validate_source_digest` が担っている）

### Step 5 — `orchestration.rs`: `TopoSorter` 構造体への切り出し（低〜中リスク）

- [x] 6 引数の内部関数 `visit` を `TopoSorter<'a>` 構造体のメソッドに変換
  - フィールド: `dependencies`, `visited`, `visiting`, `out`
  - メソッド: `visit(&mut self, current: &str, stack: &mut Vec<String>) -> Result<()>`
  - 完了後: `startup_order_from_dependencies` が `TopoSorter` を生成して `into_order()` を呼ぶだけになる
- [x] 既存テスト (`startup_order_sorts_dependencies`, `startup_order_rejects_cycles`) が通ることを確認

### Step 6 — `error.rs`: `ErrorKind` 構造体で 3 つの match を集約（中リスク）

- [x] `ErrorKind { code, name, phase }` 構造体を追加
- [x] `kind(&self) -> ErrorKind` メソッドを 1 つの 27-arm match で実装
- [x] `code()`, `name()`, `phase()` を `self.kind().xxx` の 1 行に集約
  - 新バリアント追加時のコスト: 7 箇所 → 1 箇所
- [x] 既存メソッド (`message`, `hint`, `resource`, `target`, `details`) はそのまま
- [x] 事前に `grep -rn "\.code()\|\.name()\|\.phase()"` で呼び出し元を確認

### 非ゴール

- `execution_plan/derive.rs` の大規模分割（中リスク、別タスクへ）
- `types/manifest.rs` の impl ブロック分割（コスメティック、ROI 低）
- `resource/artifact/manager.rs` の `_progress_tx` 削除（workspace 全体の呼び出し元調査が必要）

---

## 🧹 core クレートの重複コード除去 (P2, 2026-04-24 追加)

**背景**: 同一ロジックが複数ファイルにコピーされており、バグ修正や仕様変更時に複数箇所の同期が必要になっている。

### Step 1 — `bun_platform_triple` 関数の重複除去（ゼロリスク）

- [x] `src/lockfile_runtime.rs:879`, `src/lockfile_support.rs:551`, `src/packers/lockfile.rs:407` に同一関数が 3 コピー存在
- [x] `common/platform.rs` を新設し `pub(crate) fn bun_platform_triple(rust_triple: &str) -> Option<&'static str>` として集約
- [x] `lockfile_support.rs` のコピーは `#[allow(dead_code)]` 付きなので削除
- [x] 残り 2 ファイルを `common::platform::bun_platform_triple` に差し替え

### Step 2 — `detect_platform` 関数の重複除去（ゼロリスク）

- [x] `src/packers/lockfile.rs:358` と `src/packers/runtime_fetcher/mod.rs:681` に同一実装
- [x] `src/packers/lockfile.rs` 内のローカルコピーを削除し、`RuntimeFetcher::detect_platform()` を呼ぶように変更

### Step 3 — `sha256_hex` ヘルパーの集約（ゼロリスク）

- [x] 以下 7 ファイルに同一の 5 行 SHA-256 ラッパーが重複
- [x] `common/hash.rs` を新設し `pub(crate) fn sha256_hex(data: &[u8]) -> String` として集約
- [x] 各ファイルのローカルコピーを削除して `crate::common::hash::sha256_hex` に統一

### Step 4 — メモリパーサーの統一（低リスク）

- [x] `src/hardware.rs::parse_memory_to_bytes` は `src/types/utils.rs::parse_memory_string` の劣化コピー（小数値・KB/TB 単位が欠落）
- [x] `hardware.rs` の `parse_memory_to_bytes` を削除し、`types::utils::parse_memory_string` を使用するよう変更
- [x] `parse_memory_string` の `ParseError` を `None` にマップして既存呼び出しシグネチャを維持

### Step 5 — `manifest_dir` ヘルパーの抽出（ゼロリスク）

- [x] `manifest_path.parent().unwrap_or_else(|| PathBuf::from("."))` が以下 7 箇所に散在
- [x] `common/paths.rs` に `pub(crate) fn manifest_dir(path: &Path) -> PathBuf` を追加して集約

---

## 🧹 core クレートのデッドコード除去 (P2, 2026-04-24 追加)

**背景**: `#[allow(dead_code)]` で黙認されたデッドコードが蓄積しており、読み手の認知負荷と維持コストを上げている。workspace-wide grep で呼び出し元不在を確認してから削除する。

### 削除候補一覧

- [x] `src/signing/sign.rs:48` — `sign_bundle` ("Deprecated: legacy bundle format" コメント付き)
- [x] `src/signing/sign.rs:176` — `generate_keypair` (同上、テストのみが呼ぶ)
- [x] `src/signing/verify.rs` — `verify_bundle` ("use `verify_capsule_artifact_signature`" と案内されている)
- [x] `src/lockfile_support.rs:459` — `ensure_yarn_classic` (削除済み)
- [x] `src/lockfile_support.rs:503` — `ensure_bun` (削除済み)
- [x] `src/lockfile_support.rs:550` — `bun_platform_triple` (Step 1 と連動、削除済み)
- [x] `src/lockfile_support.rs:562` — `extract_zip` (削除済み)
- [x] `src/router.rs:882` — `requirements_vram_min()` メソッド (削除済み)
- [x] `src/hardware.rs:4-9` — `GpuReport` 構造体（`detect_nvidia_gpus` は `executors/oci.rs` で使用中のため保持、削除不要）
- [x] `src/signing/sign.rs` — `sign_bundle`, `generate_keypair` テストごと削除
- [x] `src/signing/verify.rs` — `verify_bundle` テストごと削除

### 要調査: CAS クライアント基盤（中リスク）

- [x] `src/resource/cas/client.rs:53` — `LocalCasClient::root` フィールド: `info!` ログ用のみ保持、`#[allow(dead_code)]` 維持が妥当
- [x] `src/resource/cas/client.rs:343` — `create_cas_client_from_env`: `packers/source.rs:93` で使用中。`#[allow(dead_code)]` アトリビュートを削除
- [x] CAS 統合は `packers/source.rs` から積極使用中。feature-gate 不要

---

## 🧹 core クレートの構造リファクタ (P3, 2026-04-24 追加)

**背景**: 実装の正確性は保ちつつ、将来の拡張コストを下げる構造改善。公開 API シグネチャは変えない。

### Step 1 — `lockfile.rs`: 10 引数 configure 関数をパラメータオブジェクトに（低リスク）

- [x] `configure_python_lockfile` / `configure_node_lockfile` / `configure_deno_lockfile` を `LockfileConfigContext<'_>` + `LockfileState<'_>` 構造体でリファクタ
- [x] `#[allow(clippy::too_many_arguments)]` 属性を全て除去

### Step 2 — `RuntimeSection`: 新ランタイム追加コストの削減（中リスク）

- [x] `ensure_runtime_if_missing!` マクロを追加し `ensure_node/python/deno_runtime_if_missing` を 3 行に集約
- [x] 注意コメントを追加: フィールド名変更は JSON スキーマ破壊的変更のため禁止

---

## 🧹 core クレートのパターン統一 (P3, 2026-04-24 追加)

**背景**: 小さな不整合が積み重なって「どちらを使うべきか」の判断コストを生んでいる。

### Step 1 — `as_str()` のある型に `Display` を実装（ゼロリスク）

以下の型は `as_str()` を持つが `Display` を実装していない。`{}` フォーマットが使えず呼び出し元が `.as_str()` を明示する必要がある:

- [x] `src/error.rs` — `AtoErrorPhase`
- [x] `src/ato_lock/schema.rs` — `KnownFeature`
- [x] `src/input_resolver.rs` — `ResolvedInputKind`
- [x] `src/ato_lock/schema.rs` — `UnresolvedReason` (`as_str()` が `Cow<'_, str>` を返す型)

Fix: 各型に `impl fmt::Display` を追加し `self.as_str()` に委譲。

### Step 2 — `FromStr` トレイトの実装（低リスク）

- [x] `src/ato_lock/schema.rs` — `KnownFeature::from_str`: `#[allow(clippy::should_implement_trait)]` で抑制中
- [x] `src/ato_lock/schema.rs` — `UnresolvedReason::from_str`: 同上
- [x] 両型に `impl std::str::FromStr` を追加し、既存メソッドを委譲先として残す

### Step 3 — プラットフォーム文字列フォーマットの統一調査（中リスク）

- [x] `src/lockfile.rs` の `"arm64"` / `"aarch64"` 混在は意図的:
  - `platform_target_key()` → `"macos-arm64"` (manifest key convention)
  - `platform_triple()` → `"aarch64-apple-darwin"` (Rust target triple convention)
  - 両関数にコメントを追加して意図を明記済み

### Step 4 — プラットフォームカバレッジチェックのパターン統一（低リスク）

- [x] `lockfile_has_required_platform_coverage` に runtime ごとのフィルタ方針の理由をインラインコメントで明記
  - deno/python/uv: バイナリ存在確認フィルタあり（プラットフォーム限定）
  - node/pnpm/yarn/bun: 全プラットフォーム必須（JS バンドルまたは広範なバイナリサポート）
- [ ] 可能なら `check_tool_platform_coverage(targets, tool_key, supported_platforms)` ヘルパーに抽出（中リスクのため別タスク）

---

## 🧹 core クレートの Clippy / 微細修正 (P3, 2026-04-24 追加)

### `result_large_err` 抑制の監査

- [x] `AtoExecutionError` の構造を確認: `String` × 4 + `Option<String>` × 3 + `Option<Value>` + `Vec<CleanupActionRecord>` + `ManifestSuggestion?` など明らかに > 256 bytes。`#![allow(clippy::result_large_err)]` は妥当。`Box<AtoExecutionError>` 化は全呼び出し元の変更を要するため、現時点では対応コスト > 恩恵。

### 個別の軽微修正（ゼロリスク）

- [x] `src/resource/artifact/manager.rs:56` — User-Agent が旧コードネーム `"gumball-engine/0.1.0"` → `"ato-cli/{CARGO_PKG_VERSION}"` に変更
- [x] `src/resource/artifact/manager.rs:76` — `starts_with` ガード後の `.unwrap()` → `let Some(hash) = uri.strip_prefix(CAS_PREFIX) else { return Err(...) };` に変更
- [x] `src/lockfile.rs:481` / `src/packers/sbom.rs:237` — `from_timestamp(0,0).expect("unix epoch")` → `DateTime::<Utc>::UNIX_EPOCH` 定数を使用
- [x] `src/signing/sign.rs:79` — `SystemTime::now().duration_since(UNIX_EPOCH).unwrap()` → `.unwrap_or_default().as_secs()` に変更

---

## 🧹 core クレートのドキュメント・テスト補強 (P3, 2026-04-24 追加)

### ドキュメント

- [x] `src/lib.rs` — クレートレベルの `//!` doc コメントを追加（クレートの責務、主要エントリポイントの説明）
- [x] `src/hardware.rs` — `pub fn requires_gpu` と `pub fn detect_nvidia_gpus` に `///` doc コメントを追加（失敗モード、外部コマンド依存の旨を記載）
- [x] `src/security/path.rs::validate_path` — `# Security` セクションで TOCTOU 制約を doc コメントに明記

### テスト

- [x] `src/security/path.rs::validate_path` — テスト確認済み: traversal/symlink escape/allowlist の各ケースが既に存在
- [x] `src/lockfile.rs::existing_lockfile_has_required_platform_coverage` — `lockfile_tests.rs` に既に 4 件のテストが存在（host-only fail、universal pass、deno-no-windows-arm64 など）
- [x] `src/hardware.rs::requires_gpu` — 6 件のユニットテストを追加（build.gpu フラグ、vram_min 各ケース）
