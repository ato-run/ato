# capsule-core アーキテクチャ概要

> コードリーディング時の道標となるドキュメント。実装の詳細よりも「どこに何があるか」「なぜそこにあるか」を重視して記述する。

---

## 1. クレートの立ち位置

```
apps/ato-cli/
├── src/          ← ato-cli バイナリ（CLI エントリポイント、I/O アダプタ）
├── core/         ← capsule-core ライブラリ（本ドキュメントの対象）
└── lock-draft-engine/  ← lockfile 草稿評価エンジン（Wasm 互換の軽量ライブラリ）
```

`capsule-core` はランタイム非依存のライブラリクレートである。CLI バイナリ (`ato-cli`) は `capsule-core` を薄いアダプタで包む形で使用する。`capsule-core` 内で `anyhow` は使用しない（`CapsuleError` / `AtoError` の 2 型設計を参照）。

---

## 2. エラー設計

**ファイル**: `src/error.rs`

2 つのエラー型が厳密に役割分担されており、統合しない。

| 型 | 用途 | 特徴 |
|---|---|---|
| `CapsuleError` | ライブラリ内部での伝播 | `thiserror`、`?` で伝播、バリアントでパターンマッチ可能 |
| `AtoError` | 診断 JSON 出力 | `serde`、`code` / `phase` / `hint` フィールドが外部契約 |

`CapsuleError` は CLI 境界で `AtoError` に変換される。`core` 内のすべての `pub fn` は `crate::error::Result<T>`（= `Result<T, CapsuleError>`）を返す。

### CapsuleError バリアント早見表

```rust
Config(String)       // 設定値・マニフェスト解釈エラー
Manifest(PathBuf, String) // capsule.toml の読み込み・検証エラー
Io(#[from] io::Error)    // ファイル I/O
Network(#[from] reqwest::Error) // HTTP
Execution(String)    // プロセス実行失敗
ProcessStart(String) // spawn 失敗
ContainerEngine(String) // Docker / Podman 操作
HashMismatch(String, String) // チェックサム不一致
Runtime(String)      // その他ランタイムエラー
Crypto(String)       // 署名・鍵操作
NotFound(String)     // リソース・バイナリ不在
Pack(String)         // アーカイブ生成エラー
```

---

## 3. モジュール全体マップ

モジュールを責務の層に分類する。

```
┌─────────────────────────────────────────────────────────────────┐
│ Layer 1: 型・共通ユーティリティ                                   │
│   error  types  metrics  reporter  common  hardware              │
├─────────────────────────────────────────────────────────────────┤
│ Layer 2: マニフェスト & ロックファイル（契約層）                   │
│   manifest  lockfile  lock_runtime  ato_lock                     │
├─────────────────────────────────────────────────────────────────┤
│ Layer 3: ルーティング & 解決                                      │
│   router  input_resolver  handle  handle_store                   │
│   launch_spec  importer                                          │
├─────────────────────────────────────────────────────────────────┤
│ Layer 4: 実行エンジン                                             │
│   engine  executors  orchestration  execution_plan               │
│   runner  runtime  lifecycle                                     │
├─────────────────────────────────────────────────────────────────┤
│ Layer 5: パッキング                                               │
│   packers/{capsule,bundle,lockfile,source,web,wasm,oci}         │
│   packers/{payload,pack_filter,runtime_fetcher,sbom}            │
├─────────────────────────────────────────────────────────────────┤
│ Layer 6: セキュリティ                                             │
│   security  signing  isolation  policy                           │
├─────────────────────────────────────────────────────────────────┤
│ Layer 7: リソース管理                                             │
│   resource/{cas,artifact,ingest}  capsule                       │
├─────────────────────────────────────────────────────────────────┤
│ Layer 8: 設定 & 周辺                                             │
│   config  runtime_config  python_runtime  schema_registry        │
│   bootstrap  discovery  diagnostics                              │
└─────────────────────────────────────────────────────────────────┘
```

---

## 4. 主要データフロー

### 4.1 `ato run`（実行フロー）

```
CLI: run コマンド
  │
  ▼
input_resolver::resolve_authoritative_input()
  │  ローカルパス / GitHub URL / レジストリハンドルを正規化
  ▼
router::route_manifest()            ← 最重要エントリポイント
  │  capsule.toml を読み、RuntimeKind を決定し ManifestData を返す
  │  サブモジュール: manifest_routing / lock_routing / services
  ▼
lock_runtime::resolve_lock_runtime_model()
  │  ato.lock.json or capsule.lock を読み ResolvedLockRuntimeModel を構築
  ▼
engine::discover_nacelle()          ← nacelle バイナリを探索
  │  優先順: CLI フラグ > 環境変数 > マニフェスト隣 > PATH
  ▼
engine::run_internal() / run_internal_streaming()
  │  nacelle を子プロセスとして起動、IPC で設定を注入
  ▼
runner::SessionRunner::run()
  │  プロセスを監視し UnifiedMetrics を収集
  ▼
reporter::UsageReporter::report_final()
```

### 4.2 `ato pack`（パッキングフロー）

```
CLI: pack コマンド
  │
  ▼
router::route_manifest()
  ▼
lockfile::ensure_lockfile()
  │  capsule.lock が存在・有効なら再利用、なければ generate_lockfile()
  │  内部: lock-draft-engine（Wasm 互換の草稿評価）→ uv/npm/deno 等で解決
  ▼
packers::capsule::pack()
  │  PAX TAR + zstd でアーカイブ生成
  │  - payload.rs: ファイルのチャンク化・ハッシュ計算
  │  - pack_filter.rs: .gitignore 相当の除外フィルタ
  │  - sbom.rs: SBOM 埋め込み
  │  - runtime_fetcher: ランタイムバイナリのダウンロード・検証
  ▼
signing::sign_capsule_artifact()    ← オプション
  ▼
成果物: *.ato アーカイブ
```

---

## 5. 主要モジュール詳解

### 5.1 `router/` — マニフェストルーティング

**責務**: `capsule.toml` を読み込み、実行に必要なすべての情報を `ManifestData` として整理する。

| ファイル | 役割 |
|---|---|
| `router.rs` | `route_manifest()` 公開 API、`ManifestData` 型 |
| `manifest_routing.rs` | `[targets]` テーブルから `ResolvedLockRuntimeModel` を合成 |
| `lock_routing.rs` | `ato.lock.json` ベースのルーティング |
| `services.rs` | `[services]` テーブルのサービス定義を解決 |

`ManifestData` は `plan` フィールドを持ち、executors や packers はこれを受け取る。

### 5.2 `lockfile.rs` — capsule.lock 管理

**責務**: `capsule.lock` の生成・検証・読み込み。ランタイムバイナリの URL/SHA-256 をピン留めする。

主要な公開関数:

```rust
ensure_lockfile(manifest_path, manifest_raw, ...) -> Result<CapsuleLock>
generate_lockfile(manifest_path, manifest_raw, ...) -> Result<CapsuleLock>
verify_lockfile_manifest(lockfile, manifest) -> Result<()>
lockfile_has_required_platform_coverage(lockfile, manifest) -> Result<bool>
```

内部実装の構成:
- `LockfileConfigContext<'_>` — 共通読み取り引数のまとめ（`manifest_raw`, `reporter` 等）
- `LockfileState<'_>` — 可変出力引数のまとめ（`runtimes`, `tools`, `targets`）
- `configure_python/node/deno_lockfile()` — 各ランタイムのロック生成
- `ensure_runtime_if_missing!` マクロ — ランタイム解決ヘルパーの共通パターン
- `#[path = "lockfile_runtime.rs"]` / `lockfile_support.rs` — 同モジュール扱いの実装ファイル

**プラットフォーム文字列の使い分け**:
- `platform_target_key()` → `"macos-arm64"` (manifest key convention、macOS 短縮形)
- `platform_triple()` → `"aarch64-apple-darwin"` (Rust target triple、toolchain 向け)

### 5.3 `ato_lock/` — ato.lock.json v1

**責務**: `capsule.lock` とは別の canonical lock フォーマット (`ato.lock.json`)。入力リゾルバやインポートフローで使用。

| ファイル | 役割 |
|---|---|
| `schema.rs` | `AtoLock` 型定義、`KnownFeature`、`UnresolvedReason` |
| `canonicalize.rs` | canonical プロジェクション（同一性比較用の正規化）|
| `closure.rs` | closure digest 計算 |
| `hash.rs` | lock_id（JCS + SHA-256）計算 |
| `validate.rs` | バリデーション |

### 5.4 `engine.rs` — nacelle 実行エンジン

**責務**: nacelle バイナリの探索と起動。

```
discover_nacelle(EngineRequest) -> Result<PathBuf>
  優先順: explicit_path > NACELLE_PATH 環境変数 > manifest 隣接 > PATH

run_internal(nacelle_path, plan, payload, ...) -> Result<UnifiedMetrics>
  nacelle を子プロセスで起動し stdin に設定 JSON を注入

run_internal_streaming(...)
  stdout をストリーミングで読みながら実行（計画中 API）
```

### 5.5 `execution_plan/` — 実行計画

**責務**: ato.lock.json からランタイム実行計画を導出する。

| ファイル | 役割 |
|---|---|
| `model.rs` | `ExecutionPlan` 型 |
| `derive.rs` | `AtoLock` → `ExecutionPlan` の導出ロジック |
| `canonical.rs` | canonical 実行計画（hash 同一性比較用） |
| `guard.rs` | 実行前ガードチェック |
| `error.rs` | `AtoExecutionError`（JSON 診断出力用、>256 bytes のため `result_large_err` 許容）|

### 5.6 `executors/` — ランタイムアダプタ

**責務**: 実行モード別の起動ロジック。すべて `execute(plan: &ManifestData) -> Result<i32>` を公開する。

| ファイル | 実行モード |
|---|---|
| `oci.rs` | Docker / Podman コンテナ |
| `wasm.rs` | wasmtime |
| `source.rs` | ソース実行（nacelle 経由、バンドルを一時ファイルに展開して実行）|

### 5.7 `packers/` — アーカイブ生成

**責務**: `.ato` カプセルアーカイブの生成。

```
packers/capsule.rs    ← メインエントリ。PAX TAR + zstd で .ato を生成
packers/payload.rs    ← TAR ペイロードのチャンク化・ハッシュ・再構成
packers/pack_filter.rs ← .gitignore ルールによるファイル除外
packers/runtime_fetcher/ ← ランタイムバイナリのダウンロード・SHA-256 検証
  ├── mod.rs          ← RuntimeFetcher (detect_platform, fetch_runtime 等)
  ├── fetcher.rs      ← HTTP ダウンロード実装
  └── verifier.rs     ← チェックサム検証
packers/sbom.rs       ← SPDX SBOM 生成・埋め込み
packers/lockfile.rs   ← lockfile のパッキング処理
packers/source.rs     ← ソースファイルの CAS インジェスト
packers/bundle.rs     ← 単一バンドルファイル形式
packers/web.rs        ← Web アプリパッキング
packers/oci.rs        ← OCI イメージ参照のパッキング
```

**定数（capsule.rs）**:
- `ZSTD_COMPRESSION_LEVEL = 19`: pack は一度だけ実行するため圧縮率優先（level 3 比 ~3× 小さく ~4× 遅い）
- `PAYLOAD_CHUNK_BYTES = 64 * 1024`: syscall オーバーヘッドと一時アロケーションのバランス点

### 5.8 `resource/` — リソース管理

```
resource/cas/           ← Content-Addressable Storage
  client.rs             ← CasClient トレイト、LocalCasClient / HttpCasClient
  index.rs              ← SQLite ベースのローカル CAS インデックス
  bloom.rs              ← AtoBloomFilter（Bloom フィルタで存在確認を高速化）
  chunker.rs            ← FastCDC コンテンツ定義チャンキング

resource/artifact/      ← ランタイムアーティファクト管理
  manager.rs            ← ArtifactManager（HTTP ダウンロード・キャッシュ）
  cache.rs              ← ローカルキャッシュ
  registry.rs           ← アーティファクトレジストリ

resource/ingest/        ← 外部リソース取り込み
  fetcher.rs            ← fetch_resource() (HTTP / S3)
  http.rs               ← HTTP ストリーミング取り込み
```

CAS クライアントは `packers/source.rs` から積極的に使用されている。実験的コードではなく本番統合済み。

### 5.9 `security/` & `signing/` — セキュリティ

```
security/path.rs    ← validate_path(): TOCTOU 制約付きパスバリデーション
                       absolute パス確認 → .. 排除 → canonicalize → allowlist チェック
signing/
  sign.rs           ← Ed25519 署名生成、sign_capsule_artifact()
  verify.rs         ← 署名検証
isolation.rs        ← HostIsolationContext: ツール実行用の隔離ディレクトリ構造
policy/egress_resolver.rs ← ネットワーク egress ポリシー解決
```

### 5.10 `types/` — 共有型定義

`capsule.toml` スキーマ型と実行モデル型を定義する。

```
types/manifest.rs   ← CapsuleManifest（capsule.toml の Rust 型）
types/orchestration.rs ← OrchestrationPlan, ResolvedService 等
types/signing.rs    ← StoredKey, SignatureJsonContent 等
types/identity.rs   ← DeveloperKey（did:key / ed25519: 形式）
types/utils.rs      ← parse_memory_string() 等の共通パーサー
```

### 5.11 `common/` — 汎用ユーティリティ

```
common/hash.rs      ← sha256_hex(data: &[u8]) -> String（7 ファイルで共用）
common/paths.rs     ← manifest_dir(), nacelle_home_dir_or_workspace_tmp() 等
common/platform.rs  ← bun_platform_triple(rust_triple) -> Option<&'static str>
```

### 5.12 `reporter.rs` & `metrics.rs` — 観測性

```rust
// reporter.rs
trait UsageReporter   // report_sample(), report_final()
trait CapsuleReporter // notify(), warn(), progress_start/inc/finish()
struct NoOpReporter   // テスト・内部処理用のダミー実装

// metrics.rs
struct UnifiedMetrics   // session_id, started_at, resources, metadata
struct ResourceStats    // duration_ms, peak_memory_bytes, cpu_seconds
enum RuntimeMetadata    // Nacelle { pid, exit_code } | Oci | Wasm
```

---

## 6. 重要な依存関係

```
capsule-core
  ├── lock-draft-engine (path dep) — lockfile 草稿の評価
  ├── tokio "full"                 — 非同期ランタイム
  ├── bollard                      — Docker API クライアント
  ├── ed25519-dalek                — Ed25519 署名
  ├── blake3                       — コンテンツハッシュ
  ├── fastcdc                      — コンテンツ定義チャンキング
  ├── rusqlite "bundled"           — CAS インデックス (SQLite)
  ├── zstd "zstdmt"                — マルチスレッド圧縮
  ├── toml                         — capsule.toml パース
  ├── serde + serde_json + serde_jcs — シリアライゼーション
  ├── reqwest (rustls-tls)         — HTTP クライアント（OpenSSL 非依存）
  └── thiserror                    — CapsuleError 定義
```

---

## 7. ファイル名パターンの慣習

| パターン | 意味 |
|---|---|
| `*_tests.rs` | `#[path = "..._tests.rs"] mod tests;` で同一モジュールに取り込まれるテストファイル |
| `lockfile_runtime.rs` | `lockfile.rs` 内で `#[path = "lockfile_runtime.rs"] mod lockfile_runtime;` として使用 |
| `lockfile_support.rs` | 同上（lockfile.rs のサブモジュール実装） |
| `mod.rs` + `*.rs` | 標準的なモジュール分割 |
| `common/*.rs` | 複数モジュールで共用するヘルパー（3 箇所以上の重複を集約した実績あり） |

---

## 8. コードリーディングの出発点

| やりたいこと | 読むべきファイル |
|---|---|
| `ato run` の全体フローを追う | `router.rs` → `engine.rs` → `runner.rs` |
| capsule.toml の型を確認する | `types/manifest.rs`, `manifest.rs` |
| lockfile の構造を理解する | `lockfile.rs` (冒頭の `CapsuleLock` 型定義) |
| エラーコードを追加する | `error.rs` (CapsuleError) + `execution_plan/error.rs` (AtoError) |
| 新しいランタイムを追加する | `executors/`, `lockfile.rs` (ensure_runtime_if_missing! マクロ), `lock_runtime.rs` |
| パッキング処理を追う | `packers/capsule.rs` → `packers/payload.rs` |
| 署名まわりを調べる | `signing/sign.rs`, `signing/verify.rs`, `types/signing.rs` |
| CAS の仕組みを理解する | `resource/cas/client.rs` → `resource/cas/index.rs` → `resource/cas/bloom.rs` |
