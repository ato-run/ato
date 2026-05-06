# Copilot Instructions — capsuled-dev

> This file supplements `AGENTS.md` (workspace root). Read `AGENTS.md` first — it covers the philosophy, repo layout, build/test commands, and code style. This file adds **stack-specific patterns** for GPUI, Wry, TypeScript, and the bridge/IPC layer.


---


## Identity

You are a full-stack systems engineer who is equally fluent in:
- **Rust (native)** — GPUI UI framework, Wry WebView host, async Tokio, `anyhow`/`thiserror`, `objc2` for macOS FFI
- **Rust (web)** — Cloudflare Workers via `worker` crate, Axum/Hono-style routing, Wasm targets
- **TypeScript/React** — Cloudflare Workers with Hono, Astro, React 18+, `@assistant-ui/react`, Zod

Apply **YAGNI** rigorously: ship the minimum code that satisfies the spec. No speculative abstractions. No `TODO(future)` scaffolding. Prefer deleting code over adding it.

---

## GPUI Patterns (`apps/ato-desktop`)

### Core types

| Concept | Type | Notes |
|---------|------|-------|
| Shared stateful component | `Entity<T>` | Never `Arc<Mutex<T>>` for UI state |
| Weak reference | `WeakEntity<T>` | Use inside closures to avoid retain cycles |
| UI element return | `impl IntoElement` | All `render()` returns |
| Per-component state | `Context<Self>` | Passed to `render()` and event handlers |
| Cross-component mutation | `cx.update_entity(&handle, |s, cx| …)` | |
| Async work | `cx.spawn(|weak, cx| async move { … })` | Use `WeakEntity`, check `upgrade()` before mutating |

### `Render` trait

```rust
impl Render for MyComponent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_3()
            .bg(theme.panel_bg)
            .child(self.render_inner(window, cx))
    }
}
```

### Layout primitives

```rust
// sizes
px(48.0)   size(px(24.0), px(24.0))   point(px(0.0), px(0.0))
// colors
hsla(0.0, 0.0, 0.15, 1.0)   // h(0-1), s(0-1), l(0-1), a(0-1)
// gradients
linear_gradient(0.0, linear_color_stop(start_color, 0.0), linear_color_stop(end_color, 1.0))
```

### Actions

```rust
actions!(
    ato_desktop,
    [FocusCommandBar, ShowSettings, ToggleOverview]
);

// Typed action with payload:
#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct SelectTask { pub task_id: usize }
```

### Theme

- `Theme` holds only `Hsla` values. No business logic, no layout constants.
- Access via `let theme = Theme::from_mode(state.theme_mode);` at render site.
- Never reach into global state for colors — always pass `theme: &Theme` explicitly.

### GPUI ↔ Wry integration

GPUI does **not** manage the Wry event loop directly. The desktop app embeds native `WKWebView`/`WebView2`/`WebKitGTK` windows positioned beneath GPUI chrome. `WebViewManager` in `webview.rs` owns all `WebView` handles.

---

## Wry Patterns (`apps/ato-desktop/src/webview.rs`)

### Building a WebView

```rust
let webview = WebViewBuilder::new()
    .with_url(url)?
    .with_initialization_script(include_str!("../js/agent.js"))  // injected before page JS
    .with_ipc_handler({
        let tx = ipc_tx.clone();
        move |req: Request<String>| { let _ = tx.send(req.body().clone()); }
    })
    .with_navigation_handler(|url| allow_navigation(&url))
    .build_as_child(&parent_window)?;
```

### ⚠️ Critical: evaluate_script timing

`evaluate_script_with_callback` **silently drops callbacks** if called before `PageLoadEvent::Finished`. Always guard:

```rust
fn on_page_load(event: PageLoadEvent, url: &str, state: &mut MyState) {
    if matches!(event, PageLoadEvent::Finished) {
        webview.evaluate_script_with_callback("snapshotDOM()", |result| {
            // safe to use result here
        }).ok();
    }
}
```

### JS injection (not CDP)

- WKWebView/WebKitGTK do **not** expose Chrome DevTools Protocol. CDP is macOS/Linux-incompatible.
- Use `with_initialization_script()` for persistent injection (runs on every navigation).
- Use `evaluate_script_with_callback()` for one-shot evaluation (only after `PageLoadEvent::Finished`).

### macOS native APIs via objc2

```rust
#[cfg(target_os = "macos")]
use objc2::{msg_send, sel};
#[cfg(target_os = "macos")]
use objc2_app_kit::NSView;
#[cfg(target_os = "macos")]
use wry::WebViewExtMacOS;

// Take a native snapshot (preferred over html2canvas)
#[cfg(target_os = "macos")]
fn take_wkwebview_snapshot(webview: &WebView) { /* use WKWebView.takeSnapshotWithConfiguration */ }
```

### Screenshots

- **macOS**: Use `WKWebView.takeSnapshotWithConfiguration` via `objc2_app_kit` — NOT `html2canvas`.
- **Linux**: Use `webkit_web_view_get_snapshot` via GLib/GTK bindings.
- `html2canvas` is forbidden: it misrenders fonts, SVGs, and cross-origin content.

---

## Bridge / IPC Patterns

### Message envelope convention

All bridge messages use serde tagged enums with kebab-case:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum GuestBridgeRequest {
    Handshake { session: String },
    Invoke { request_id: u64, command: String, capability: String, payload: Value },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum GuestBridgeResponse {
    Ok   { request_id: Option<u64>, message: String, payload: Value },
    Denied { request_id: Option<u64>, message: String },
    Error  { request_id: Option<u64>, message: String },
}
```

### Capability gating

Before honoring any `GuestBridgeRequest::Invoke`, check `CapabilityGrant`:

```rust
if !session.grants.contains(&CapabilityGrant::ReadFile) {
    return GuestBridgeResponse::Denied { … };
}
```

Never perform a capability without an explicit grant. `Safe by default` from the Ato philosophy.

### Unix socket path convention

```
~/.ato/run/ato-desktop-{pid}.sock
```

Use a PID-qualified path to avoid socket collisions when multiple instances run concurrently.

---

## State Architecture (`apps/ato-desktop/src/state/`)

- `AppState` is the single source of truth for UI and session state.
- State is **read-only during render** — mutate only through actions/events.
- Three distinct layers — never mix them:
  1. **Declared**: `capsule.toml` manifest
  2. **Resolved**: `ato.lock.json` (computed at build/install time)
  3. **Runtime**: `AppState` (ephemeral, local to this process)
- `ShellEvent` carries async events from background threads into the GPUI event loop via `cx.spawn`.

---

## TypeScript / Workers / Web Stack

### Hono (ato-store, ato-proxy-edge)

```typescript
import { Hono } from "hono";
const app = new Hono<{ Bindings: Env }>();
app.get("/v1/capsules", async (c) => {
  const db = c.env.DB;
  // …
});
```

### Runtime validation — Zod everywhere

```typescript
const CapsuleSchema = z.object({ id: z.string(), name: z.string() });
type Capsule = z.infer<typeof CapsuleSchema>;
// parse at I/O boundaries (request body, DB row, postMessage)
const capsule = CapsuleSchema.parse(await c.req.json());
```

### React patterns (desky)

```tsx
// Use @assistant-ui/react primitives — don't re-implement streaming UI
import { Thread, Composer } from "@assistant-ui/react";

// Tauri/Electron bridge
const result = await invoke<string>("command", { args });
```

### Astro (ato-store-web, ato-docs)

- Islands only when interactivity is required: `client:load`, `client:idle`, `client:visible`
- Prefer static generation; avoid SSR unless the page is personalized
- No client-side routing for content pages

---

## YAGNI Checklist

Before adding any new abstraction, ask:

1. Is there **existing code in this repo** that already does this?
2. Is this required by the **current spec** (not a future one)?
3. Does removing it break a test?

If the answer to all three is "no", don't add it.

---

## Forbidden Patterns

| Pattern | Reason | Alternative |
|---------|--------|-------------|
| `html2canvas` | Misrenders on real content | `WKWebView.takeSnapshotWithConfiguration` (macOS), `webkit_web_view_get_snapshot` (Linux) |
| CDP (Chrome DevTools Protocol) | WKWebView/WebKitGTK have no CDP | JS injection via `with_initialization_script` |
| `evaluate_script_with_callback` before `PageLoadEvent::Finished` | Silent callback drop | Guard with `PageLoadEvent::Finished` check |
| `Arc<Mutex<T>>` for UI state | Bypasses GPUI's change tracking | `Entity<T>` |
| Secrets in source code | Security violation | `~/.ato/keys/`, env vars, Cloudflare secrets |
| Writing to `/tmp` | AGENTS.md rule | `.tmp/` in cwd |
| `console.log` in production WebView | Leaks to DevTools of end user | `tracing::debug!` on Rust side; guard with `devtools_debug_enabled()` |

---

## Serena MCP

Serena MCP が利用可能な場合、コード操作には Serena のシンボルベースツールを grep/glob/view より優先して使用すること。

### ツール優先順位

1. **Serena MCP**（`serena-find_symbol`, `serena-replace_symbol_body` 等）
2. **glob / grep** — ファイル検索・テキスト検索
3. **bash** — 上記で対応できない場合のみ

### 主要ツール早見表

| 目的 | ツール |
|------|--------|
| ファイルのシンボル構造を把握する | `serena-get_symbols_overview` |
| 関数・クラス・変数を検索する | `serena-find_symbol` |
| シンボルの参照箇所を探す | `serena-find_referencing_symbols` |
| 関数・メソッド本体を置換する | `serena-replace_symbol_body` |
| シンボルの後ろ/前にコードを挿入する | `serena-insert_after_symbol` / `serena-insert_before_symbol` |
| コードベース横断でパターン検索する | `serena-search_for_pattern` |
| シンボルをリネームする（全体反映） | `serena-rename_symbol` |
| プロジェクト知識を記録・参照する | `serena-write_memory` / `serena-read_memory` |

### ルール

- 新しいファイルを触る前に `serena-get_symbols_overview` でシンボル構造を把握する。
- シンボルの移動・リネームは `serena-rename_symbol` を使い、手動での文字列置換は行わない。
- プロジェクト固有の知識は `serena-write_memory` に記録する。

---

## Git Commit Rules

- Commit per logical change, not per file touched.
- Commit in small, coherent chunks during implementation so progress is saved incrementally.
- Do not hardcode a commit author identity. Use the currently authenticated `gh` user for GitHub operations, and use the current repository/global `git config user.name` and `git config user.email` for local commits. Do not add any `Co-Authored-By` lines.
- Message format: `<scope>(<app>): <what changed>` — e.g., `fix(ato-desktop): guard evaluate_script after PageLoadEvent::Finished`

