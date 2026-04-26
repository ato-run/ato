# Desky Guest Contract Reference

**Status:** Draft v0.1  
**Last Updated:** 2026-04-05  
**Audience:** Desky guest capsule authors and Desky host implementers

---

## Purpose

This document defines the minimum contract a guest capsule must satisfy to run inside Desky during Phase 0 and Phase 1.

The contract is intentionally narrow. It does not aim to run arbitrary Tauri, Wails, or Electron binaries as-is. Instead, it defines the metadata and runtime behavior required for a capsule frontend to be rendered by Desky while a separate backend process is started and supervised by Ato.

## Scope

Included:

- guest metadata required in `capsule.toml`
- frontend asset discovery
- backend launch contract
- readiness and transport expectations
- fail-closed rules

Excluded:

- native window embedding
- app store packaging rules
- permission UI flows beyond the minimum capability list
- Phase 2 Wry implementation detail

---

## Contract Summary

A Desky guest capsule must provide all of the following:

1. A launchable backend target in `capsule.toml`
2. Guest metadata under `[metadata.desky_guest]`
3. A frontend entry file that Desky can load in a tab
4. A readiness endpoint the host can poll before the tab is considered ready
5. An invoke transport endpoint the host can call after readiness

The backend must behave like a headless logic server. It must not require that it owns the user-visible window.

For long-term adoption, a guest should ideally remain usable both as a standalone native app and as a Desky guest. This document therefore separates the low-level runtime contract from the higher-level SDK experience layered on top of it.

---

## Manifest Contract

Desky currently reads the following guest metadata:

```toml
[metadata.desky_guest]
adapter = "tauri"
frontend_entry = "frontend/index.html"
transport = "http"
rpc_path = "/rpc"
health_path = "/health"
default_port = 43123
capabilities = [
  "ping",
  "app.invoke",
  "plugin:window|setTitle",
  "plugin:dialog|open",
  "plugin:fs|readFile",
  "shell.open",
]
```

### Field Reference

- `adapter`
  Identifies the guest API surface Desky should emulate. Current planned values are `tauri`, `wails`, and `electron`.
- `frontend_entry`
  Relative path to the frontend entry asset Desky should load.
- `transport`
  Transport kind used by the backend. Phase 0 supports `http`.
- `rpc_path`
  HTTP path for invoke requests.
- `health_path`
  HTTP path for readiness checks.
- `default_port`
  Preferred local port for the guest backend. Desky may choose another local port if this one is unavailable.
- `capabilities`
  Minimal allowlist exposed to the host and preload adapter. Current implemented capability names include generic invoke (`app.invoke`), direct commands such as `ping`, host-managed plugin capabilities such as `plugin:fs|readFile`, `plugin:dialog|open`, `plugin:window|setTitle`, and `shell.open`.

## Frontend Host Contract

When a guest frontend is loaded inside Desky, the host currently guarantees all of the following:

- frontend assets are served from `capsule://<session_id>/...`
- the guest receives a session metadata object before requests are drained
- the guest can observe an origin hint of `tauri://localhost` when it needs framework-compatible behavior
- unsupported capabilities are rejected before backend forwarding

This means a guest frontend must tolerate explicit promise rejection for undeclared or out-of-policy host calls.

## Proposed Dual-Mode Runtime Contract

The canonical runtime signal for Desky guest mode is:

- `ATO_GUEST_MODE=1`

Semantics:

- if `ATO_GUEST_MODE` is absent, the app runs as a normal standalone native app
- if `ATO_GUEST_MODE=1`, the app runs as a Desky guest and must yield visible window ownership to the host

This contract is intentionally minimal. It does not require a separate Desky-only artifact.

Allowed guest-mode strategies:

- create the normal window and immediately hide it
- skip primary window creation and run backend logic only
- switch to a framework-provided server mode when that mode already exists

The important requirement is behavioral: the same shipped app should remain launchable both outside and inside Desky.

## Backend Contract

The backend target remains a normal capsule target. For Phase 0, Desky accepts experimental guest drivers for local resolution and session startup.

Example:

```toml
schema_version = "0.2"
name = "desky-mock-tauri"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "tauri"
run_command = "python3 backend/server.py"
working_dir = "."
```

The important point is not the framework label itself. The important point is that the launched backend must work as a logic server without owning a window.

---

## Host Environment Variables

When Desky starts a guest session, it may inject the following variables:

- `ATO_GUEST_MODE`
- `DESKY_SESSION_ID`
- `DESKY_SESSION_HOST`
- `DESKY_SESSION_PORT`
- `DESKY_SESSION_ADAPTER`
- `DESKY_SESSION_RPC_PATH`
- `DESKY_SESSION_HEALTH_PATH`

A guest backend must be prepared to bind to the provided host and port.

The frontend should not assume direct access to `file://` assets. Desky currently loads guest HTML, JS, and CSS via the `capsule://` custom protocol and attaches a host-defined `Content-Security-Policy` header to HTML responses.

`ATO_GUEST_MODE` is the app-facing mode switch. The `DESKY_SESSION_*` variables remain session transport metadata.

---

## Readiness Rules

A guest backend is considered ready only when the host can successfully call:

- `GET http://127.0.0.1:<port><health_path>`

If readiness is not confirmed before timeout, the session fails closed.

Fail-closed behavior means:

- the tab must not be marked interactive
- pending invoke requests must not be forwarded
- the process must be treated as failed startup

---

## Invoke Rules

After readiness, the host may send invoke requests to:

- `POST http://127.0.0.1:<port><rpc_path>`

Phase 0 uses JSON-RPC shaped payloads. A minimal request body is:

```json
{
  "jsonrpc": "2.0",
  "id": "req-1",
  "method": "capsule/invoke",
  "params": {
    "command": "ping",
    "payload": {
      "message": "hello"
    }
  }
}
```

A successful response is:

```json
{
  "jsonrpc": "2.0",
  "id": "req-1",
  "result": {
    "ok": true,
    "echo": "hello"
  }
}
```

Framework-specific frontend APIs are normalized by the preload layer before they reach this backend transport. Current implemented compatibility surfaces are:

- Tauri: `window.__TAURI_INTERNALS__.invoke`, `window.__TAURI__.fs`, `window.__TAURI__.dialog`, `window.__TAURI__.window`, `window.__TAURI__.shell`
- Wails: `window.runtime.Invoke`, `window.runtime.invoke`, `window.go.main.App.*`
- Electron guest: limited `window.electron.ipcRenderer.invoke` allowlist

## SDK Layer

The low-level environment-variable contract is necessary but not sufficient for good developer experience. Desky therefore plans a thin integration SDK layer for each supported framework.

The SDK should let an app author opt into dual-mode behavior with minimal boilerplate.

Required SDK behavior:

- detect `ATO_GUEST_MODE`
- apply guest-mode window policy for the framework
- keep standalone launch behavior unchanged when `ATO_GUEST_MODE` is absent
- prepare readiness and teardown hooks required by the Desky guest contract

Current first-cut implementation in this workspace:

- backend runtime SDKs
  - `apps/ato-cli/packages/desky-guest-tauri`
  - `apps/ato-cli/packages/desky-guest-wails`
  - `apps/ato-cli/packages/desky-guest-electron-backend`
- frontend bridge helper source
  - `apps/ato-cli/packages/desky-guest-frontend`

Current frontend delivery model:

- browser helper source is maintained in the shared package
- runtime helper files are vendored into each guest sample under `frontend/vendor/desky-guest-frontend/`
- sample-local `bridge.js` files are thin re-export shims over the vendored runtime copy

The vendoring step is required because Desky currently serves guest assets only from the directory rooted at `frontend_entry`. Browser modules outside that root are intentionally not importable from `capsule://<session_id>/...`.

SDK non-goals:

- replacing the framework's own IPC model wholesale
- forcing a special Ato-only application layout
- making unsupported frameworks appear supported

---

## Fail-Closed Requirements

Desky and the guest must follow these rules:

- Unknown adapter metadata is invalid for guest-webview mode.
- Missing `frontend_entry` is invalid.
- Missing `health_path` or `rpc_path` is invalid.
- Guest startup is unsuccessful if readiness never becomes healthy.
- Unsupported handle kinds must return a structured error rather than guessing a launch strategy.
- Undeclared capability requests must be rejected before backend forwarding.
- Host-managed plugin APIs may enforce boundary policy locally, and boundary violations such as reading outside the workspace root must be rejected without calling the backend.
- `tab close`, `reload`, `force stop`, and renderer crash must release the guest session through `ato app session stop`.
- guest mode support must not silently break standalone native launch behavior.

---

## Phase 0 Deliverables

A Phase 0-compliant guest must demonstrate:

1. `ato app resolve <local-path> --json` returns `guest-webview` and guest metadata.
2. `ato app session start <local-path> --json` starts the backend and returns an invoke URL.
3. The backend returns `200 OK` for health.
4. The backend accepts one `capsule/invoke` request and returns a JSON-RPC response.
5. `ato app session stop <session-id> --json` stops the process and clears session state.

## Current Phase 1.5 Deliverables

As of 2026-04-05, the following behaviors are implemented and expected:

1. Tauri guest plugin APIs for file read, dialog open, and window title update are intercepted by Desky.
2. Wails guests can use both `window.runtime.*` and `window.go.*` shims.
3. Electron guests can use a limited `ipcRenderer.invoke`-style allowlist exposed by Desky.
4. Guest assets load only from `capsule://<session_id>/...` and HTML responses receive host-defined CSP.
5. Session stop on tab close or crash is verified by E2E coverage that confirms the backend process is reaped from the OS.

## Proposed Phase 1.6 Deliverables

1. `ATO_GUEST_MODE=1` is injected by Desky when launching guest sessions.
2. Official first-cut backend runtime SDKs exist for Tauri, Wails, and Electron in the workspace.
3. Shared frontend bridge helper source exists and can be vendored into guest frontend roots.
4. A guest can declare dual-mode support without requiring a separate guest-only binary.
5. Store and manifest surfaces can distinguish dual-mode-capable guests from guest-only experiments.

---

## Non-Goals

The following are explicitly out of scope for this contract revision:

- binary patching of framework runtimes
- extraction of Tauri invoke keys
- virtual display requirements
- embedding native windows into Desky
- complete framework API emulation

The contract is designed to prove the Desky architecture with the smallest viable surface first.
