---
title: "Secret Auto-Injection Spec"
status: draft
date: 2026-04-17
author: "@Koh0920"
ssot:
  - "apps/ato-desktop/src/config.rs"
  - "apps/ato-desktop/src/orchestrator.rs"
  - "apps/ato-desktop/src/bridge.rs"
  - "apps/ato-cli/src/app_control/session.rs"
related:
  - "docs/researches/devpulse_ato_ecosystem_20260415/"
---

# Secret Auto-Injection Spec

## 1. 概要

GUI で管理されたシークレットを、カプセル起動時に環境変数として自動注入する。
現在は SecretStore（config.rs）と設定UI（settings.rs）が実装済みだが、
実際のカプセルプロセスへの注入パスが未接続。このSpecでそのギャップを埋める。

背景: 2025年にGitHub上で2865万件のハードコードシークレット漏洩（前年比+34%）。
AIアシスタント経由の漏洩は通常の2倍。OS Keychain統合による安全な注入が差別化要素。

## 2. スコープ

### スコープ内

- Desktop SecretStore → カプセルプロセスへの環境変数注入
- ato-run REPL子プロセスへの環境変数注入
- Bridge GetSecrets レスポンスの完成（guest JSからのシークレット取得）
- MVP: JSON平文 → Phase 2で ato-cli secrets（OS Keychain）との統合

### スコープ外

- ato-cli secrets コマンドの変更
- シークレットのローテーション自動化
- チームVault / 共有シークレット
- .env ファイルのドラッグ&ドロップインポートUI

## 3. 設計

### 3.1 注入アーキテクチャ

```
┌─ Desktop Settings UI ─┐
│  SecretStore           │
│  ├ secrets: [{k,v}]   │
│  └ grants: {handle→[k]}│
└──────────┬─────────────┘
           │
           ▼
┌─ Injection Points ─────────────────────────────┐
│                                                 │
│  A) Capsule Session (ato-cli経由)               │
│     orchestrator::start_capsule()               │
│     → Command::new("ato").env("SECRET_*", val)  │
│     → ato-cli が launch.env_vars に転送         │
│     → capsule process に環境変数として注入      │
│                                                 │
│  B) ato-run REPL 子プロセス (PTY経由)           │
│     orchestrator::spawn_ato_run_repl()          │
│     → cmd_builder.env("SECRET_*", val)          │
│     → PTY子プロセスに直接注入                   │
│                                                 │
│  C) Guest JS Bridge (WebView経由)               │
│     bridge::GetSecrets → ShellEvent             │
│     → WebViewManager が AppState 参照           │
│     → evaluate_script で JSON レスポンス返却    │
│                                                 │
└─────────────────────────────────────────────────┘
```

### 3.2 注入ポイント A: Capsule Session

**現状のギャップ:**
```rust
// orchestrator.rs:348-377 — run_ato_json()
let output = Command::new(&ato_bin)
    .args(args)
    .output()  // ← env vars なし!
```

**修正:**
```rust
fn start_capsule_with_secrets(
    handle: &str,
    secrets: &[SecretEntry],
) -> Result<SessionStartInfo> {
    let mut cmd = Command::new(resolve_ato_binary()?);
    cmd.args(["app", "session", "start", handle, "--json"]);

    // 注入: ATO_SECRET_ プレフィックスで名前空間化
    for secret in secrets {
        cmd.env(format!("ATO_SECRET_{}", secret.key.to_uppercase()), &secret.value);
    }

    let output = cmd.output()?;
    // ... parse response
}
```

**呼び出し側の変更（webview.rs）:**
```rust
// resolve_and_start_guest() 内
let granted_secrets = state.secret_store.secrets_for_capsule(handle);
let session = start_capsule_with_secrets(handle, &granted_secrets)?;
```

### 3.3 注入ポイント B: ato-run REPL 子プロセス

**現状のギャップ:**
```rust
// orchestrator.rs:2598-2631 — spawn_ato_run_repl() 内の cmd_builder
cmd_builder.env("FORCE_COLOR", "1");
cmd_builder.env("TERM", "xterm-256color");
// ... HOME, USER, LANG等
// ← シークレット注入なし
```

**修正:**
`spawn_ato_run_repl` と `spawn_cli_session` にシークレットを渡す。
TerminalPane に capsule_handle が保存されているため、そこからグラント照合可能。

```rust
pub fn spawn_cli_session(
    session_id: String,
    cols: u16,
    rows: u16,
    spec: CliLaunchSpec,
    secrets: Vec<SecretEntry>,  // 追加
) -> Result<TerminalProcess> {
    // ...
}
```

子プロセス起動時（PTY内）:
```rust
for secret in &secrets {
    cmd_builder.env(
        format!("ATO_SECRET_{}", secret.key.to_uppercase()),
        &secret.value,
    );
}
```

### 3.4 注入ポイント C: Bridge GetSecrets

**現状:** `ShellEvent::GetSecrets` がpushされるが、レスポンスが `Null`。

**修正（webview.rs の apply_shell_events）:**

```rust
ShellEvent::GetSecrets { request_id, pane_id } => {
    if let Some(pane_id) = pane_id {
        // pane → capsule_handle を取得
        let handle = self.capsule_handle_for_pane(*pane_id);
        let secrets = state.secret_store.secrets_for_capsule(&handle);
        let payload = serde_json::json!(
            secrets.iter().map(|s| (&s.key, &s.value)).collect::<HashMap<_,_>>()
        );
        // WebView に evaluate_script でレスポンスを返す
        if let Some(view) = self.views.get_mut(pane_id) {
            let script = format!(
                "window.__ATO_HOST__.resolveSecrets({}, {});",
                request_id,
                serde_json::to_string(&payload).unwrap_or_default()
            );
            let _ = view.webview.evaluate_script(&script);
        }
    }
}
```

### 3.5 環境変数の命名規則

| 形式 | 例 | 説明 |
|------|---|------|
| `ATO_SECRET_{KEY}` | `ATO_SECRET_API_KEY` | Desktop注入のプレフィックス |
| key はUPPERCASE | `database_url` → `ATO_SECRET_DATABASE_URL` | 自動変換 |
| 元のキー名も設定 | `API_KEY=value` | `ATO_SECRET_` なし版も並行設定（オプション） |

### 3.6 セキュリティ制御

```
シークレット注入の条件:
1. SecretStore にシークレットが登録されている
2. grants に capsule_handle → [secret_key] マッピングがある
3. WebPane の capabilities に CapabilityGrant::Secrets が含まれている
4. (Bridge経由の場合) allowlist に "secrets" が含まれている

すべての条件がANDで成立した場合のみ注入。
```

## 4. インターフェース

### Desktop Settings UI（既存拡張）

```
Secrets セクション:
┌────────────────────────────────────────┐
│ Secrets                                │
│                                        │
│  API_KEY          ••••••••  (2 capsules)│
│  DATABASE_URL     ••••••••  (1 capsule) │
│                                        │
│  [+ Add Secret]                        │
│                                        │
│  Capsule Grants:                       │
│  ┌──────────────────────────────┐      │
│  │ koh0920/my-app               │      │
│  │  ☑ API_KEY  ☑ DATABASE_URL  │      │
│  └──────────────────────────────┘      │
└────────────────────────────────────────┘
```

### Guest JS API

```javascript
// guest preload (host_bridge.js に追加)
window.__ATO_HOST__.getSecrets().then(secrets => {
  // secrets = { API_KEY: "sk-...", DATABASE_URL: "postgres://..." }
});
```

### 環境変数（プロセス内）

```bash
# capsule process 内
echo $ATO_SECRET_API_KEY    # → sk-...
echo $ATO_SECRET_DATABASE_URL  # → postgres://...
```

## 5. セキュリティ

- シークレットはプロセスの環境変数としてのみ存在（ファイルに書き出さない）
- `ATO_SECRET_` プレフィックスでatoが注入した変数と区別
- Capability check は fail-closed（"secrets" が allowlist になければ拒否）
- MVP では `~/.ato/secrets.json` に平文保存（Phase 2 で OS Keychain 統合）
- nacelle の `env_filter: "safe"` はカスタム env を通過させるよう調整が必要

## 6. 既知の制限

- MVP は JSON 平文保存（ディスク暗号化に依存）
- ato-cli の `secrets` コマンド（OS Keychain）との統合は Phase 2
- nacelle 経由のターミナルセッションは `env_filter` の制約あり
- シークレットのローテーション通知なし

## 実装計画

### Phase 1: 環境変数注入パス（2-3日）

1. **orchestrator.rs** — `start_capsule` に secrets パラメータ追加
   - `run_ato_json()` で `Command::env()` にシークレット設定
   - `resolve_and_start_guest()` から SecretStore 参照して渡す

2. **orchestrator.rs** — `spawn_ato_run_repl` にシークレット注入
   - `spawn_cli_session()` に secrets パラメータ追加
   - `cmd_builder.env()` で子プロセスに注入

3. **webview.rs** — capsule起動時にシークレット取得・注入
   - `sync_from_state()` 内の capsule launch フローで secrets 参照

### Phase 2: Bridge GetSecrets 完成（1日）

4. **webview.rs** — `ShellEvent::GetSecrets` ハンドリング
   - pane_id → capsule_handle 解決
   - SecretStore から granted secrets 取得
   - evaluate_script でレスポンス返却

5. **assets/preload/host_bridge.js** — `getSecrets()` API追加

### Phase 3: UI 改善（1日）

6. **settings.rs** — シークレット追加フォーム
   - key/value 入力フィールド
   - Add ボタンで `AddSecret` アクション

7. **settings.rs** — カプセル別グラント管理UI
   - カプセルハンドル一覧 + シークレットチェックボックス

### Phase 4: OS Keychain 統合（将来）

8. ato-cli `secrets` との統合
9. `security-framework` crate でmacOS Keychain読み書き
10. JSON平文からの移行パス

## 参照

- `apps/ato-desktop/src/config.rs:102-211` — SecretStore 実装
- `apps/ato-desktop/src/orchestrator.rs:348-377` — run_ato_json（注入ポイントA）
- `apps/ato-desktop/src/orchestrator.rs:2598-2631` — REPL env設定（注入ポイントB）
- `apps/ato-desktop/src/bridge.rs:295-317` — GetSecrets handler（注入ポイントC）
- `apps/ato-cli/src/app_control/session.rs:210-212` — capsule env_vars設定
