---
title: "Nacelle Terminal Spec (v0.1)"
status: accepted
date: "2026-04-16"
author: "@egamikohsuke"
ssot:
  - "apps/nacelle/"
related:
  - "NACELLE_SPEC.md"
  - "DESKTOP_SPEC.md"
---

# Nacelle Terminal Spec — Interactive PTY Execution for ato-desktop

## 1. 概要

nacelle に Interactive PTY モードを追加し、ato-desktop の WebView 内で対話的シェルセッションを実行可能にする。
`ato share` 等の CLI ツールを GUI から実行するユースケースを主対象とする。

### 1.1 設計意図

ato のランタイムエンジンは nacelle に一元化されている（Smart Build, Dumb Runtime）。
Tier 2 の Python/Node 実行も nacelle 経由でサンドボックスされる以上、ターミナル実行も nacelle を経由すべきである。
外部サンドボックスcrate（nono 等）を直接採用するのではなく、nacelle の既存アーキテクチャを拡張し、
不足する機能（PTY Seatbelt 許可、Landlock pre_exec 適用等）を nono の実装を参考に取り込む。

**理由:**
- サンドボックスポリシーの一元管理（capsule.toml → IsolationPolicy → SandboxPolicy）
- 機密パス自動除外（`~/.ssh`, `~/.aws` 等）の既存ロジックを再利用
- IPC socket path 許可の既存基盤をターミナルセッションにも適用
- ato エコシステム全体でセキュリティモデルを統一

### 1.2 スコープ

- **スコープ内:**
  - nacelle への PTY 実行モード追加（`interactive: true`）
  - macOS Seatbelt プロファイルへの PTY デバイス許可追加
  - Linux bwrap への PTY デバイスバインド追加
  - NDJSON protocol のターミナルデータストリーム拡張
  - ato-desktop の Terminal PaneSurface + xterm.js WebView
  - セキュリティ多層防御（環境変数フィルタ、出力サニタイズ、セッション管理）
- **非スコープ:**
  - nono crate の直接依存（参考実装としてのみ利用）
  - 汎用ターミナルエミュレータ（ato の capsule 実行コンテキスト限定）
  - GPUI native ターミナル描画（WebView + xterm.js を採用）

---

## 2. アーキテクチャ

### 2.1 全体データフロー

```
ato-desktop (GPUI + Wry)
│
├─ xterm.js v5 (WebView, Canvas renderer)
│    ↕ base64 JSON over Wry IPC
│    ↕ evaluate_script() / ipc_handler()
│
├─ Terminal Session Manager (Rust, ato-desktop 側)
│    ↕ ato-cli subprocess (stdin/stdout JSON)
│
├─ ato-cli (session orchestrator)
│    ↕ nacelle subprocess (stdin: JSON commands / stdout: NDJSON events)
│
└─ nacelle (interactive: true)
     │
     ├─ portable-pty: master fd ←→ child process (PTY slave)
     │    ↕ raw terminal I/O
     │
     └─ Sandbox Enforcer:
          macOS: sandbox_init(flags=0) ← dynamic SBPL ← /bin/zsh
                 (Phase 13a / ADR-007; sandbox-exec CLI is not used)
          Linux: bwrap --dev-bind /dev/pts ... /bin/zsh
```

### 2.2 既存アーキテクチャとの関係

nacelle の既存責務「Sandbox Enforcer」は変わらない。
PTY モードは Source Runtime の新しい実行パスであり、既存の stdout/stderr パイプモードと並立する。

```
nacelle internal exec
  ├─ interactive: false (既存) → Stdio::piped() → log_path
  └─ interactive: true  (新規) → portable-pty → NDJSON terminal_data events
```

### 2.3 コンポーネント責務

| コンポーネント | 責務 | 変更 |
|---|---|---|
| **ato-desktop** | xterm.js 描画、Wry IPC、セッションUI | 新規: PaneSurface::Terminal, xterm.js WebView |
| **ato-cli** | セッション管理、nacelle 起動、stdin/stdout 中継 | 拡張: interactive session type, stdin forwarding |
| **nacelle** | PTY 確保、サンドボックス適用、I/O ストリーミング | 拡張: portable-pty, PTY sandbox rules, NDJSON terminal events |
| **子プロセス** | 実際のシェル/コマンド実行 | 変更なし（PTY slave に接続されるのみ） |

---

## 3. nacelle 変更仕様

### 3.1 ExecEnvelope 拡張

既存の `interactive` フィールド（現在常に `false`）を活用する。

```json
{
  "spec_version": "1.0",
  "interactive": true,
  "terminal": {
    "cols": 80,
    "rows": 24,
    "shell": "/bin/zsh",
    "env_filter": "safe"
  },
  "workload": {
    "type": "source",
    "manifest": "/path/to/capsule.toml"
  },
  "env": [["KEY", "VALUE"]],
  "ipc_socket_paths": ["/tmp/capsule-ipc/service.sock"]
}
```

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `interactive` | bool | `false` | `true` で PTY モード起動 |
| `terminal.cols` | u16 | 80 | 初期カラム数 |
| `terminal.rows` | u16 | 24 | 初期行数 |
| `terminal.shell` | string? | `$SHELL` | シェルパス（allowlist で検証） |
| `terminal.env_filter` | string | `"safe"` | 環境変数フィルタモード（§5.2） |

### 3.2 Initial Response 拡張

```json
{
  "ok": true,
  "spec_version": "1.0",
  "pid": 12345,
  "log_path": "/tmp/nacelle-run.log",
  "terminal": {
    "session_id": "term-a1b2c3",
    "cols": 80,
    "rows": 24,
    "shell": "/bin/zsh"
  }
}
```

### 3.3 NDJSON Event 拡張

**nacelle → ato-cli（stdout、出力ストリーム）:**

```json
{"event":"terminal_data","session_id":"term-a1b2c3","data_b64":"SGVsbG8gV29ybGQK"}
{"event":"terminal_exited","session_id":"term-a1b2c3","exit_code":0}
```

**ato-cli → nacelle（stdin、入力ストリーム）:**

```json
{"type":"terminal_input","session_id":"term-a1b2c3","data_b64":"bHMgLWxhCg=="}
{"type":"terminal_resize","session_id":"term-a1b2c3","cols":120,"rows":40}
{"type":"terminal_signal","session_id":"term-a1b2c3","signal":"SIGINT"}
```

### 3.4 PTY 実行フロー

```
nacelle internal exec (interactive: true)
  │
  ├─ 1. Parse ExecEnvelope, validate terminal config
  ├─ 2. Validate shell against allowlist (§5.1)
  ├─ 3. Build SandboxPolicy with PTY rules (§4)
  ├─ 4. Generate platform sandbox profile
  │     ├─ macOS: .sb file with PTY device allows (§4.1)
  │     └─ Linux: bwrap args with /dev/pts bind (§4.2)
  ├─ 5. Filter environment variables (§5.2)
  ├─ 6. Allocate PTY via portable-pty
  │     └─ PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
  ├─ 7. Spawn child process in sandbox with PTY slave
  ├─ 8. Emit initial response (JSON, single line)
  ├─ 9. Start I/O bridge:
  │     ├─ Thread A: PTY master read → batch (16ms) → sanitize → base64 → NDJSON stdout
  │     └─ Thread B: stdin JSON parse → validate → PTY master write
  └─ 10. Wait for child exit → emit terminal_exited event
```

### 3.5 Cargo.toml 追加依存

```toml
[dependencies]
portable-pty = "0.9"
base64 = "0.22"

# 既存
tokio = { version = "1", features = ["full"] }
serde_json = "1.0"
```

---

## 4. サンドボックス拡張

### 4.1 macOS Seatbelt Profile — PTY デバイス許可

nacelle の `launcher/source/macos.rs` が生成する `.sb` プロファイルに以下を追加する。
これは nono (`crates/nono/src/sandbox/macos.rs`) の PTY 許可実装を参考にしている。

```scheme
;; === PTY Device Access (参考: nono macos.rs) ===
(allow pseudo-tty)
(allow file-ioctl
    (literal "/dev/tty")
    (regex #"^/dev/ttys[0-9]+$")
    (regex #"^/dev/pty[a-z][0-9a-f]+$"))
(allow file-read* file-write*
    (literal "/dev/ptmx")
    (regex #"^/dev/pts/")
    (literal "/dev/tty")
    (regex #"^/dev/ttys[0-9]+$"))
```

**条件:** `interactive: true` の場合のみ追加。非対話モードでは PTY 許可を付与しない（最小権限）。

**既存プロファイルとの統合:**

```
generate_seatbelt_profile(target, isolation_policy)
  ├─ (version 1)
  ├─ (allow default)              // 既存
  ├─ (deny sensitive paths)       // 既存
  ├─ (allow IPC socket paths)     // 既存
  ├─ (allow PTY devices)          // 新規: interactive 時のみ
  ├─ (deny network*)              // 既存: ポリシーに応じて
  └─ Write to {state_dir}/{workload_id}.sb
```

### 4.2 Linux Bubblewrap — PTY デバイスバインド

`launcher/source/linux.rs` の bwrap コマンド構築に追加:

```rust
// interactive: true の場合のみ
if target.interactive {
    cmd.arg("--dev-bind").arg("/dev/pts").arg("/dev/pts");
    cmd.arg("--dev-bind").arg("/dev/ptmx").arg("/dev/ptmx");
}
```

**既存との統合:**
- `--dev /dev` は既に存在するが、`/dev/pts` は明示バインドが必要（bwrap の `--dev` は最小限の devtmpfs のみ）
- `--new-session` は PTY モードでも維持（セッション隔離）

### 4.3 Linux Landlock — pre_exec 適用（nono 参考）

nacelle の `generate_landlock_policy()` は存在するが未適用。これを実際に pre_exec フックで適用する。
nono の `restrict_self()` パターンを参考にする。

```rust
// launcher/source/linux.rs — bwrap child の pre_exec 内
unsafe {
    cmd.pre_exec(move || {
        // 1. PR_SET_NO_NEW_PRIVS (nono と同様)
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);

        // 2. Landlock ルールセット適用
        if let Some(ref policy) = landlock_policy {
            apply_landlock_sandbox(policy)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        }
        Ok(())
    });
}
```

---

## 5. セキュリティモデル（多層防御）

### 5.1 シェル Allowlist

nacelle が起動を許可するシェルの明示リスト。`terminal.shell` はこのリストで検証される。

```rust
const ALLOWED_SHELLS: &[&str] = &[
    "/bin/bash",
    "/bin/zsh",
    "/bin/sh",
    "/usr/bin/bash",
    "/usr/bin/zsh",
    "/usr/local/bin/bash",
    "/usr/local/bin/zsh",
    "/usr/local/bin/fish",
    "/opt/homebrew/bin/fish",
    "/opt/homebrew/bin/bash",
    "/opt/homebrew/bin/zsh",
];
```

リスト外のパスは `exit code 10`（policy violation）で拒否する。

### 5.2 環境変数フィルタリング

`terminal.env_filter` で制御。nacelle 側で子プロセスに渡す環境変数をフィルタする。

| モード | 動作 |
|---|---|
| `"safe"` (デフォルト) | Blocklist パターンに一致する変数を除去 |
| `"minimal"` | Allowlist に一致する変数のみ透過 |
| `"passthrough"` | フィルタなし（開発モード限定） |

**Blocklist パターン（`safe` モード）:**

```rust
const SECRET_PATTERNS: &[&str] = &[
    "API_KEY", "API_SECRET", "SECRET_KEY", "ACCESS_KEY",
    "AUTH_TOKEN", "PRIVATE_KEY", "PASSWORD", "CREDENTIAL",
    "AWS_SECRET", "AWS_SESSION_TOKEN",
    "GITHUB_TOKEN", "GH_TOKEN", "GITLAB_TOKEN",
    "NPM_TOKEN", "DOCKER_PASSWORD",
    "DATABASE_URL",       // 接続文字列にパスワードを含むことが多い
    "OPENAI_API_KEY", "ANTHROPIC_API_KEY",
];

const DANGEROUS_VARS: &[&str] = &[
    "LD_PRELOAD", "DYLD_INSERT_LIBRARIES",
    "BASH_ENV", "ENV", "CDPATH",
    "PROMPT_COMMAND",     // コマンド注入ベクタ
];
```

**Allowlist（`minimal` モード）:**

```rust
const ALWAYS_ALLOW: &[&str] = &[
    "PATH", "HOME", "USER", "SHELL", "TERM", "LANG", "LC_ALL",
    "EDITOR", "VISUAL", "PAGER", "TMPDIR", "XDG_RUNTIME_DIR",
    "COLORTERM", "TERM_PROGRAM",
    "CAPSULE_IPC_SERVICE_URL",  // ato IPC は透過
];
```

### 5.3 ターミナル出力サニタイズ

nacelle の PTY 読み取りスレッドで、xterm.js に送信する前に危険なエスケープシーケンスを除去する。

| シーケンス | リスク | 対処 |
|---|---|---|
| OSC 52 (clipboard set) | ユーザーのクリップボードを上書き | 除去 |
| OSC 777 (custom private) | アプリ固有コマンド注入 | 除去 |
| DCS (Device Control String) | キーストローク注入（CVE-2019-0542） | 全除去 |
| OSC 0/1/2 (title set) | タイトル偽装でソーシャルエンジニアリング | 許可（低リスク） |
| OSC 8 (hyperlink) | javascript: URI | http/https のみ許可 |
| 標準 SGR/CSI | 色、カーソル移動等 | 許可（xterm.js 描画に必要） |

```rust
/// PTY 出力サニタイザ（nacelle 側で適用）
fn sanitize_pty_output(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x1b && i + 1 < data.len() {
            match data[i + 1] {
                b'P' => { i = skip_to_st(data, i); continue; }  // DCS: 全除去
                b']' => {                                         // OSC: 選択除去
                    let end = find_osc_end(data, i);
                    let osc = &data[i..end];
                    if is_safe_osc(osc) {
                        output.extend_from_slice(osc);
                    }
                    i = end;
                    continue;
                }
                _ => {} // CSI, SGR 等: 通過
            }
        }
        output.push(data[i]);
        i += 1;
    }
    output
}
```

### 5.4 セッション管理

| 制約 | 値 | 理由 |
|---|---|---|
| 最大同時セッション数 | 4 | リソース枯渇防止 |
| アイドルタイムアウト | 30 分 | 放置セッションのクリーンアップ |
| 最大セッション時間 | 4 時間 | 長時間占有防止 |
| 入力レート制限 | 1000 msg/sec | DoS 防止 |
| 出力バッチ間隔 | 16ms (60fps) | WebView 描画最適化 |
| 出力バッファ上限 | 1 MB | メモリ枯渇防止 |

### 5.5 セキュリティ境界図

```
┌─────────────────────────────────────────────────────────────┐
│ Layer 1: WKWebView Process Sandbox                          │
│  WebContent プロセスは別プロセス。JS は file/network 直接不可  │
│  window.ipc.postMessage() が Rust への唯一の通信路           │
├─────────────────────────────────────────────────────────────┤
│ Layer 2: ato-desktop Wry IPC Bridge                         │
│  - GuestBridgeRequest typed enum (既存)                     │
│  - CapabilityGrant::Terminal (新規)                         │
│  - capability_allowed() fail-closed チェック (既存)          │
│  - base64 encode/decode (raw bytes を JS に渡さない)         │
├─────────────────────────────────────────────────────────────┤
│ Layer 3: nacelle Terminal Session                            │
│  - Shell allowlist (§5.1)                                    │
│  - 環境変数フィルタ (§5.2)                                   │
│  - 出力サニタイズ (§5.3)                                     │
│  - セッション管理 (§5.4)                                     │
│  - cwd 制限 (capsule.toml isolation.filesystem に準拠)      │
├─────────────────────────────────────────────────────────────┤
│ Layer 4: OS Sandbox (nacelle 既存基盤)                       │
│  macOS: sandbox_init(flags=0) + 動的 SBPL (Phase 13a)       │
│  Linux: bwrap namespace + Landlock pre_exec (実適用)        │
│  機密パス自動除外 (~/.ssh, ~/.aws 等) — 既存ロジック         │
└─────────────────────────────────────────────────────────────┘
```

---

## 6. ato-desktop 変更仕様

### 6.1 PaneSurface 拡張

`state/mod.rs` の `PaneSurface` enum にターミナル variant を追加:

```rust
pub enum PaneSurface {
    Web(WebPane),
    Native { body: String },
    CapsuleStatus(CapsuleStatusPane),
    Terminal(TerminalPane),  // 新規
    Inspector,
    DevConsole,
    Launcher,
    AuthHandoff { /* ... */ },
}

pub struct TerminalPane {
    pub session_id: String,
    pub capsule_handle: String,
    pub cols: u16,
    pub rows: u16,
}
```

### 6.2 xterm.js WebView

ターミナル用の WebView を既存の WebViewManager で管理する。
capsule WebView と同じ Wry インフラを再利用し、カスタムプロトコルでアセットを配信する。

```
terminal://{session_id}/index.html   → xterm.js + Canvas renderer 初期化
terminal://{session_id}/xterm.js     → @xterm/xterm v5 UMD bundle
terminal://{session_id}/xterm.css    → xterm.js スタイルシート
terminal://{session_id}/addon-canvas.js → Canvas renderer addon
```

**WKWebView 注意点:**
- User Agent に "Safari" を含める（`builder.with_user_agent()`）— xterm.js の WKWebView 検出バグ回避（xtermjs#3575）
- Canvas renderer を使用（WebGL は Safari/WKWebView で不安定）
- `allowTransparency: false`

### 6.3 Wry IPC Bridge 拡張

`bridge.rs` の `GuestBridgeRequest` に Terminal variant を追加:

```rust
pub enum GuestBridgeRequest {
    // ... 既存 variants ...
    TerminalInput { session_id: String, data_b64: String },
    TerminalResize { session_id: String, cols: u16, rows: u16 },
}
```

`CapabilityGrant` に `Terminal` を追加:

```rust
pub enum CapabilityGrant {
    // ... 既存 ...
    Terminal,  // ターミナル WebView にのみ付与
}
```

### 6.4 出力パス（nacelle → xterm.js）

```
nacelle stdout: {"event":"terminal_data","session_id":"...","data_b64":"..."}
  ↓ ato-cli: NDJSON parse → mpsc channel
  ↓ ato-desktop: receive event → find terminal WebView
  ↓ webview.evaluate_script():
    window.__ATO_TERMINAL_RECV__({
      type: "terminal-output",
      sessionId: "...",
      data_b64: "..."   // serde_json::to_string() で安全にエスケープ
    })
  ↓ JS: base64 decode → Uint8Array → terminal.write(bytes)
```

**重要:** `evaluate_script()` に渡す文字列は必ず `serde_json::to_string()` でシリアライズする。
raw bytes の `format!()` 補間は JS injection に直結するため禁止。

### 6.5 入力パス（xterm.js → nacelle）

```
xterm.js: terminal.onData(data)
  ↓ JS: window.ipc.postMessage(JSON.stringify({
           type: "terminal-input",
           sessionId: "...",
           data_b64: btoa(data)
         }))
  ↓ ato-desktop ipc_handler: parse → validate (§5.4 レート制限)
  ↓ ato-cli stdin: {"type":"terminal_input","session_id":"...","data_b64":"..."}
  ↓ nacelle stdin reader: parse → base64 decode → PTY master write
```

---

## 7. macOS 配布とパーミッション

### 7.1 App Sandbox は使用不可

PTY 子プロセスは親の App Sandbox を継承する。サンドボックス内のシェルは
ファイルシステムにアクセスできず実用的に機能しない。全ターミナルアプリ
（Terminal.app, iTerm2, Warp, Wezterm）が同じ理由で App Sandbox を不採用。

**ato-desktop の配布方式:** Hardened Runtime + notarization（Mac App Store 外）

### 7.2 必要な Entitlements

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <!-- Hardened Runtime: JIT (WKWebView の JS エンジンに必要) -->
    <key>com.apple.security.cs.allow-jit</key>
    <true/>
    <!-- Hardened Runtime: unsigned executable memory (WKWebView) -->
    <key>com.apple.security.cs.allow-unsigned-executable-memory</key>
    <true/>
</dict>
</plist>
```

### 7.3 TCC (Transparency, Consent, and Control)

ユーザーに **Full Disk Access** の付与を案内する必要がある。
PTY 子プロセスのファイルアクセスは ato-desktop のバンドル ID に帰属するため、
TCC は ato-desktop に対してアクセス許可を要求する。

---

## 8. 実装フェーズ

### Phase 1: nacelle PTY 基盤（工数: 中）

| タスク | ファイル | 内容 |
|---|---|---|
| ExecEnvelope 拡張 | `cli/commands/internal.rs` | `terminal` フィールド追加、バリデーション |
| portable-pty 統合 | `launcher/source/mod.rs` (新規) | PTY allocator、master/slave 管理 |
| macOS .sb PTY rules | `launcher/source/macos.rs` | §4.1 の Seatbelt ルール追加 |
| Linux bwrap PTY bind | `launcher/source/linux.rs` | §4.2 の `--dev-bind` 追加 |
| Shell allowlist | `system/sandbox/mod.rs` | §5.1 のバリデーション |
| 環境変数フィルタ | `system/sandbox/mod.rs` | §5.2 の実装 |

### Phase 2: NDJSON ストリーミング（工数: 小）

| タスク | ファイル | 内容 |
|---|---|---|
| terminal_data event | `internal_api.rs` | NacelleEvent 拡張 |
| PTY → stdout bridge | `launcher/source/mod.rs` | 16ms バッチ読み取り + base64 + NDJSON |
| stdin → PTY bridge | `cli/commands/internal.rs` | stdin JSON parse + PTY write |
| 出力サニタイズ | `system/sandbox/mod.rs` (新規) | §5.3 のエスケープシーケンスフィルタ |
| ato-cli 中継 | `adapters/runtime/executors/source.rs` | stdin forwarding + terminal event routing |

### Phase 3: ato-desktop UI（工数: 中）

| タスク | ファイル | 内容 |
|---|---|---|
| PaneSurface::Terminal | `state/mod.rs` | §6.1 の enum variant 追加 |
| xterm.js WebView | `webview.rs` | §6.2 のカスタムプロトコル + アセット配信 |
| Terminal IPC bridge | `bridge.rs` | §6.3 の TerminalInput/Resize handling |
| Session manager | 新規 `terminal.rs` | §5.4 のセッション管理（上限、タイムアウト） |
| xterm.js HTML/JS | `assets/terminal/` | xterm.js 初期化、IPC 接続、resize handler |

### Phase 4: 防御強化（工数: 小）

| タスク | ファイル | 内容 |
|---|---|---|
| Landlock pre_exec 適用 | `launcher/source/linux.rs` | §4.3 の実装 |
| CSP ヘッダ追加 | `webview.rs` | terminal:// プロトコルレスポンスに CSP |
| Paste 確認ダイアログ | `assets/terminal/` | 複数行ペースト時の確認 UI |
| 監査ログ | `bridge.rs` | ターミナルセッション lifecycle のログ |

---

## 9. nono からの取り込み一覧

以下の機能は nono (`always-further/nono` v0.36) の実装を参考にして nacelle に取り込む。
nacelle は nono を crate 依存として追加しない。コードパターンの参考利用のみ。

| 機能 | nono ソース | nacelle 適用先 | 備考 |
|---|---|---|---|
| PTY Seatbelt 許可 | `crates/nono/src/sandbox/macos.rs` | `launcher/source/macos.rs` | `(allow pseudo-tty)` + `/dev/tty*` ioctl |
| Landlock restrict_self | `crates/nono/src/sandbox/linux.rs` | `launcher/source/linux.rs` | ABI V1-V6 プローブ + pre_exec |
| PR_SET_NO_NEW_PRIVS | Linux sandbox 共通 | `launcher/source/linux.rs` | 特権昇格防止 |
| Symlink 解決 | macOS Seatbelt profile | `launcher/source/macos.rs` | `/tmp` → `/private/tmp` 二重ルール |
| Keychain IPC 拒否 | macOS Seatbelt profile | `launcher/source/macos.rs` | 将来: 高セキュリティモード用 |

**取り込まないもの:**
- Extension Token メカニズム（ato のユースケースでは不要）
- seccomp-notify（Landlock で十分）
- PTY プロキシ / detach-attach（ato-desktop が UI を管理）
- JSON プロファイル形式（capsule.toml を維持）

---

## 10. 既知のリスクと緩和策

| リスク | 重大度 | 緩和策 |
|---|---|---|
| nono Issue #450: macOS PTY stdin ブロック | High | 動的 SBPL プロファイルで検証、問題発生時は dev mode fallback |
| evaluate_script() のスループット制限 | Medium | 16ms バッチ + 1MB バッファ上限で実用十分（インタラクティブシェル） |
| `sandbox_init` (Seatbelt) は deprecated API | Low | macOS 15 で動作確認済み。Apple が代替 API を提供するまで使用継続（ADR-007 で 6 ヶ月毎レビュー） |
| WKWebView UA 検出バグ (xtermjs#3575) | Low | `builder.with_user_agent()` で "Safari" を含む UA を設定 |
| Full Disk Access 未付与時の UX | Medium | 初回起動時にガイダンス表示、権限不足時のエラーメッセージ改善 |

---

## 11. 関連ドキュメント

- [NACELLE_SPEC.md](NACELLE_SPEC.md) — nacelle エンジン仕様
- [DESKTOP_SPEC.md](DESKTOP_SPEC.md) — ato-desktop 仕様
- [ADR-005: Secrets Management](../../specs/ADR-005-secrets-management-architecture.md) — シークレット管理
- [CAPSULE_SPEC.md](CAPSULE_SPEC.md) — Capsule 仕様（capsule.toml）
- [CAPSULE_IPC_SPEC.md](CAPSULE_IPC_SPEC.md) — IPC 仕様

### 外部参考資料

- [nono (always-further/nono)](https://github.com/always-further/nono) — Seatbelt/Landlock PTY 許可の参考実装
- [portable-pty](https://crates.io/crates/portable-pty) — Rust PTY crate (wezterm 由来)
- [@xterm/xterm v5](https://www.npmjs.com/package/@xterm/xterm) — WebView 内ターミナル描画
- [xterm.js WKWebView issue #3575](https://github.com/xtermjs/xterm.js/issues/3575) — UA 検出バグ
- [xterm.js security guide](https://xtermjs.org/docs/guides/security/) — セキュリティモデル
