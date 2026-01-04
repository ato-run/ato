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

```
running 194 tests
...
test result: ok. 194 passed; 0 failed; 1 ignored
```

## 今後の作業

1. **gRPC GetJobStatus の統合**: `grpc_server.rs` に `GetJobStatus` RPC を追加
2. **CAS Client の統合**: `capsule_manager.rs` で L1 Policy を呼び出し
3. **SPIFFE 認証**: Engine 間通信に mTLS を導入
4. **Wasmtime バージョン固定**: `Cargo.toml` で `wasmtime = "=16.0"` に変更検討

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
│  │   • Runtime selection (Docker, PythonUV, Youki)      │  │
│  │   • Lifecycle management                              │  │
│  │   • Metrics collection                                │  │
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
