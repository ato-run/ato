---
title: "ADR-005: シークレット管理アーキテクチャ"
status: proposed
date: 2026-04
author: "@egamikohsuke"
related: ["ADR-006"]
---

# ADR-005: シークレット管理アーキテクチャ — グローバルストア + ACL + カプセル分離

## 1. コンテキスト

### 現状の問題

ato-cli は capsule ベースのアプリ配布ツールであり、実行時に環境変数（API キー等）をカプセルに注入する。現在の実装には以下の問題がある:

1. **平文保存**: `~/.ato/env/targets/<sha256>.env` にシークレットが平文で保存される
2. **カプセルごとに重複入力**: 同じ `OPENAI_API_KEY` を10カプセル分10回入力する必要がある
3. **アクセス制御なし**: 一度設定した値はどのカプセルからも参照可能 — 信頼できないカプセルに本番キーが漏洩するリスク
4. **チーム共有なし**: チームメンバー間でシークレットを安全に共有する手段がない
5. **アーカイブ漏洩リスク**: `ato encap` 時に `.env` ファイルが `.capsule` アーカイブに混入しうる

### 設計原則

```
capsule.toml = 開発者の意図（何が必要か）
ato secrets  = ユーザーの意図（実際の値とアクセスポリシー）
```

この分離は不可分である:

- **capsule.toml** にシークレットの値やスコープを書いてはならない
- **ato secrets** CLI にカプセルの要件定義を書いてはならない

## 2. 決定

### 2.1 アーキテクチャ概要

```
┌─────────────────────────────────────────────────────┐
│ capsule.toml（開発者が記述）                           │
│                                                      │
│  [env.required]                                      │
│  OPENAI_API_KEY = { description = "...", secret = true } │
│  DATABASE_URL = { description = "...", secret = true }   │
│  NODE_ENV = { default = "development" }              │
└──────────────────────┬──────────────────────────────┘
                       │ 「何が必要か」
                       ▼
┌─────────────────────────────────────────────────────┐
│ Secret Resolution Engine（ato run 時）               │
│                                                      │
│  1. Process env        ← 外部ツール (doppler/op run) │
│  2. --env-file         ← 明示ファイル                 │
│  3. Capsule-scoped     ← ato.secrets.capsule.<id>    │
│  4. Global secrets     ← ato.secrets.global          │
│  5. Team vault         ← ato.run remote (Phase 3)    │
│  6. default            ← capsule.toml (secret=false)  │
│  7. Interactive prompt ← 初回のみ → ストアに保存      │
│                                                      │
│  ※ 各段階で SecretPolicy (ACL) チェックを実施        │
└──────────────────────┬──────────────────────────────┘
                       │ 「値 + 許可判定」
                       ▼
┌─────────────────────────────────────────────────────┐
│ Executor（shell / source / deno / ...）              │
│                                                      │
│  cmd.env("OPENAI_API_KEY", resolved_value)           │
│  ※ ATO_UI_OVERRIDE_PORT 経由の既存フロー活用        │
└─────────────────────────────────────────────────────┘
```

### 2.2 グローバルシークレットストア

**値**: OS keychain に保存（`keyring` Rust crate, v2.3 — auth token 用に既に使用中。Cargo.toml で確認済み）。

```
OS Keychain:
  service: "ato.secrets.global"
    account: "OPENAI_API_KEY"       → "sk-xxx..."
    account: "ANTHROPIC_API_KEY"    → "sk-ant-..."

  service: "ato.secrets.capsule.publisher/myapp"
    account: "DATABASE_URL"         → "postgres://..."

  service: "ato.secrets.capsule.<sha256(canonical_path)>"
    account: "DATABASE_URL"         → "postgres://..."  (ローカルパス用)
```

**メタデータ** (ポリシー、利用履歴): `~/.ato/secrets/metadata.json` に保存。値を含まないので git-safe。

```json
{
  "secrets": {
    "OPENAI_API_KEY": {
      "policy": { "type": "allow_all" },
      "created_at": "2026-04-15T...",
      "last_used": "2026-04-15T...",
      "used_by": ["publisher/myapp", "publisher/demo"]
    },
    "STRIPE_LIVE_KEY": {
      "policy": {
        "type": "allow_list",
        "allow": ["publisher/billing-app"]
      },
      "created_at": "2026-04-10T..."
    }
  }
}
```

### 2.3 SecretPolicy（ACL モデル）

```rust
enum SecretPolicy {
    /// デフォルト: 全カプセルで利用可能
    AllowAll,

    /// 許可リスト: 指定カプセルのみ利用可能
    /// glob 対応: "publisher/*", "*/billing-*"
    AllowList { allow: Vec<String> },

    /// 拒否リスト: 指定カプセル以外は利用可能
    DenyList { deny: Vec<String> },
}
```

**判定ルール**:

| Policy      | Capsule in list | Result    |
| ----------- | --------------- | --------- |
| `AllowAll`  | —               | Allow     |
| `AllowList` | match           | Allow     |
| `AllowList` | no match        | **Block** |
| `DenyList`  | match           | **Block** |
| `DenyList`  | no match        | Allow     |

**Capsule-scoped secrets** は暗黙的に `AllowList { allow: ["<capsule_id>"] }` と同義。

**Glob マッチング**: `publisher/*` は publisher 配下の全カプセルに一致。`*` は全カプセルに一致（= `AllowAll` と等価だが明示的）。

### 2.4 capsule.toml スキーマ（開発者側）

```toml
schema_version = "1.1"

# 必要なシークレットと設定の宣言（値は書かない）
[env.required]
OPENAI_API_KEY = { description = "OpenAI API key for chat features", secret = true }
DATABASE_URL   = { description = "Postgres connection string", secret = true }
LOG_LEVEL      = { description = "Logging verbosity (debug/info/warn)", default = "info" }
SITE_NAME      = { description = "Display name for the site", default = "My App" }

# ホスト環境からの通過を許可する変数（サンドボックス境界）
[env.allowed]
HTTP_PROXY  = { description = "HTTP proxy for outbound requests" }
HTTPS_PROXY = { description = "HTTPS proxy" }
NO_PROXY    = { description = "Proxy bypass list" }

# 静的値（非シークレット、アーカイブに含まれる）
[targets.app.env]
NODE_ENV = "development"
PORT = "3000"

# レガシー互換（引き続きパース、内部で env.required に変換）
# required_env = ["OPENAI_API_KEY"]
# allow_env = ["HTTP_PROXY"]
```

**フィールド定義**:

| Field         | Type   | Default | Description                                         |
| ------------- | ------ | ------- | --------------------------------------------------- |
| `description` | string | —       | プロンプト時にユーザーに表示                        |
| `secret`      | bool   | `true`  | true: keychain保存、ログマスク / false: plaintext可 |
| `default`     | string | —       | 値未提供時のフォールバック（secret=false のみ有効） |

**注意**: `scope`, `allow`, `deny` などのアクセス制御フィールドは capsule.toml に含めない。これはユーザーの意図であり、開発者の意図ではない。

### 2.5 CLI UX（ユーザー側）

#### シークレット設定

```bash
# グローバル（全カプセルで利用可能）
ato secrets set OPENAI_API_KEY
# → Enter value for OPENAI_API_KEY: ****
# → Stored globally (available to all capsules)

# グローバル + 許可リスト付き
ato secrets set STRIPE_LIVE_KEY --allow publisher/billing-app
# → Stored globally (allowed: publisher/billing-app)

# グローバル + 許可 glob
ato secrets set COMPANY_API_KEY --allow "mypublisher/*"
# → Stored globally (allowed: mypublisher/*)

# グローバル + 拒否リスト
ato secrets set OPENAI_API_KEY --deny "untrusted/sketchy-app"
# → Stored globally (denied: untrusted/sketchy-app)

# カプセル専用（暗黙的に --allow <capsule> のみ）
ato secrets set DATABASE_URL --capsule publisher/myapp
# → Stored for capsule publisher/myapp only
```

#### シークレット確認・管理

```bash
# 一覧
ato secrets list
#  KEY                  SCOPE      POLICY              LAST USED
#  OPENAI_API_KEY       global     allow_all           2 hours ago
#  STRIPE_LIVE_KEY      global     allow [pub/billing] 3 days ago
#  DATABASE_URL         capsule    pub/myapp only      1 hour ago

# 詳細
ato secrets inspect OPENAI_API_KEY
#  Key:       OPENAI_API_KEY
#  Scope:     global
#  Policy:    allow_all
#  Created:   2026-04-15
#  Last used: 2026-04-15 (2 hours ago)
#  Used by:   publisher/myapp (47 times), publisher/demo (12 times)

# ACL 変更
ato secrets allow STRIPE_LIVE_KEY publisher/checkout-app
ato secrets deny  OPENAI_API_KEY  untrusted/sketchy-app

# 削除
ato secrets remove OPENAI_API_KEY
ato secrets remove DATABASE_URL --capsule publisher/myapp
```

#### `ato run` 時の初回プロンプト

```
$ ato run publisher/chat-app

Secret OPENAI_API_KEY is required but not configured.
  "OpenAI API key for chat features"

Enter value: sk-****

How should this secret be stored?
  [1] Global — available to all capsules (recommended)
  [2] Global — but only allow this capsule (publisher/chat-app)
  [3] This capsule only

> 1
Stored OPENAI_API_KEY globally.
```

#### ブロック時の UX

```
$ ato run untrusted/sketchy-app

✗ Secret STRIPE_LIVE_KEY is required but not authorized for this capsule.
  Policy: allow [publisher/billing-app]

  To authorize:  ato secrets allow STRIPE_LIVE_KEY untrusted/sketchy-app
  To use a different value:  ato secrets set STRIPE_LIVE_KEY --capsule untrusted/sketchy-app
```

### 2.6 解決順序（Resolution Chain）

```rust
fn resolve_secret(
    key: &str,
    capsule_id: &str,
    process_env: &HashMap<String, String>,
    env_file: Option<&Path>,
    store: &SecretStore,
    manifest_defaults: &HashMap<String, String>,
) -> Result<Option<String>> {
    // 1. Process env（外部ツールが設定 — 最優先）
    if let Some(val) = process_env.get(key) {
        return Ok(Some(val.clone()));
    }

    // 2. --env-file
    if let Some(val) = load_from_env_file(env_file, key)? {
        return Ok(Some(val));
    }

    // 3. Capsule-scoped keychain
    if let Some(val) = store.get_capsule_scoped(key, capsule_id)? {
        return Ok(Some(val));  // ACL は暗黙的に capsule_id のみ許可
    }

    // 4. Global keychain + ACL check
    if let Some(entry) = store.get_global(key)? {
        match entry.policy.check(capsule_id) {
            PolicyResult::Allow => return Ok(Some(entry.value)),
            PolicyResult::Deny => return Err(SecretBlockedError { key, capsule_id, policy: entry.policy }),
        }
    }

    // 5. Team vault (Phase 3)
    // if let Some(val) = team_vault.get(key, capsule_id).await? { ... }

    // 6. Default (secret=false のみ)
    if let Some(val) = manifest_defaults.get(key) {
        return Ok(Some(val.clone()));
    }

    // 7. Not found — caller should prompt interactively
    Ok(None)
}
```

### 2.7 アーカイブ安全性

#### `ato encap` のデフォルト除外

```rust
// pack_filter.rs — SMART_DEFAULT_EXCLUDES に追加
".env",
".env.*",
"**/.env",
"**/.env.*",
"*.pem",
"*.key",
"**/credentials.json",
"**/service-account*.json",
```

#### Env injection denylist（CVE-2026-4039 対策）

```rust
const DANGEROUS_ENV_VARS: &[&str] = &[
    "NODE_OPTIONS",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "PYTHONSTARTUP",
    "PERL5OPT",
    "RUBYOPT",
];
```

`required_env` / `allow_env` に上記変数が宣言されている場合は警告を表示し、明示的な `--allow-dangerous-env` フラグなしでは注入しない。

### 2.8 データモデル

```rust
// secrets/store.rs

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMetadata {
    pub secrets: HashMap<String, SecretEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntry {
    pub policy: SecretPolicy,
    pub scope: SecretScope,
    pub created_at: SystemTime,
    #[serde(default)]
    pub last_used: Option<SystemTime>,
    #[serde(default)]
    pub used_by: Vec<UsageRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub capsule_id: String,
    pub count: u32,
    pub last_used: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SecretPolicy {
    #[serde(rename = "allow_all")]
    AllowAll,
    #[serde(rename = "allow_list")]
    AllowList { allow: Vec<String> },
    #[serde(rename = "deny_list")]
    DenyList { deny: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SecretScope {
    Global,
    Capsule { capsule_id: String },
}
```

```rust
// secrets/keychain.rs
//
// NOTE: keyring crate v2.3 API (current Cargo.toml dependency).
// v2 uses keyring::Keyring::new(service, username) — not Entry::new().
// If upgrading to v3, the API changes to Entry::new(service, user).

pub struct KeychainBackend;

impl KeychainBackend {
    const GLOBAL_SERVICE: &'static str = "ato.secrets.global";

    pub fn get_global(key: &str) -> Result<Option<String>> {
        let kr = keyring::Keyring::new(Self::GLOBAL_SERVICE, key);
        match kr.get_password() {
            Ok(val) => Ok(Some(val)),
            Err(keyring::KeyringError::NoPasswordFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_global(key: &str, value: &str) -> Result<()> {
        let kr = keyring::Keyring::new(Self::GLOBAL_SERVICE, key);
        kr.set_password(value)?;
        Ok(())
    }

    pub fn get_capsule_scoped(key: &str, capsule_id: &str) -> Result<Option<String>> {
        let service = format!("ato.secrets.capsule.{}", capsule_id);
        let kr = keyring::Keyring::new(&service, key);
        match kr.get_password() {
            Ok(val) => Ok(Some(val)),
            Err(keyring::KeyringError::NoPasswordFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_capsule_scoped(key: &str, value: &str, capsule_id: &str) -> Result<()> {
        let service = format!("ato.secrets.capsule.{}", capsule_id);
        let kr = keyring::Keyring::new(&service, key);
        kr.set_password(value)?;
        Ok(())
    }
}
```

## 3. 却下した代替案

### 3A. 外部ツール委譲のみ（doppler run / op run）

```bash
doppler run -- ato run myapp
```

**却下理由**: ato-cli 独自の DX を提供できない。ユーザーに外部ツールのセットアップを強制する。`allow_env` による passthrough は escape hatch として残すが、primary path にはしない。

### 3B. 暗号化ファイル方式（SOPS / dotenvx）

```
.env.capsule.encrypted   # git にコミット可
```

**却下理由**: 鍵配布問題をエンドユーザーに押し付ける。DX として OS keychain より劣る。air-gapped チーム向けの optional export として Phase 3+ で検討。

### 3C. capsule.toml にスコープ/ACL を記述

```toml
# 却下: 開発者がユーザーのポリシーを決めるべきではない
OPENAI_API_KEY = { secret = true, scope = "global" }
```

**却下理由**: capsule.toml は開発者の意図を記述する場所。ユーザーの意図（どのカプセルに許可するか）は CLI (`ato secrets`) で管理すべき。

### 3D. カプセルごとの独立ストア（現行方式の暗号化版）

**却下理由**: 同じキーを N 個のカプセル分 N 回設定する問題が解消されない。グローバルストア + symlink 的参照が必要。

## 4. 実装ロードマップ

### Phase 1: 安全性の基盤（1 day）

| Task                                       | File                | Impact   |
| ------------------------------------------ | ------------------- | -------- |
| `.env*` を `SMART_DEFAULT_EXCLUDES` に追加 | `pack_filter.rs`    | Critical |
| Dangerous env var denylist                 | env injection layer | Critical |

### Phase 2: グローバル keychain ストア（3-4 days）

| Task                              | Detail                                                   |
| --------------------------------- | -------------------------------------------------------- |
| `SecretStore` モジュール          | keychain CRUD, metadata.json 管理                        |
| `ato secrets set/get/list/remove` | CLI subcommands                                          |
| `ato run` 時の解決チェーン        | process env → capsule-scoped → global → default → prompt |
| 既存 plaintext env の移行         | `~/.ato/env/targets/*.env` → keychain + ファイル削除     |
| Secret masking in logs            | `[REDACTED]` 表示                                        |

### Phase 3: ACL / ポリシー（2-3 days）

| Task                                     | Detail                                        |
| ---------------------------------------- | --------------------------------------------- |
| `SecretPolicy` 実装                      | AllowAll, AllowList, DenyList + glob matching |
| `--allow` / `--deny` / `--capsule` flags | CLI options                                   |
| `ato run` 時の policy check              | Block + actionable error message              |
| `ato secrets allow/deny`                 | ACL 変更コマンド                              |

### Phase 4: UX 強化（2-3 days）

| Task                                 | Detail                                         |
| ------------------------------------ | ---------------------------------------------- |
| Interactive prompt with scope choice | 「Global / Allow this capsule / Capsule only」 |
| `ato secrets inspect`                | 利用履歴、ポリシー詳細                         |
| `ato encap --dry-run` + secret scan  | アーカイブ前の安全チェック                     |
| Headless/CI mode (`--no-prompt`)     | Missing → error                                |

### Phase 5: capsule.toml v1.1 スキーマ（2-3 days）

| Task                                     | Detail                             |
| ---------------------------------------- | ---------------------------------- |
| `[env.required]` table parser            | `secret`, `default`, `description` |
| `[env.allowed]` table parser             | passthrough 宣言                   |
| Legacy `required_env` / `allow_env` 互換 | 内部変換                           |
| `ato init` で新形式生成                  | テンプレート更新                   |

### Phase 6: Backup / Export / Rotation（1-2 days, Phase 4 以降）

| Task | Detail |
|------|--------|
| `ato secrets export --encrypted` | age 暗号化バックアップファイル出力 |
| `ato secrets import --encrypted` | 暗号化バックアップからの復元 |
| `--expires-after` flag | シークレットに有効期限を設定。期限切れ時に `ato run` で警告 |
| `ato secrets rotate <KEY>` | 新しい値を入力、旧値を即座に上書き |

### Phase 7: Team Vault（4-6 weeks, 後日）

| Task                         | Detail                  |
| ---------------------------- | ----------------------- |
| ato.run server API           | CRUD + auth + audit log |
| `ato secrets push/pull/sync` | CLI integration         |
| Team management              | grant/revoke access     |
| capsule_id 署名検証           | サーバーサイドで capsule_id の信頼性を暗号的に保証 |

## 5. セキュリティモデル

### 保存時の暗号化

| 対象                    | 保存先                         | 暗号化                                                                                                           |
| ----------------------- | ------------------------------ | ---------------------------------------------------------------------------------------------------------------- |
| Secret values           | OS keychain                    | macOS: Keychain Services (AES-256) / Linux: Secret Service (GNOME Keyring) / Windows: Credential Manager (DPAPI) |
| Metadata (policy, 履歴) | `~/.ato/secrets/metadata.json` | なし（値を含まない）                                                                                             |
| Headless fallback       | `~/.ato/secrets/fallback.env`  | chmod 600 の平文ファイル（下記 5.2 参照）                                                                        |

### 5.2 ヘッドレス/CI フォールバック

OS keychain が利用できない環境（SSH セッション、Docker コンテナ、ヘッドレス CI）では、以下の 2 段階フォールバックを適用する:

**Option A（デフォルト・推奨）**: `~/.ato/secrets/fallback.env` に chmod 600 で保存。mise/direnv と同等のセキュリティモデル。正直にファイルパーミッションに依存する。

**Option B（強化モード、`--encrypted-fallback` flag）**: `~/.ato/secrets/vault.enc` に AES-256-GCM で保存。鍵導出は `blake3(ATO_TOKEN + canonical_path)` — ato.run の認証トークンをマスターキーの一部として使用。ATO_TOKEN が未設定の場合は Option A にフォールバック。

**判断**: Phase 2 では Option A のみ実装。Option B は Phase 4+ で必要に応じて追加。理由: machine UUID ベースの鍵導出はセキュリティシアター（同一マシンの攻撃者には無意味）であり、ファイルパーミッションの方が誠実。

### アクセス制御

| 脅威                                     | 対策                                          |
| ---------------------------------------- | --------------------------------------------- |
| 信頼できないカプセルへのシークレット漏洩 | SecretPolicy (ACL) による capsule_id チェック |
| `.capsule` アーカイブへの混入            | `.env*` デフォルト除外 + secret scan          |
| 危険な環境変数の注入 (RCE)               | `DANGEROUS_ENV_VARS` denylist                 |
| ログ出力への平文露出                     | `secret = true` の値は `[REDACTED]` 表示      |
| CI/CD での非対話実行                     | `--no-prompt` + process env passthrough       |
| process env による ACL バイパス          | 意図的な設計（下記 5.3 参照）                 |
| capsule_id の偽装                        | Phase 1-2 のスコープ外（下記 5.4 参照）       |

### 5.3 Process env は ACL をバイパスする（意図的設計）

Resolution Chain の Step 1 で process env（`export SECRET=... && ato run ...`）は ACL チェックなしで注入される。これは **意図的な設計**:

- `doppler run -- ato run untrusted/app` のようなユースケースで、外部ツールが設定した値を ato-cli が拒否すべきではない
- ユーザーが明示的に env var を設定している = ユーザーの意図的な許可
- Infisical、Doppler も同じ設計（process env は常に最優先、ACL 不問）

**リスク**: ユーザーがグローバルに `export STRIPE_LIVE_KEY=...` を設定していると、全カプセルに漏洩する。
**対策**: `ato secrets inspect` の出力に「process env で ACL がバイパスされた」旨を表示。ドキュメントで注意喚起。

### 5.4 capsule_id の信頼境界

Phase 1-2 では capsule_id は以下のソースから取得:
- **ato.run 経由**: `ATO_UI_SCOPED_ID` env var（registry が設定、`publisher/slug` 形式）
- **ローカル**: `capsule.toml` の `manifest_path` の canonical path

**脅威**: 悪意あるカプセルが `capsule.toml` に `publisher = "trusted-publisher"` を自己申告し、ACL を突破する。

**Phase 1-2 のスコープ**: ato-cli はローカルファイルシステムへのアクセスを持つ攻撃者を脅威モデルに含めない。ローカル実行では capsule_id はファイルパスベースで一意であり、偽装リスクは低い。

**Phase 3+ (Team Vault)**: ato.run が capsule_id を署名付きで発行し、サーバーサイドで検証する。これにより capsule_id の信頼性が暗号的に保証される。

### 5.5 metadata.json の並行書き込み

複数の `ato run` が同時実行されると `metadata.json` の last-write-wins 競合が発生する。

**Phase 2**: `fs2::lock_exclusive()` によるファイルロック（port_map.json と同じパターン、既に実装済み）。

**Phase 4+ (必要に応じて)**: SQLite に移行。`rusqlite` crate でアトミックな read-modify-write。metadata + port_map + 利用履歴を統合。

### 監査

- `metadata.json` の `used_by` フィールドでローカル利用履歴を追跡
- `ato secrets inspect` で誰（どのカプセル）がいつアクセスしたか確認可能
- Process env で ACL がバイパスされた場合はログに記録
- Phase 6 (Team Vault) でサーバーサイド監査ログを追加

## 6. 移行計画

### 既存ユーザーへの影響

1. `~/.ato/env/targets/*.env` の既存平文ファイルは Phase 2 で自動移行
   - 初回 `ato run` 時に検出 → keychain に移行 → 平文ファイル削除
   - 移行前にバックアップ通知
2. `capsule.toml` の `required_env = [...]` / `allow_env = [...]` は引き続きサポート
   - 内部で `[env.required]` / `[env.allowed]` に変換
   - 新規プロジェクトには v1.1 形式を推奨
3. Breaking changes: なし（全て additive）

## 7. 参考文献

- [17ツール調査レポート](../research_orchestrated_env_secrets_20260415/REPORT.md)
- aws-vault: permanent creds in keychain, temporary in subprocess (closest architectural match)
- 1Password CLI: `op://` reference syntax
- Infisical: fetch-merge-spawn pattern, reserved var filtering
- dotenvx: ECIES encryption for .env files
- SOPS + age: DEK/KEK, multi-recipient, value-only encryption
- devenv SecretSpec: declare-what-you-need, provider-agnostic resolution
- GitHub Codespaces: recommended secrets prompt, three-tier hierarchy
- OpenClaw CVE-2026-4039: env injection RCE via NODE_OPTIONS/LD_PRELOAD
