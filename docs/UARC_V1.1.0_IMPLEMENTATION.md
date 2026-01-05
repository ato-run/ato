# UARC V1.1.0 準拠 - capsuled 実装変更サマリー

## 概要

本ドキュメントは、UARC (Universal Application Runtime Contract) V1.1.0 仕様に基づいた capsuled エンジンの修正内容をまとめています。主な目的は、Coordinator 依存を除去し、スタンドアロンで動作する UARC 準拠エンジンを実現することです。

## 実装済み変更

### Step 1.1: Coordinator 依存の除去 ✅

**変更ファイル:**

- `src/metrics/collector.rs` (新規作成)
- `src/main.rs` (修正)
- `src/capsule_manager.rs` (修正)
- `src/api_server.rs` (修正)

**変更内容:**

- `UsageReporter` (push-based) を `MetricsCollector` (pull-based) に置換
- `COORDINATOR_URL` 環境変数を削除
- Prometheus 形式の `/metrics` エンドポイントを追加

**設計:**

```rust
pub struct MetricsCollector {
    // Prometheus registry for pull-based metrics
    registry: prometheus::Registry,
    active_sessions: DashMap<String, SessionMetrics>,
    // ...
}

impl MetricsCollector {
    pub fn start_tracking(&self, capsule_name: &str, session_id: &str);
    pub fn stop_tracking(&self, session_id: &str, exit_code: Option<i32>);
    pub fn gather_prometheus(&self) -> String; // Prometheus text format
}
```

### Step 1.2: CAS Client 抽象化 ✅

**変更ファイル:**

- `src/cas/mod.rs` (新規作成)
- `src/cas/client.rs` (新規作成)

**変更内容:**

- `CasClient` trait を定義（将来の IPFS/P2P 対応のため）
- `LocalCasClient`: ファイルシステムベースの CAS
- `HttpCasClient`: HTTP ベースのリモート CAS（ローカルキャッシュ付き）
- 環境変数による設定: `ATO_CAS_TYPE`, `ATO_CAS_ENDPOINT`, `ATO_CAS_ROOT`

**Trait 設計:**

```rust
#[async_trait]
pub trait CasClient: Send + Sync {
    async fn fetch_blob(&self, digest: &str) -> CasResult<PathBuf>;
    async fn store_blob(&self, content: &[u8]) -> CasResult<String>;
    async fn exists(&self, digest: &str) -> CasResult<bool>;
}
```

### Step 2: Cap'n Proto 正規化の有効化 ✅

**変更ファイル:**

- `src/capnp_to_manifest.rs` (完全書き換え)
- `src/security/verifier.rs` (修正)
- `src/lib.rs` (修正)

**変更内容:**

- `manifest_to_capnp_bytes()` 関数を capsuled 内に直接実装
- JSON フォールバックを削除し、Cap'n Proto 正規バイトを使用
- UARC V1.1.0 Normative Decision #2 に準拠

**署名検証:**

```rust
pub fn verify_manifest(
    &self,
    manifest: &CapsuleManifestV1,
    signature_bytes: &[u8],
    developer_key: &str,
) -> Result<()> {
    // Generate canonical Cap'n Proto bytes for verification (UARC V1.1.0)
    let canonical_bytes = manifest_to_capnp_bytes(manifest)
        .map_err(|e| anyhow!("Failed to generate canonical bytes: {:?}", e))?;

    self.verify_canonical_bytes(&canonical_bytes, signature_bytes, developer_key)
}
```

### Step 3: L1 Source Policy 実装 ✅

**変更ファイル:**

- `src/security/verifier.rs` (追加)

**変更内容:**

- `L1PolicyError` 列挙型の定義
- `verify_l1_source_policy()` 関数の実装
- 危険パターンの検出:
  - `base64 -d` / `base64 --decode`
  - `eval(` / `exec(`
  - `| sh` / `| bash` (curl/wget パイプ)
  - `__import__` / `importlib.import_module`
  - `subprocess.Popen` / `os.system(` / `os.popen(`

**使用例:**

```rust
// L1 Source Policy 検証
verify_l1_source_policy(source_path, &["py", "sh", "js"])?;
```

### Step 4: Job Status 永続化 ✅

**変更ファイル:**

- `src/job_history/mod.rs` (新規作成)
- `src/job_history/store.rs` (新規作成)

**変更内容:**

- SQLite ベースの Job History Store 実装
- `JobPhase` 列挙型: `Pending`, `Running`, `Succeeded`, `Failed`, `Cancelled`
- `JobRecord` 構造体: 完全なジョブ実行履歴
- `JobHistory` trait: 永続化バックエンドの抽象化

**テーブルスキーマ:**

```sql
CREATE TABLE job_history (
    job_id TEXT PRIMARY KEY,
    capsule_name TEXT NOT NULL,
    capsule_version TEXT NOT NULL,
    phase TEXT NOT NULL,
    error_message TEXT,
    exit_code INTEGER,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    duration_secs INTEGER,
    resource_usage_json TEXT
);
```

## テスト結果

**Phase 1 (Steps 1-4):**

```
running 194 tests
...
test result: ok. 194 passed; 0 failed; 1 ignored
```

**Phase 2 (Wasm Runtime Integration):**

```
running 196 tests
...
test result: ok. 196 passed; 0 failed; 1 ignored
```

### Phase 2: Wasm Runtime 統合 (進行中) 🔄

**変更ファイル:**

- `src/runtime/wasm.rs` (新規作成)
- `src/runtime/mod.rs` (修正 - RuntimeKind::Wasm 追加)
- `capsule-cli/capsule-core/src/capsule_v1.rs` (修正 - RuntimeType::Wasm 追加)
- `src/capsule_manager.rs` (修正 - wasm_runtime フィールド追加)
- `Cargo.toml` (修正 - wasmtime 依存追加)

**変更内容:**

- `WasmRuntime` 構造体の作成
- `RuntimeType::Wasm` 列挙型バリアントの追加
- CapsuleManager への統合
- Component Model 依存の設定

**設計:**

```rust
pub struct WasmRuntime {
    _artifact_manager: Option<Arc<ArtifactManager>>,
    log_dir: PathBuf,
    _egress_proxy_port: Option<u16>,
}

impl Runtime for WasmRuntime {
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError>;
    async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError>;
    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf>;
}
```

**依存:**

```toml
wasmtime = { version = "16.0", features = ["component-model", "async"] }
wasmtime-wasi = "16.0"
```

**実装計画 (次のステップ):**

1. ✅ Cargo.toml に wasmtime 依存を追加
2. ✅ WasmRuntime 構造体の作成
3. ✅ RuntimeType/RuntimeKind に Wasm バリアント追加
4. ✅ CapsuleManager への統合
5. ⏳ Component Model ローディングの実装
   - `Component::from_file()` with validation
   - WASI Context 設定 (log redirection)
   - Resource limits (512MB memory default)
   - Async execution (wasi:cli/command::run)
6. ✅ テストの追加
7. ✅ ドキュメントの完成

**機能要件:**

- **Component Model API**: 旧 Module API ではなく高レベル Component Model 使用
- **wasi:cli/command world**: 標準エントリーポイント `run` 関数のサポート
- **Resource Limiting**: ResourceLimiter trait 実装 (メモリ 512MB デフォルト)
- **Async Execution**: Config::async_support(true) による Tokio ブロッキング防止
- **Log Redirection**: stdout/stderr をファイルにリダイレクト (inherit_stdio 不使用)

### Phase 3: Runtime Resolution (マルチターゲット解決) ✅

**変更ファイル:**

- `uarc/schema/capsule.capnp` (修正 - wasm @4 追加)
- `capsule-cli/capsule-core/src/capsule_v1.rs` (修正 - targets フィールド追加)
- `src/capnp_to_manifest.rs` (修正 - Wasm マッピングバグ修正)
- `src/runtime/resolver.rs` (新規作成 - ~450 行)
- `src/capsule_manager.rs` (修正 - Resolver 統合)
- `tests/runtime_resolution_e2e.rs` (新規作成 - 12 テスト)

**変更内容:**

- Cap'n Proto `RuntimeType` に `wasm @4` を追加
- `CapsuleManifestV1` に `targets: Option<TargetsConfig>` を追加
- `TargetsConfig`, `WasmTarget`, `SourceTarget`, `OciTarget` 構造体を定義
- UARC V1.1.0 Resolution Algorithm 実装 (Filter → Constraint → Preference)
- レガシーフォールバック (targets 未定義時は execution.runtime を使用)

**Runtime Resolver 設計:**

```rust
pub enum ResolvedTarget {
    Wasm { digest: String, world: String, component_path: Option<PathBuf> },
    Source { language: String, version: Option<String>, entrypoint: String, ... },
    Oci { image: String, digest: Option<String>, cmd: Vec<String> },
    Legacy { runtime_type: RuntimeType, entrypoint: String },
}

pub struct ResolveContext {
    pub platform: String,                        // e.g., "darwin-arm64"
    pub supported_runtimes: HashSet<RuntimeKind>, // Engine capabilities
    pub wasm_available: bool,
    pub docker_available: bool,
    pub available_toolchains: HashSet<String>,   // e.g., {"python", "node"}
}

pub fn resolve_runtime(
    manifest: &CapsuleManifestV1,
    context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError>;
```

**capsule.toml の書き方:**

```toml
[targets]
preference = ["wasm", "oci"]  # 優先順位 (省略時: wasm → source → oci)

[targets.wasm]
digest = "sha256:abc123..."
world = "wasi:cli/run@0.2.0"

[targets.source]
language = "python"
version = "3.11"
entrypoint = "main.py"
dependencies = "requirements.txt"

[targets.oci]
image = "python:3.11-slim"
digest = "sha256:xyz789..."
cmd = ["python", "main.py"]
```

**E2E テストケース:**

- `test_legacy_fallback_when_no_targets`: targets 未定義 → Legacy
- `test_wasm_first_preference`: preference=["wasm", "oci"] → Wasm 選択
- `test_oci_first_preference`: preference=["oci", "wasm"] → OCI 選択
- `test_wasm_only_engine_selects_wasm`: OCI 優先だが Wasm のみ対応 → Wasm
- `test_docker_only_engine_selects_oci`: Wasm 優先だが Docker のみ → OCI
- `test_source_target_with_toolchain`: python toolchain あり → Source
- `test_source_target_without_toolchain_falls_back`: ruby なし → OCI fallback
- `test_no_compatible_target_error`: 対応ランタイムなし → エラー

### Phase 4: gRPC GetJobStatus/ListJobs ✅

**変更ファイル:**

- `uarc/proto/engine/v1/api.proto` (修正)
- `src/grpc_server.rs` (修正)
- `src/main.rs` (修正)

**Proto 定義:**

```protobuf
// 新規 RPC
rpc GetJobStatus(GetJobStatusRequest) returns (GetJobStatusResponse);
rpc ListJobs(ListJobsRequest) returns (ListJobsResponse);

enum JobPhase {
  JOB_PHASE_UNSPECIFIED = 0;
  JOB_PHASE_PENDING = 1;
  JOB_PHASE_RUNNING = 2;
  JOB_PHASE_SUCCEEDED = 3;
  JOB_PHASE_FAILED = 4;
  JOB_PHASE_CANCELLED = 5;
}

message GetJobStatusResponse {
  string job_id = 1;
  string capsule_name = 2;
  string capsule_version = 3;
  JobPhase phase = 4;
  string error_message = 5;
  int32 exit_code = 6;
  string created_at = 7;
  string started_at = 8;
  string finished_at = 9;
  uint64 duration_secs = 10;
  JobResourceUsage resource_usage = 11;
}

message JobResourceUsage {
  uint64 cpu_time_ms = 1;
  uint64 memory_peak_bytes = 2;
  uint64 vram_peak_bytes = 3;
}
```

**実装内容:**

1. **EngineService 拡張**: `job_history: Arc<SqliteJobHistoryStore>` フィールド追加
2. **get_job_status()**: JobHistory から job_id で検索し、proto 変換して返却
3. **list_jobs()**: capsule_name/phase フィルタでジョブ一覧取得
4. **Phase 変換ヘルパー**: `internal_phase_to_proto()`, `proto_phase_to_internal()`
5. **リソース使用量**: JSON から `JobResourceUsage` への変換

**使用例 (grpcurl):**

```bash
# ジョブステータス取得
grpcurl -plaintext -d '{"job_id": "abc-123"}' \
  localhost:50051 ato.engine.v1.Engine/GetJobStatus

# ジョブ一覧
grpcurl -plaintext -d '{"limit": 10, "capsule_name": "my-capsule"}' \
  localhost:50051 ato.engine.v1.Engine/ListJobs
```

**テスト (6 件追加):**

- `test_internal_phase_to_proto`
- `test_proto_phase_to_internal`
- `test_job_record_to_proto`
- `test_job_record_to_proto_without_resource_usage`
- `test_resource_usage_json_parsing`
- `test_job_history_integration`

### Phase 5: gRPC CancelJob (Control Plane 完結) ✅

**変更ファイル:**
- `uarc/proto/engine/v1/api.proto` (修正)
- `src/grpc_server.rs` (修正)

**Proto 定義:**
```protobuf
// 新規 RPC
rpc CancelJob(CancelJobRequest) returns (CancelJobResponse);

message CancelJobRequest {
  string job_id = 1;             // Job ID to cancel
  bool force = 2;                // If true, send SIGKILL instead of SIGTERM
}

message CancelJobResponse {
  bool success = 1;              // True if cancellation was initiated
  string message = 2;            // Human-readable status message
  JobPhase previous_phase = 3;   // Phase before cancellation attempt
}
```

**実装内容:**
1. **冪等性**: 既に終了済み (Succeeded/Failed/Cancelled) のジョブへの CancelJob は成功として扱う
2. **JobHistory 連携**: キャンセル成功時に `phase = Cancelled` へ遷移、error_message に理由記録
3. **CapsuleManager 連携**: `stop_capsule()` 経由でプロセス停止
4. **エラーハンドリング**: ジョブが見つからない場合は `NOT_FOUND`、停止失敗は適切なエラーメッセージ

**使用例 (grpcurl):**
```bash
# ジョブキャンセル
grpcurl -plaintext -d '{"job_id": "abc-123"}' \
  localhost:50051 ato.engine.v1.Engine/CancelJob

# 強制停止 (SIGKILL)
grpcurl -plaintext -d '{"job_id": "abc-123", "force": true}' \
  localhost:50051 ato.engine.v1.Engine/CancelJob
```

**テスト (2件追加):**
- `test_cancel_job_updates_phase`: Running → Cancelled 遷移確認
- `test_cancel_already_completed_job_is_idempotent`: 完了済みジョブへの冪等性確認

**ジョブ管理ライフサイクル完結:**
```
Deploy → GetJobStatus/ListJobs (監視) → CancelJob (停止)
   ↓              ↓                           ↓
 Pending → Running → Succeeded/Failed/Cancelled
```

## 今後の作業

1. ~~**gRPC GetJobStatus の統合**~~: ✅ 完了
2. ~~**gRPC CancelJob の追加**~~: ✅ 完了
3. **Source Runtime 実装**: Ephemeral Container による Python/Node.js 実行
4. **SPIFFE 認証**: Engine 間通信に mTLS を導入

## アーキテクチャ図

```
┌─────────────────────────────────────────────────────────────┐
│                      capsuled Engine                        │
├─────────────────────────────────────────────────────────────┤
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │  Manifest    │  │ CAS Client   │  │  Job History     │  │
│  │  Verifier    │  │ (Local/HTTP) │  │  (SQLite)        │  │
│  │  (L1/L2)     │  │              │  │                  │  │
│  └──────┬───────┘  └──────┬───────┘  └────────┬─────────┘  │
│         │                 │                    │            │
│         ▼                 ▼                    ▼            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │                   CapsuleManager                      │  │
│  │                         │                             │  │
│  │            ┌────────────▼────────────┐               │  │
│  │            │    Runtime Resolver     │               │  │
│  │            │  (UARC V1.1.0 Algorithm)│               │  │
│  │            └────────────┬────────────┘               │  │
│  │                         │                             │  │
│  │    ┌────────────────────┼────────────────────┐       │  │
│  │    ▼                    ▼                    ▼       │  │
│  │ ┌────────┐        ┌──────────┐        ┌──────────┐  │  │
│  │ │  Wasm  │        │  Docker  │        │  Youki   │  │  │
│  │ │Runtime │        │  Runtime │        │  Runtime │  │  │
│  │ └────────┘        └──────────┘        └──────────┘  │  │
│  └──────────────────────────────────────────────────────┘  │
│                            │                                │
│         ┌──────────────────┼──────────────────┐            │
│         ▼                  ▼                  ▼            │
│  ┌────────────┐    ┌────────────┐    ┌────────────────┐   │
│  │ gRPC Server│    │ HTTP API   │    │ /metrics       │   │
│  │ :50051     │    │ :8080      │    │ (Prometheus)   │   │
│  └────────────┘    └────────────┘    └────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

## 依存関係

```toml
[dependencies]
capnp = "0.24"
rusqlite = { version = "0.32", features = ["bundled"] }
prometheus = "0.13"
async-trait = "0.1"
thiserror = "1.0"
```

## 関連ドキュメント

- [UARC V1.1.0 Specification](../uarc/SPEC.md)
- [UARC README](../uarc/README.md)
- [Capsule Schema](../uarc/schema/capsule.capnp)
