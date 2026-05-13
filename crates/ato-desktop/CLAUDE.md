# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo run          # build and launch the desktop shell
cargo test         # run all unit tests
cargo build        # compile without running
cargo clippy       # lint
```

To run a single test:
```bash
cargo test <test_name>
# e.g. cargo test bridge_denies_unknown_capability
```

## Architecture

`ato-desktop` is a native macOS desktop shell built with **GPUI** (for the host UI chrome) and **Wry** (for embedded guest WebViews). It acts as a sandboxed container that mounts third-party app capsules as child WebViews.

### Module map

| Module | Responsibility |
|--------|---------------|
| `app.rs` | GPUI `Application` bootstrap, key bindings, window options |
| `state/mod.rs` | All state types: `AppState`, `Workspace`, `TaskSet`, `Pane`, `GuestRoute`, `CapabilityGrant` |
| `ui/` | GPUI render tree — `DesktopShell` owns state and dispatches actions to sub-renderers |
| `webview.rs` | `WebViewManager` — creates/destroys Wry child WebViews from `AppState` on each render pass |
| `bridge.rs` | `BridgeProxy` — handles JSON-RPC messages from guest JS over the custom protocol |
| `orchestrator.rs` | Subprocess wrapper around `ato-cli` for guest session lifecycle (resolve, start, stop) |

### State → WebView sync model

`DesktopShell::render` calls `webviews.sync_from_state(window, &mut state)` on every GPUI render. `WebViewManager` diffs the active pane against its cached state and tears down or rebuilds the Wry child WebView when the pane ID or route changes. Bounds changes are applied in place via `WebView::set_bounds`.

### Custom protocol (`capsule<partitionId>://`)

Each pane gets a scheme derived from its `partition_id`. All asset requests are handled asynchronously off the UI thread. The `/__ato/bridge` path is the IPC endpoint — guest JS POSTs JSON-RPC there; `BridgeProxy` validates the capability allowlist before dispatching. File reads are path-canonicalized against the session's `app_root` to prevent directory traversal.

### Guest session lifecycle

For `GuestRoute::LocalCapsule`, `webview.rs` calls `orchestrator::resolve_and_start_guest`, which shells out to `ato-cli` via `cargo run --manifest-path ../ato-cli/Cargo.toml`. Session state files live under `~/.ato/apps/desky/sessions/` (override with `DESKY_SESSION_ROOT`). On pane close or process exit, the session is stopped via `ato-cli app session stop`.

### Preload scripts (`assets/preload/`)

`host_bridge.js` is injected into every guest page before any page JS runs. It exposes the `window.__ATO_BRIDGE_*` globals and wires `fetch` calls to `/__ato/bridge`. Adapter shims (`tauri.js`, `electron.js`, `wails.js`) are appended after to emulate the native runtime each guest expects. The profile is inferred from the handle name (e.g. `*electron*` → `electron` shim).

### UI sub-modules

- `ui/chrome/` — top command bar with omnibar and window controls
- `ui/sidebar/` — workspace rail (left 64px strip)
- `ui/panels/` — stage area that hosts the pane layout
- `ui/share/` — task preview cards shown in overview mode

### Key layout constants (in `ui/mod.rs`)

`CHROME_HEIGHT = 48`, `RAIL_WIDTH = 64`, `STAGE_PADDING = 16`, `AGENT_PANEL_WIDTH = 280`, `OVERVIEW_HEIGHT = 210`. Stage bounds are recomputed on every render and pushed into `AppState` so the WebViewManager can resize the child WebView.

### Demo state

`AppState::demo()` seeds the shell with three `LocalCapsule` routes pointing at `../../samples/desky-real-{tauri,electron,wails}`, a bundled welcome page (`capsule://welcome/index.html`), and two external URLs. This exercises every rendering path on boot.

---

## Architecture decisions

### Terminal renderer: xterm.js (WebView) — alacritty 移行は見送り (2026-04-21)

現在の端末表示は `assets/terminal/index.html` + `xterm.js` を Wry WebView に載せた構成。

**採用理由 (xterm.js を継続)**

- MCP / automation ツールが WebView 前提で設計されており、DOM 要素・IPC チャネルを統一的に扱える。WebView でない描画面を追加すると MCP アダプタを別途実装する必要が生じる。
- 既存の `GuestRoute::Terminal` → bridge → orchestrator の PTY 経路が完成しており、置き換えコストが大きい。
- xterm.js は Canvas アドオンにより WKWebView の WebGL 制約を回避でき、描画品質は許容範囲内。

**alacritty 移行を見送った理由**

- alacritty はネイティブ GPU 描画のため、現行の WebView 統一面から外れる。
- MCP でターミナルを操作する場合、alacritty 専用のホスト側 MCP アダプタ（入力送信・リサイズ・出力取得・終了待ち）を新規実装する必要がある。
- 現状の bottleneck（flow control / backpressure）は alacritty 移行なしに xterm.js 側で改善できる。

**TODO: alacritty ネイティブ端末化（Phase N）**

以下が揃ったタイミングで再評価する:

1. MCP が WebView 非依存の「ターミナル操作 tool」を標準化、あるいは ato-mcp に専用ツールを追加できる見通しが立つ。
2. xterm.js の flow control 改善後も描画性能・入力レイテンシが基準に満たない実測データが揃う。
3. GPUI ネイティブテキスト描画が alacritty_terminal クレートとの統合を十分サポートする。

移行する場合の実装要件:
- `alacritty_terminal` crate で PTY parser + cell buffer を管理。
- GPUI の `Canvas` / 独自レンダー層でセルを描画。
- ato-mcp に `terminal_write`, `terminal_read`, `terminal_resize`, `terminal_wait` ツールを追加。
- 既存の `GuestRoute::Terminal` / bridge / orchestrator の PTY 経路との統合を再設計。

---

## Agent workflows

- [`SKILL.md`](./SKILL.md) — Mockup HTML → GPUI component workflow. When the
  user hands you HTML for a UI element, lower it with the external
  [`gpui-html`](https://github.com/ato-run/gpui-html) CLI and port the
  generated Rust skeleton into `src/ui/`. Do **not** add an HTML parser to
  this crate or take a `gpui-html-core` dependency.
