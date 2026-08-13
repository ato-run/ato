---
title: "RFC: Capsule Core Specification"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot:
  - "apps/ato-cli/core/src/types/manifest.rs"
  - "apps/ato-cli/core/src/"
  - "apps/uarc/"
related: []
---

# RFC: Capsule Core Specification

## Overview

A **Capsule** is a self-describing, portable execution unit defined by a `capsule.toml` manifest. It encapsulates identity, runtime routing metadata, network policy, sandbox constraints, and signing provenance into a single coherent artifact. This document is derived entirely from source code. Every claim cites `file:line`.

## 1. Capsule Identity

### 1.1 Name

`name` は kebab-case の一意な識別子。

```toml
name = "my-app"
```

> `types/manifest.rs:400` — `pub name: String` (kebab-case, unique identifier)

### 1.2 Version

オプション。空文字はバージョン未設定を意味する。

```toml
version = "0.1.0"
```

> `types/manifest.rs:403-404` — `#[serde(default, skip_serializing_if = "String::is_empty")] pub version: String`

### 1.3 Capsule Type

```toml
type = "app"   # デフォルト
```

| 値          | 意味                                  |
|-------------|---------------------------------------|
| `app`       | デフォルト。汎用アプリケーション      |
| `inference` | AI/ML 推論モデル                      |
| `tool`      | 一発実行ツール                        |
| `job`       | バッチ/スケジュールジョブ             |
| `library`   | 他 Capsule が依存するライブラリ       |

> `types/manifest.rs:33-47` — `CapsuleType` enum。`#[default]` は `App`。

### 1.4 Default Target

```toml
default_target = "cli"
```

`[targets]` セクション内の既定ターゲットラベルを指定する。空の場合、エンジンが解決する。

> `types/manifest.rs:411-412` — `pub default_target: String`

---

## 2. Manifest Format

### 2.1 Schema Versions

`capsule.toml` の先頭で宣言する。

```toml
schema_version = "0.3"
```

サポートされるバージョン: `"0.3"` のみ。

> `types/manifest.rs:568-570` — `fn is_supported_schema_version(value: &str) -> bool { matches!(value.trim(), "0.3") }`

### 2.2 CapsuleManifest 構造

```toml
schema_version = "0.3"
name           = "my-app"
version        = "1.0.0"
type           = "app"
default_target = "main"

[metadata]
display_name = "My App"
description  = "..."
author       = "acme"
icon         = "https://..."
tags         = ["ai", "tool"]

[network]
egress_allow    = ["api.example.com"]
egress_id_allow = []

[targets.main]
runtime    = "source"
driver     = "python"
entrypoint = "src/main.py"
port       = 8080

[routing]
weight           = "light"
fallback_to_cloud = true

[transparency]
level            = "loose"
allowed_binaries = []

[isolation]
allow_env = ["HF_TOKEN"]
```

> `types/manifest.rs:393-511` — `CapsuleManifest` struct の全フィールド定義。

---

## 3. Runtime Types

### 3.1 現行 RuntimeType

```
Source  — ソースコード直接実行 (Python, Node, Deno 等)
Wasm    — WebAssembly (wasmtime)
Oci     — OCI/Docker コンテナ
Web     — 静的 Web 配信
```

> `types/manifest.rs:57-95` — `RuntimeType` enum。`#[default]` は `Source`。

### 3.2 非推奨 RuntimeType (legacy)

以下は自動的に正規形に変換される。

| 旧値     | 変換後   |
|----------|----------|
| `docker` | `oci`    |
| `youki`  | `oci`    |
| `native` | `source` |

> `types/manifest.rs:97-107` — `RuntimeType::normalize()`
> `types/manifest.rs:57-95` — `#[deprecated]` アノテーション付き

### 3.3 ExecutionDriver

ランタイム内でプロセスを起動するドライバ。

| 値         | 意味                                       |
|------------|--------------------------------------------|
| `static`   | 静的ファイル配信 (`web` ランタイム向け)    |
| `deno`     | Deno ランタイム                            |
| `node`     | Node.js                                    |
| `python`   | CPython                                    |
| `wasmtime` | Wasmtime                                   |
| `native`   | ネイティブバイナリ直接実行                 |

> `execution_plan/model.rs:37-71` — `ExecutionDriver` enum。
> `execution_plan/model.rs:26-35` — `ExecutionRuntime::from_manifest()` で `docker`/`youki`/`runc` はすべて `Oci` に正規化される。

---

## 4. Named Targets

### 4.1 基本構造

`[targets.<label>]` で複数のターゲットを定義できる。エンジンが最適なターゲットを選択する。

```toml
[targets.cli]
runtime    = "source"
driver     = "python"
entrypoint = "src/main.py"

[targets.wasm]
runtime    = "wasm"
driver     = "wasmtime"
entrypoint = "dist/app.wasm"
```

> `types/manifest.rs:491-492` — `pub targets: Option<TargetsConfig>`

### 4.2 TargetsConfig

```toml
[targets]
port         = 8080
health_check = "/healthz"
```

`[targets]` 直下のフィールドはデフォルト値として全ターゲットに継承される。

> `types/manifest.rs:1011-1058` — `TargetsConfig` struct、`port`/`health_check` フィールド

---

## 5. サービス (Supervisor Mode)

マルチプロセス構成を宣言する。`services` が存在する場合、単一プロセス `execution` は使用されない。

```toml
[services.api]
entrypoint = "src/api.py"
target     = "main"
depends_on = ["db"]
expose     = ["PORT"]

[services.db]
entrypoint = "postgres"
```

| フィールド       | 説明                                     |
|------------------|------------------------------------------|
| `entrypoint`     | 実行コマンド (`command` は alias)       |
| `target`         | 参照する `[targets.<label>]`            |
| `depends_on`     | 起動順序の依存関係                       |
| `expose`         | 動的ポートプレースホルダー              |
| `env`            | サービスへの環境変数注入                 |
| `state_bindings` | State の Bind                            |
| `readiness_probe`| HTTP/TCP ヘルスチェック                  |
| `network`        | サービス間ネットワーク制御 (aliases, publish, allow_from) |

> `types/manifest.rs:318-355` — `ServiceSpec` struct
> `types/manifest.rs:363-376` — `ServiceNetworkSpec` struct

---

## 6. ネットワーク/Egress 制御

### 6.1 L7 ドメイン許可リスト

```toml
[network]
egress_allow = ["api.openai.com", "api.example.com"]
```

> `types/manifest.rs:970-979` — `NetworkConfig.egress_allow: Vec<String>`

### 6.2 L3 IP/CIDR 許可リスト

```toml
[network]
[[network.egress_id_allow]]
type  = "cidr"
value = "10.0.0.0/8"
```

`type` は `ip`、`cidr`、`spiffe` (将来)。

> `types/manifest.rs:982-1000` — `EgressIdRule`、`EgressIdType`

---

## 7. サンドボックス/分離

### 7.1 透明性強制 (TransparencyLevel)

```toml
[transparency]
level            = "loose"   # デフォルト
allowed_binaries = ["lib/**/*.so"]
```

| レベル   | 意味                                                     |
|----------|----------------------------------------------------------|
| `strict` | ソースコード必須。バイナリ禁止 (allowlist 除く)          |
| `loose`  | .pyc/.class は許容。それ以外は allowlist が必要          |
| `off`    | 透明性強制なし (Docker 互換レガシー)                     |

> `types/manifest.rs:169-181` — `TransparencyLevel` enum。`#[default]` は `Loose`。
> `types/manifest.rs:187-198` — `TransparencyConfig` struct

### 7.2 環境変数パススルー (IsolationConfig)

```toml
[isolation]
allow_env = ["LD_LIBRARY_PATH", "CUDA_HOME", "HF_TOKEN"]
```

デフォルトはホスト環境変数をすべて遮断。明示 opt-in 方式。

> `types/manifest.rs:306-313` — `IsolationConfig` struct

---

## 8. 状態管理 (State)

```toml
[state.workspace]
kind       = "filesystem"
durability = "persistent"
purpose    = "user workspace files"
attach     = "auto"
```

| フィールド   | 型                                 | デフォルト |
|--------------|------------------------------------|------------|
| `kind`       | `filesystem`                       | —          |
| `durability` | `ephemeral` \| `persistent`        | —          |
| `purpose`    | String                             | —          |
| `producer`   | Option\<String\>                   | `None`     |
| `attach`     | `auto` \| `explicit`               | `auto`     |
| `schema_id`  | Option\<String\>                   | `None`     |

エフェメラルベースパス: 環境変数 `ATO_STATE_EPHEMERAL_BASE`、デフォルト `/var/lib/ato/state`。

> `types/manifest.rs:740-772` — `StateKind`、`StateDurability`、`StateRequirement`
> `types/manifest.rs:954-956` — `fn default_ephemeral_state_base()`

---

## 9. ルーティング

### 9.1 RouteWeight

```toml
[routing]
weight            = "light"   # デフォルト
fallback_to_cloud = true
cloud_capsule     = "acme/my-cloud-capsule"
```

| 値      | 意味                               |
|---------|------------------------------------|
| `light` | 軽量タスク。ローカル優先           |
| `heavy` | 重い計算。クラウドを検討           |

> `types/manifest.rs:134-143` — `RouteWeight` enum。`#[default]` は `Light`。

### 9.2 RuntimeDecision と routing フロー

```
route_manifest(manifest_path, profile, target_label)
  → route_manifest_with_validation_mode(...)
    → route_manifest_with_state_overrides_and_validation_mode(...)
      → RuntimeDecision { kind: RuntimeKind, reason: String, plan: ExecutionDescriptor }
```

`RuntimeKind` の解決:

ルーターは選択されたターゲットの `runtime` フィールドを直接読み取り、`parse_runtime_kind()` で変換する。自動的な優先順位ベースのフォールバックは行わない。

- `"oci"` / `"docker"` / `"youki"` / `"runc"` → `RuntimeKind::Oci`
- `"wasm"` → `RuntimeKind::Wasm`
- `"source"` / `"native"` → `RuntimeKind::Source`
- `"web"` → `RuntimeKind::Web`

ターゲット選択は `default_target` または明示的な `--target` フラグで決定される。

> `router.rs:18-24` — `RuntimeKind` enum
> `router.rs:261-266` — `RuntimeDecision` struct
> `router.rs:268-359` — `route_manifest()` とその内部実装
> `router.rs:1394-1402` — `parse_runtime_kind()` 変換マッピング

### 9.3 CompatManifestBridge

`capsule.toml` の raw TOML + パース済み `CapsuleManifest` + sha256 をまとめる移行サーフェス。

> `router.rs:32-136` — `CompatManifestBridge` struct

---

## 10. ato.lock (AtoLock v1)

### 10.1 概要

`ato.lock.json` はロックファイル。スキーマバージョンは `1` (整数)。

> `ato_lock/schema.rs:7` — `pub const ATO_LOCK_SCHEMA_VERSION: u32 = 1;`

### 10.2 AtoLock 構造

```json
{
  "schema_version": 1,
  "lock_id": "blake3:<64桁hex>",
  "generated_at": "2024-01-01T00:00:00Z",
  "features": { "declared": [], "required_for_execution": [] },
  "resolution": {},
  "contract": {},
  "binding": {},
  "policy": {},
  "attestations": {},
  "signatures": []
}
```

> `ato_lock/schema.rs:9-47` — `AtoLock` struct

### 10.3 LockId

形式: `blake3:<64文字小文字 hex>`

```
blake3:a3b2c1d4e5f6...
```

> `ato_lock/schema.rs:49-87` — `LockId` struct と `validate_format()`

### 10.4 KnownFeature

ロックが宣言できる既知フィーチャー。

| フィーチャー名          | 意味                                   |
|-------------------------|----------------------------------------|
| `read_only_root_fs`     | ルートファイルシステムを read-only に  |
| `identity`              | ID 機能を有効化                        |
| `reserved_env_prefixes` | 予約済み環境変数プレフィックスの保護  |
| `required_supervisor`   | スーパーバイザー必須                   |
| `enforced_network`      | ネットワークポリシーの強制             |

> `ato_lock/schema.rs:152-183` — `KnownFeature` enum

### 10.5 Sections

各セクション (`resolution`, `contract`, `binding`, `policy`, `attestations`) は以下の共通構造を持つ:

```typescript
{
  unresolved: UnresolvedValue[],   // 未解決フィールドの記録
  ...entries                       // flatten された動的キー
}
```

`UnresolvedReason` の値: `insufficient_evidence`、`ambiguity`、`deferred_host_local_binding`、`policy_gated_resolution`、`explicit_selection_required`。

> `ato_lock/schema.rs:185-343` — 各 Section 構造と `UnresolvedReason` enum

### 10.6 DeliveryEnvironment

`contract.delivery.install.environment` パスに配置される実行環境定義。

```json
{
  "strategy": "compose",
  "target": "main",
  "services": [
    { "name": "api", "from": "...", "lifecycle": "long-running" }
  ],
  "bootstrap": { "requires_personalization": false, "model_tiers": [] }
}
```

> `ato_lock/schema.rs:201-244` — `DeliveryEnvironment`、`DeliveryService`、`DeliveryBootstrap`、`DeliveryRepair`

### 10.7 lock_id の算出と検証

ロックファイルの正準化:

1. `normalize_lock_closure()` でクロージャ正規化
2. `recompute_lock_id()` で blake3 再計算
3. `validate_persisted_strict()` で構造検証
4. `serde_jcs::to_vec()` で JCS (JSON Canonicalization Scheme) シリアライズ

> `ato_lock/mod.rs:89-113` — `to_pretty_json()` と `write_canonical_to_vec()`

---

## 11. ExecutionPlan

`ExecutionPlan` はエンジンが生成する実行計画。スキーマバージョン `"1"`。

```json
{
  "schema_version": "1",
  "capsule": { "scoped_id": "...", "version": "..." },
  "target": {
    "label": "main",
    "runtime": "source",
    "driver": "python",
    "language": "python"
  },
  "provisioning": {
    "network": { "allow_registry_hosts": [] },
    "lock_required": true,
    "integrity_required": true,
    "allowed_registries": []
  },
  "runtime": {
    "policy": {
      "network": { "allow_hosts": [] },
      "filesystem": { "read_only": [], "read_write": [] },
      "secrets": { "allow_secret_ids": [], "delivery": "fd" },
      "args": []
    },
    "fail_closed": true,
    "non_interactive_behavior": "deny_if_unconsented"
  },
  "consent": {
    "key": { "scoped_id": "...", "version": "...", "target_label": "..." },
    "policy_segment_hash": "...",
    "provisioning_policy_hash": "...",
    "mount_set_algo_id": "lockfile_mountset_v1",
    "mount_set_algo_version": 1
  },
  "reproducibility": {
    "platform": { "os": "...", "arch": "...", "libc": "..." }
  }
}
```

> `execution_plan/model.rs:3` — `pub const EXECUTION_PLAN_SCHEMA_VERSION: &str = "1";`
> `execution_plan/model.rs:79-88` — `ExecutionPlan` struct (フィールド: `schema_version`, `capsule`, `target`, `provisioning`, `runtime`, `consent`, `reproducibility`)

### 11.1 MOUNT_SET_ALGO_ID

```
lockfile_mountset_v1  (バージョン: 1)
```

> `execution_plan/model.rs` — `MOUNT_SET_ALGO_ID = "lockfile_mountset_v1"`、`version = 1`

---

## 12. Pack / アーカイブ形式

### 12.1 `.capsule` ファイル構造

Capsule は PAX TAR アーカイブ。

```
my-app.capsule  (PAX TAR)
├── capsule.toml           # マニフェスト
├── capsule.lock.json      # ロックファイル
├── signature.json         # 署名メタデータ
└── payload.tar.zst        # zstd 圧縮 TAR
    ├── source/            # ソースコード
    └── config.json        # コントローラが生成する設定
```

> `packers/capsule.rs:30-39` — アーカイブ構造のコメントと実装

### 12.2 CapsulePackOptions

```rust
CapsulePackOptions {
    compat_input:    Option<CompatProjectInput>,
    workspace_root:  PathBuf,
    output:          Option<PathBuf>,
    config_json:     Arc<r3_config::ConfigJson>,
    config_path:     PathBuf,
    lockfile_path:   PathBuf,
}
```

> `packers/capsule.rs:41-49` — `CapsulePackOptions` struct

---

## 13. 署名 (Signing)

### 13.1 アルゴリズム

- Ed25519 署名 (`ed25519_dalek`)
- コンテンツは SHA-256 でハッシュ
- 秘密鍵: 32 バイトバイナリ または JSON (`StoredKey` 形式)
- 公開鍵: Base64 エンコード

> `signing/sign.rs` — Ed25519 署名実装 (`ed25519_dalek` + `sha2`)

### 13.2 CapsuleSignature

```json
{
  "algorithm":            "ed25519",
  "signature":            "<base64>",
  "content_hash":         "<sha256 hex>",
  "public_key":           "<base64>",
  "signer":               "acme-corp",
  "signed_at":            1700000000,
  "transparency_log_url": null
}
```

> `signing/sign.rs:14-30` — `CapsuleSignature` struct

### 13.3 署名対象

- **バンドル署名**: `capsule.toml` の内容を署名。出力: `bundle_path/.signature`
- **アーティファクト署名**: 任意ファイル (e.g. `.wasm`)。出力: `<artifact>.sig`

> `signing/sign.rs:41-97` — `sign_bundle()`  
> `signing/sign.rs:99-151` — `sign_artifact()`

### 13.4 鍵ファイル形式

JSON `StoredKey` 形式:

```json
{
  "key_type":   "ed25519",
  "public_key": "<base64>",
  "secret_key": "<base64>"
}
```

または raw 32 バイトバイナリ。鍵生成時のファイルパーミッション: `0o600`。

> `signing/sign.rs:200-225` — `read_key_bytes()`
> `signing/sign.rs:169-198` — `generate_keypair()`

---

## 14. Handle (Capsule アドレス)

### 14.1 CanonicalHandle

Capsule の参照アドレスには 3 種類ある。

```rust
enum CanonicalHandle {
    GithubRepo    { owner, repo },
    RegistryCapsule { registry, publisher, slug, version },
    LocalPath     { path },
}
```

> `handle.rs:73-87` — `CanonicalHandle` enum

### 14.2 入力サーフェス

```rust
enum InputSurface {
    CliRun,
    CliResolve,
    DesktopOmnibar,
    DeepLink,
}
```

> `handle.rs:11-18` — `InputSurface` enum

### 14.3 レジストリ識別

| フィールド          | 公式レジストリ                |
|---------------------|-------------------------------|
| `display_authority` | `ato.run`                     |
| `registry_identity` | `ato-official`                |
| `registry_endpoint` | `https://api.ato.run`         |

ループバック (開発用) の `registry_identity` は `ato-loopback:<host>` 形式。

> `handle.rs:7-69` — `RegistryIdentity` と定数

---

## 15. レガシーフォーマット

### 15.1 CHML (schema_version なし)

以下のいずれかのフィールドを含む、`schema_version` を持たないマニフェストは CHML として検出される:

- `packages`、`workspace`
- `build`/`run`/`runtime` が文字列値
- `outputs`、`build_env`、`required_env`
- `runtime_version`、`runtime_tools`、`readiness_probe`
- `external_injection`、`dependencies`、`capsule_path`

> `types/manifest.rs:579-604` — `is_chml_manifest()` 関数

### 15.2 Schema v0.3

`schema_version = "0.3"` を持ち、`entrypoint`/`cmd` を拒否して `run` を使用する移行フォーマット。

> `types/manifest.rs:572-577` — `is_v03_schema()`

---

## 16. ストレージボリューム

```toml
[[storage.volumes]]
name       = "data"
mount_path = "/data"
read_only  = false
size_bytes = 10737418240   # 10 GiB
encrypted  = false
```

> `types/manifest.rs:712-738` — `CapsuleStorage`、`StorageVolume` struct

---

## 17. 推論 Capsule (Inference Type)

`type = "inference"` のとき有効になる追加フィールド。

```toml
type = "inference"

[capabilities]
chat             = true
function_calling = true
vision           = false
context_length   = 131072

[model]
source       = "hf:org/model-name"
quantization = "4bit"

[requirements]
vram_min         = "6GB"
vram_recommended = "8GB"
disk             = "10GB"
platform         = ["darwin-arm64", "linux-amd64"]
```

`quantization` の値: `fp16`、`bf16`、`8bit`、`4bit`。

> `types/manifest.rs:798-878` — `CapsuleCapabilities`、`CapsuleRequirements`
> `types/manifest.rs:958-968` — `ModelConfig`
> `types/manifest.rs:145-154` — `Quantization` enum

---

## 18. PolymorphismConfig

他の Capsule スキーマを実装することを宣言する。

```toml
[polymorphism]
implements = ["schema:acme/chat-api@sha256:abc123"]
```

> `types/manifest.rs:555-562` — `PolymorphismConfig` struct

---

## 19. DistributionInfo

パック/パブリッシュ時にエンジンが付与するメタデータ。

```toml
[distribution]
manifest_hash = "<sha256 hex>"
merkle_root   = "<hash>"

[[distribution.chunk_list]]
chunk_hash  = "<hash>"
offset      = 0
length      = 1048576
codec       = "zstd"
compression = "zstd"

[[distribution.signatures]]
signer_did = "did:key:..."
key_id     = "..."
algorithm  = "ed25519"
signature  = "<base64>"
signed_at  = "2024-01-01T00:00:00Z"
```

> `types/manifest.rs:513-540` — `DistributionInfo`、`ChunkDescriptor`、`SignatureEntry`

---

## Summary: Key Invariants

| 不変条件                            | ソース                                      |
|-------------------------------------|---------------------------------------------|
| `schema_version` は `"0.3"` がデフォルト | `types/manifest.rs:564-566`            |
| `type` のデフォルトは `app`         | `types/manifest.rs:33-47` (`#[default]` = App) |
| `runtime` のデフォルトは `source`   | `types/manifest.rs:57-95` (`#[default]` = Source) |
| `weight` のデフォルトは `light`     | `types/manifest.rs:134-143`                 |
| `transparency.level` のデフォルトは `loose` | `types/manifest.rs:169-181`         |
| `attach` のデフォルトは `auto`      | `types/manifest.rs:753-759`                 |
| `ato.lock.json` の lock_id は `blake3:` prefix | `ato_lock/schema.rs:63-76`      |
| 署名アルゴリズムは Ed25519          | `signing/sign.rs:76`                        |
| アーカイブは PAX TAR + zstd payload | `packers/capsule.rs:30-39`                  |
