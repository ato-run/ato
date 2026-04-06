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
