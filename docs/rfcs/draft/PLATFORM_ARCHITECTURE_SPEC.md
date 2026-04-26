---
title: "Platform Architecture Spec — The Web App OS"
status: draft
date: 2026-04-25
author: "@Koh0920"
ssot:
  - "apps/ato-desktop/src/orchestrator.rs"
  - "apps/ato-desktop/src/bridge.rs"
  - "apps/ato-desktop/src/webview.rs"
  - "apps/ato-desktop/src/state/mod.rs"
  - "apps/ato-desktop/src/cli_envelope.rs"
  - "apps/ato-cli/core/src/"
related:
  - "CROSS_PLATFORM_DISTRIBUTION_SPEC.md"
  - "DESKTOP_AUTOMATION_SPEC.md"
  - "UNIFIED_EXECUTION_MODEL.md"
  - "DRAFT_CAPSULE_IPC.md"
---

# Platform Architecture Spec — The Web App OS

## 1. Overview

ato-desktop is a **WebView-based application operating system**. It
provides sandboxed execution, lifecycle management, and a native host
chrome for third-party web applications ("capsules"), following the
same architectural principles that made VS Code the de facto standard
for extensible developer tools.

This document codifies the architectural invariants, process model,
communication protocol, and security boundaries that govern the
platform. It serves as the binding reference for all contributors.

## 2. Architectural Identity

ato is to web applications what VS Code is to programming languages.
The mapping is precise:

```
VS Code                         ato
===============================  ===============================
Main + Renderer Process          ato-desktop (GPUI)
Extension Host                   ato-cli (subprocess)
LSP / DAP                        Capsule Control Protocol (CCP)
Monaco Editor                    Wry (WebView)
Extension API                    Bridge IPC (BridgeProxy)
Extension Manifest               capsule.toml (+ config_schema)
Marketplace                      ato.run Store / Local Registry
```

### 2.1 Why This Mapping Matters

The analogy is not cosmetic. Each row encodes a **process boundary
decision** that determines crash isolation, security posture, and
platform extensibility:

| Decision | VS Code | ato | Consequence |
|---|---|---|---|
| Host survives guest crash | Main process survives Extension Host crash | GPUI survives ato-cli crash | Platform stability |
| Guest cannot escalate privilege | Extensions access only VS Code API | Capsules access only Bridge IPC (capability-gated) | Zero-Trust security |
| Protocol enables polyglot guests | LSP works with any language server | CCP works with any runtime (Deno, Node, Python, native) | Ecosystem growth |
| Rendering is embedded, not linked | Monaco is a browser component | Wry is a native WebView | Platform-independent content |

## 3. Process Model

### 3.1 Process Architecture

```
┌─────────────────────────────────────────────────────┐
│                  ato-desktop (GPUI)                  │
│                                                     │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────┐  │
│  │ Command  │  │ Sidebar  │  │  Stage / Panels  │  │
│  │   Bar    │  │  Rail    │  │                  │  │
│  └──────────┘  └──────────┘  │  ┌────────────┐  │  │
│                              │  │ Wry WebView│  │  │
│  AppState ◄── render ───────►│  │ (capsule)  │  │  │
│      │                       │  └─────┬──────┘  │  │
│      │                       └────────┼─────────┘  │
│      │                                │             │
│      │    BridgeProxy ◄── IPC ────────┘             │
│      │        │                                     │
│      │        │ capability_allowed()?                │
│      │        │                                     │
└──────┼────────┼─────────────────────────────────────┘
       │        │
       │  ┌─────▼─────────────────────────────────┐
       │  │          ato-cli (subprocess)          │
       │  │                                       │
       │  │  ato app resolve <handle> --json      │
       │  │  ato app session start <handle> --json│
       │  │  ato app session stop <id> --json     │
       │  │                                       │
       │  │  ┌─────────────────────────────────┐  │
       │  │  │    Capsule Process (sandboxed)  │  │
       │  │  │    Deno / Node / Python / native│  │
       │  │  └─────────────────────────────────┘  │
       │  └───────────────────────────────────────┘
       │
       ▼
  SecretStore / CapsuleConfigStore / Desktop Config
  (~/.ato/secrets.json, capsule-configs.json, etc.)
```

### 3.2 Role of Each Process

#### ato-desktop (GPUI) — The Sovereign

The desktop process is the **absolute authority**. It:

- Owns the window, the render loop, and all user interaction.
- Spawns and kills ato-cli subprocesses at will.
- Never executes capsule code in-process.
- Decides which capabilities to grant each capsule pane.
- Manages secrets and config persistence.

If ato-cli crashes or hangs, the desktop shows a toast and remains
fully operational. The user can retry or switch tabs without data loss.

This is equivalent to VS Code's Main Process.

#### ato-cli (subprocess) — The Extension Host

The CLI process is the **sandboxed execution environment**. It:

- Receives instructions via command-line args and returns JSON on
  stdout.
- Reports errors via structured JSONL on stderr.
- Manages capsule lifecycle: resolve, provision, execute, stop.
- Enforces runtime sandboxing (nacelle, network egress policy).
- Has no knowledge of the desktop's UI state.

If a capsule inside ato-cli panics, segfaults, or enters an infinite
loop, the desktop can `kill -9` the process tree. The capsule dies;
the OS survives.

This is equivalent to VS Code's Extension Host.

#### Wry WebView — Monaco Editor

The WebView renders capsule content inside the desktop's stage area.
Each pane gets a unique custom protocol scheme
(`capsule<partitionId>://`) preventing cross-capsule state leakage.

Guest JavaScript communicates with the host exclusively through the
Bridge IPC endpoint (`/__ato/bridge`). The host preloads adapter shims
(`tauri.js`, `electron.js`, `wails.js`) so capsules built for other
frameworks run unmodified.

This is equivalent to VS Code's Monaco Editor — the embeddable
rendering surface for user-facing content.

## 4. Capsule Control Protocol (CCP)

### 4.1 Protocol Principles

CCP is the JSON-based communication protocol between ato-desktop and
ato-cli, analogous to LSP (Language Server Protocol) in VS Code.

**Invariants:**

1. **Versioned:** Every envelope carries `schema_version: "ccp/v1"`.
2. **Additive-only:** Within a major version, fields may be added but
   never removed or renamed.
3. **Forward-compatible:** The desktop MUST ignore unknown fields.
   (Verified by test at `cli_envelope.rs:238-240`.)
4. **Unidirectional per call:** Desktop sends args → CLI returns JSON.
   No bidirectional streaming (V1).

### 4.2 Message Catalogue

#### 4.2.1 Resolve

```
Desktop → CLI:  ato app resolve <handle> --json
CLI → Desktop:  { schema_version, action: "resolve_handle", resolution: { ... } }
```

Returns metadata needed to create a pane: `kind`, `render_strategy`,
`trust_state`, `source`, `canonical_handle`, adapter profile.

#### 4.2.2 Session Start

```
Desktop → CLI:  ato app session start <handle> --json
                env: ATO_SECRET_<KEY>=<value>  (secrets)
                env: <KEY>=<value>              (plain config)
CLI → Desktop:  { schema_version, action: "session_start", session: { ... } }
```

Spawns the capsule process and returns: `session_id`, `pid`,
`local_url`, `display_strategy`, `log_path`.

#### 4.2.3 Session Stop

```
Desktop → CLI:  ato app session stop <session_id> --json
CLI → Desktop:  { schema_version, action: "session_stop", stopped: true }
```

#### 4.2.4 Error Envelope (E103 — Missing Config)

When a capsule requires configuration the user hasn't provided:

```
CLI → Desktop (stderr, JSONL):
{
  "level": "fatal",
  "code": "ATO_ERR_MISSING_REQUIRED_ENV",
  "details": {
    "missing_keys": ["OPENAI_API_KEY"],
    "missing_schema": [
      {
        "name": "OPENAI_API_KEY",
        "kind": "secret",
        "label": "OpenAI API Key",
        "description": "...",
        "placeholder": "sk-..."
      }
    ],
    "target": "app"
  }
}
```

The desktop parses `missing_schema`, builds a `PendingConfigRequest`,
and renders the ConfigModal overlay. After the user fills the form,
the desktop retries session start with the new env vars injected.

### 4.3 CCP vs Direct Library Import

| | CCP (current, correct) | Direct import (rejected) |
|---|---|---|
| Crash isolation | CLI crash = toast notification | CLI crash = desktop crash |
| Deployment | Independent versioning | Lockstep releases |
| Testing | CLI testable standalone | Requires GPUI test harness |
| Mobile | CLI runs as subprocess on any OS | Requires GPUI on all platforms |
| Complexity | Higher (serialize/deserialize) | Lower (function calls) |

The serialization cost is negligible. Capsule lifecycle operations are
IO-bound (process spawn, network, filesystem). The FFI overhead of
JSON parsing is measured in microseconds; process startup is measured
in milliseconds.

## 5. Security Boundaries

### 5.1 Three Walls

```
Wall 1: Process boundary (ato-desktop ↔ ato-cli)
  └── CLI crash cannot kill the desktop.
  └── Desktop can kill CLI at any time.

Wall 2: Sandbox boundary (ato-cli ↔ capsule process)
  └── Capsule runs inside nacelle sandbox.
  └── Network egress filtered by EgressPolicy.
  └── Filesystem access restricted to session root.

Wall 3: WebView boundary (ato-desktop ↔ Wry guest)
  └── Guest JS can only call Bridge IPC.
  └── Every call checked against CapabilityGrant allowlist.
  └── Custom protocol prevents cross-capsule data access.
```

### 5.2 Capability Model

Capabilities are declared at pane creation time and enforced at every
Bridge IPC call:

```rust
pub enum CapabilityGrant {
    ReadFile,         // Read files within app_root
    WorkspaceInfo,    // Query workspace metadata
    Automation,       // Programmatic control (MCP)
    OpenExternal,     // Open URLs in system browser
    // Future: Notification, Clipboard, FileWrite, ...
}
```

**Rule:** No new OS-level action may be exposed to capsules without:
1. A new `CapabilityGrant` variant.
2. An explicit grant at pane creation.
3. A `capability_allowed()` check in `BridgeProxy`.

### 5.3 Secret Injection Model

Secrets flow: `SecretStore` → `ATO_SECRET_<KEY>` env var → child
process. They never touch disk as plaintext, never appear in logs, and
are never readable by the capsule after process exit.

Plain config flows: `CapsuleConfigStore` → direct env var → child
process. Readable by the capsule at runtime, stored in
`~/.ato/capsule-configs.json`.

## 6. State Architecture

### 6.1 Platform-Independent State Layer

The entire state layer is **GPUI-free**:

```
capsule-core     (0 GPUI imports)  — manifest, routing, lockfile, error types
state/mod.rs     (0 GPUI imports)  — AppState, Workspace, Pane, GuestRoute
orchestrator.rs  (0 GPUI imports)  — subprocess lifecycle
bridge.rs        (0 GPUI imports)  — JSON-RPC dispatch
config.rs        (0 GPUI imports)  — SecretStore, CapsuleConfigStore
cli_envelope.rs  (0 GPUI imports)  — CCP error parsing
```

**Invariant:** These files MUST NEVER import `gpui`, `gpui_component`,
`wry`, `objc2`, or any platform-specific crate. This constraint is
what makes the V2.0 mobile migration (HostShell trait) feasible.

### 6.2 Rendering Layer (Platform-Specific)

Only these modules touch GPUI:

```
ui/mod.rs          — DesktopShell (implements Render for GPUI)
ui/chrome/         — command bar, window controls
ui/sidebar/        — workspace rail
ui/panels/         — stage area, settings, launcher
ui/modals/         — ConfigModal overlay
ui/share/          — task preview cards
app.rs             — Application bootstrap, key bindings
```

The rendering layer **reads** `AppState` and **maps** it to GPUI
elements. It does not contain business logic. Actions dispatched from
the UI mutate state, which triggers a re-render — a unidirectional
data flow.

### 6.3 Future: HostShell Trait (V2.0)

```rust
pub trait HostShell {
    type WebViewHandle;
    fn render_chrome(&mut self, state: &AppState);
    fn create_guest_webview(&mut self, config: WebViewConfig) -> Self::WebViewHandle;
    fn destroy_guest_webview(&mut self, handle: Self::WebViewHandle);
    fn dispatch_action(&mut self, action: AppAction);
    fn push_notification(&mut self, message: &str, tone: ActivityTone);
}

// V1: Desktop
struct GpuiShell { /* current DesktopShell */ }
impl HostShell for GpuiShell { ... }

// V2.1: iOS (via UniFFI)
// struct SwiftUIShell { ... }
// impl HostShell for SwiftUIShell { ... }
```

Because the state layer is already platform-independent, this
extraction is a refactor, not a rewrite.

## 7. Capsule Manifest as Extension Manifest

In VS Code, an extension's `package.json` declares capabilities,
activation events, and UI contributions. In ato, `capsule.toml`
serves the same role:

```toml
# capsule.toml — the extension manifest
schema_version = "0.3"
name = "byok-ai-chat"
runtime = "web/node"
run = "npm run dev"
port = 3000

# Schema-driven config (Feature 2)
[[config_schema]]
name = "OPENAI_API_KEY"
kind = "secret"
label = "OpenAI API Key"

[network]
egress_allow = ["api.openai.com"]
```

The manifest declares:
- **What** the capsule is (name, version, type)
- **How** to run it (runtime, run command, port)
- **What** it needs (config_schema — drives the ConfigModal)
- **What** it's allowed to do (network egress rules)

The desktop never needs to understand the capsule's internals. It
reads the manifest (via CCP) and renders the appropriate UI.

## 8. Known Limitations

- **No bidirectional CCP streaming (V1):** Desktop polls or waits for
  CLI exit. Real-time log streaming uses file tailing, not protocol
  messages. V2 may add a WebSocket or Unix socket channel.
- **Single CLI version per desktop:** The Helpers/ato binary is locked
  to the bundle version. Version negotiation is implicit.
- **Bridge IPC is request-response only:** No server-push from host to
  guest WebView. Capsules must poll or use postMessage callbacks.

## References

### Source Code
- `apps/ato-desktop/src/orchestrator.rs` — CLI subprocess management, CCP consumer
- `apps/ato-desktop/src/bridge.rs:134-396` — BridgeProxy, capability enforcement
- `apps/ato-desktop/src/webview.rs` — Wry WebView lifecycle, custom protocol
- `apps/ato-desktop/src/state/mod.rs` — Platform-independent state (AppState, Pane, GuestRoute)
- `apps/ato-desktop/src/cli_envelope.rs` — CCP error envelope parser
- `apps/ato-desktop/src/ui/mod.rs` — DesktopShell (GPUI rendering layer)
- `apps/ato-desktop/src/ui/modals/config_form.rs` — ConfigModal (E103 response UI)
- `apps/ato-cli/src/app_control/session.rs` — Session start/stop (CCP producer)
- `apps/ato-cli/core/src/` — capsule-core (0 GPUI imports, fully portable)

### External References
- [VS Code Architecture](https://code.visualstudio.com/api/advanced-topics/extension-host)
- [Language Server Protocol](https://microsoft.github.io/language-server-protocol/)
- [GPUI Framework (Glass-HQ)](https://github.com/Glass-HQ/gpui)
- [Wry Cross-Platform WebView](https://github.com/tauri-apps/wry)
- [Tauri 2.0 Mobile](https://v2.tauri.app/)
