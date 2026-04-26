---
title: "Desktop WebView Automation Spec"
status: draft
date: 2026-04-16
author: "@Koh0920"
ssot:
  - "apps/ato-desktop/src/automation/"
  - "apps/ato-desktop/assets/automation/agent.js"
related:
  - "NACELLE_TERMINAL_SPEC.md"
  - "DESKTOP_TAB_SPEC.md"
  - "MULTI_WEBVIEW_SPEC.md"
---

# Desktop WebView Automation Spec

## 1. 概要

AI コーディングエージェント (Claude Code, Cursor 等) が ato-desktop 上の capsule アプリに対して Playwright 相当の自動 E2E テストを実行するための自動化基盤。JS injection + MCP プロトコルで、capsule サンドボックス内のブラウザ操作をクロスプラットフォームで実現する。

## 2. スコープ

### スコープ内

- WebView 内の DOM snapshot (accessibility tree)、要素操作 (click/fill/type)、screenshot
- MCP server (`ato-desktop-mcp`) 経由での AI エージェント連携
- Playwright MCP 互換の tool 名・snapshot format
- capsule.toml `[capabilities] automation = true` による opt-in 制御
- クロスプラットフォーム (macOS WKWebView, Linux WebKitGTK, Windows WebView2)

### スコープ外

- 外部ブラウザの自動化 (Playwright/Puppeteer が担当)
- OS レベルの UI 自動化 (Computer Use が担当)
- Network layer の完全なインターセプト (CDP 依存機能)
- iframe のクロスオリジンアクセス

## 3. 設計

### 3.1 アーキテクチャ

```
AI Agent (Claude Code / Cursor)
  | MCP stdio
ato-desktop-mcp (Rust binary, 別プロセス)
  | Unix socket (~/.ato/run/ato-desktop-{pid}.sock)
ato-desktop Automation Host (src/automation/)
  | evaluate_script_with_callback
WebView (Wry) + Injected agent.js
  → DOM traversal, element actions, a11y snapshot
```

3 層構成:
- **Interface Layer**: MCP server binary — stdio JSON-RPC ↔ socket bridge
- **Domain Layer**: Automation command types, assertion logic, ref resolution
- **Infrastructure Layer**: Automation Host (Rust) + Injected Script Engine (JS)

### 3.2 Injected Automation Script (`agent.js`)

capsule.toml で `[capabilities] automation = true` を宣言した capsule にのみ inject ("Safe by default")。デバッグモードや `--automation-override` フラグで全 capsule に inject 可能。

**機能:**
1. **Accessibility Snapshot**: DOM 走査 → ARIA roles/labels/values → Playwright MCP 互換の structured tree (~200 tokens/page)
2. **Hash-Stable Element Reference**: `data-ato-ref` ID を tag+aria-label+nth-child のハッシュで生成。再 snapshot 時にも同一要素には同一 ref
3. **Action Execution**: click, fill, select, check, press_key を ref ベースで実行
4. **Wait/Observe**: MutationObserver + fetch/XMLHttpRequest interceptor で DOM 変更・network idle を検出

**Snapshot format (Playwright MCP 互換):**
```json
{
  "role": "WebArea", "name": "My App",
  "children": [
    { "role": "navigation", "name": "Main nav", "ref": "e3", "children": [
      { "role": "link", "name": "Home", "ref": "e4" }
    ]},
    { "role": "main", "ref": "e6", "children": [
      { "role": "textbox", "name": "Search", "ref": "e8", "value": "" },
      { "role": "button", "name": "Submit", "ref": "e9" }
    ]}
  ]
}
```

### 3.3 Automation Host (`src/automation/`)

Rust モジュール。Unix socket で外部からのコマンドを受付け、WebView に dispatch。

**Transport**: `trait AutomationTransport` で抽象化 — Unix socket (macOS/Linux), named pipe (Windows)。Socket path は `~/.ato/run/ato-desktop-{pid}.sock` (PID qualified で複数インスタンス対応)。

**PageLoadEvent Guard**: `evaluate_script_with_callback` を WKWebView の `did_commit_navigation` 完了前に呼ぶとコールバックがサイレントにドロップされる (Wry バグ)。各コマンド実行前に `PageLoadEvent::Finished` 受信済みを確認し、ナビゲーション中はキューイング。

### 3.4 Screenshot

JS ベースの html2canvas は使用しない (WKWebView で CORS エラー、CSS filter 非対応、500ms-2s 遅延)。

| Platform | API | 性能 |
|----------|-----|------|
| macOS | `WKWebView.takeSnapshot(with:completionHandler:)` via objc2 | <50ms |
| Linux | `webkit_web_view_get_snapshot()` via WebKitGTK | <50ms |
| Windows | WebView2 `CapturePreview()` | <50ms |

## 4. インターフェース

### 4.1 MCP Tool List (Playwright MCP 互換名)

| MCP Tool | 内部コマンド | 実装層 |
|----------|-------------|--------|
| `browser_snapshot` | snapshot | JS |
| `browser_take_screenshot` | screenshot | Rust native |
| `browser_click` | click(ref) | JS |
| `browser_fill` | fill(ref, value) | JS |
| `browser_type` | type(ref, text) | JS |
| `browser_select_option` | select_option(ref, value) | JS |
| `browser_check` / `browser_uncheck` | check/uncheck(ref) | JS |
| `browser_press_key` | press_key(key) | JS |
| `browser_navigate` | navigate(url) | Rust |
| `browser_navigate_back/forward` | evaluate("history.back/forward()") | JS |
| `browser_wait_for` | wait_for(selector, timeout) | JS |
| `browser_evaluate` | evaluate(js) | Rust |
| `browser_console_messages` | console_messages | JS |
| `browser_verify_text_visible` | snapshot + text search | JS + Domain |
| `browser_verify_element_visible` | snapshot + ref check | JS + Domain |
| `browser_tabs` / `browser_tab_focus` | list_panes / focus_pane | Rust |

### 4.2 Automation Host Protocol

JSON-RPC over Unix domain socket。Request/Response 例:

```json
// Request
{"jsonrpc": "2.0", "method": "click", "params": {"ref": "e9"}, "id": 1}

// Response
{"jsonrpc": "2.0", "result": {"ok": true}, "id": 1}
```

### 4.3 capsule.toml opt-in

```toml
[capabilities]
automation = true
```

## 5. セキュリティ

- **Opt-in 必須**: `[capabilities] automation = true` がない capsule には agent.js を inject しない
- **サンドボックス内完結**: 自動化は Wry の `evaluate_script` 経由で capsule の WebView 内でのみ動作。ホスト OS のファイルシステムやプロセスにはアクセスしない
- **Socket アクセス制御**: Unix socket のパーミッションは 0600 (owner のみ)
- **コマンドインジェクション**: ref ベースの操作はエスケープ済み。`browser_evaluate` のみ任意 JS を許可するが、capsule サンドボックス内に閉じる

## 6. 既知の制限

- **iframe クロスオリジン**: JS injection ではクロスオリジン iframe 内の DOM にアクセスできない
- **Network interception**: CDP がないため、HTTP request/response の書き換えは不可 (Phase 4 で Service Worker 経由の部分対応を検討)
- **Wry サイレントバグ**: `evaluate_script_with_callback` がナビゲーション中にコールバックをドロップする問題は PageLoadEvent Guard で回避するが、根本修正は Wry upstream に依存
- **Vision mode の座標精度**: DPI スケーリングやスクロール位置の考慮が必要

## 参照

- `apps/ato-desktop/src/webview.rs` — 既存 WebView 管理、evaluate_script 使用箇所
- `apps/ato-desktop/src/bridge.rs` — 既存 IPC bridge プロトコル
- `apps/ato-desktop/assets/preload/host_bridge.js` — 既存 preload script パターン
- [Playwright MCP](https://github.com/microsoft/playwright-mcp) — Tool 名・snapshot format の参考元
- [tauri-webdriver-automation](https://github.com/nicholasgasior/tauri-webdriver-automation) — Wry JS injection による WebDriver 実装の先行事例
