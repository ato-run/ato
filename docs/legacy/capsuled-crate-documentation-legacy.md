# capsuled クレート ドキュメント

このファイルは `capsuled` クレートのドキュメント生成と閲覧方法をまとめたものです。

## ドキュメント生成

`cargo doc` コマンドでドキュメントを生成できます：

```bash
# ドキュメントを生成（依存関係のドキュメントは不含）
cargo doc --lib --no-deps

# 生成後、ブラウザで自動的に開く
cargo doc --lib --no-deps --open
```

## 生成されるドキュメント

生成されたドキュメントは以下の場所に保存されます：

```
target/doc/capsuled/index.html
```

## ドキュメント構成

### モジュール概要

- **`runtime`** - ランタイム実装（Wasm、Source、OCI）
  - ドキュメント：`target/doc/capsuled/runtime/index.html`
  - 説明：複数のランタイムバックエンドの選択・実行モデル、セキュリティ設計

- **`engine`** - コアエンジン（Capsule管理、実行制御）
  - ドキュメント：`target/doc/capsuled/engine/index.html`
  - 説明：Capsuleライフサイクル、Architecture、UARC準拠性

- **`interface`** - 外部インターフェース（gRPC、HTTP、Dev Server）
  - ドキュメント：`target/doc/capsuled/interface/index.html`
  - 説明：API使用パターン、Embedded/Standalone モード

- **`resource`** - リソース管理（CAS、Artifact、Storage）
  - ドキュメント：`target/doc/capsuled/resource/index.html`

- **`verification`** - セキュリティ層（署名検証、CAS整合性）
  - ドキュメント：`target/doc/capsuled/verification/index.html`

- **`workload`** - Manifest とランプラン管理
  - ドキュメント：`target/doc/capsuled/workload/index.html`

- **`common`** - 共有ユーティリティ（認証、設定）
- **`system`** - システムレベル（ハードウェア検出、ネットワーク）
- **`observability`** - ロギング・メトリクス・監査

### 主要トレイト

#### `runtime::Runtime`

すべてのランタイムが実装するコアインターフェース。以下を提供：

```rust
/// Launch a workload
async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError>

/// Stop a workload
async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError>

/// Get log path for a workload
fn get_log_path(&self, workload_id: &str) -> Option<PathBuf>
```

#### `engine::CapsuleManager`

Capsule ライフサイクル管理の中心。デプロイ、停止、クエリを実装。

## Embedded Usage（組み込みモード）

`capsuled` をライブラリとして使用する場合の一般的なパターン：

```rust
use capsuled::dev_server::{DevServerConfig, DevServerHandle};

// 1. 設定を作成
let config = DevServerConfig::default()
    .with_dev_mode(true)
    .with_allowed_paths(vec!["/tmp".to_string()]);

// 2. サーバーを起動
let handle = DevServerHandle::start(config).await?;

// 3. gRPC エンドポイントを取得
println!("Engine running at {}", handle.grpc_endpoint());

// 4. 使用...

// 5. シャットダウン
handle.shutdown().await;
```

## UARC V1.1.0 準拠

このドキュメントは UARC V1.1.0 準拠の実装を対象としており、以下を記載：

- Layer 3 (Runtime): Wasm/Source/OCI ランタイム
- Layer 4 (Engine): Capsule 管理、検証、リソース管理
- Layer 5 (Interface): gRPC/HTTP API、Discovery

## 参考

- [UARC Specification](../../uarc/SPEC.md)
- [README.md](../README.md) - プロジェクト概要
- [BUILD.md](BUILD.md) - ビルド手順
