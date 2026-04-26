---
title: "Cross-Platform Distribution & UI Architecture Spec"
status: draft
date: 2026-04-25
author: "@Koh0920"
ssot:
  - "apps/ato-desktop/xtask/src/main.rs"
  - "apps/ato-desktop/src/orchestrator.rs"
  - "apps/ato-cli/src/main.rs"
related:
  - "PLATFORM_ARCHITECTURE_SPEC.md"
  - "DESKTOP_AUTOMATION_SPEC.md"
  - "UNIFIED_EXECUTION_MODEL.md"
---

# Cross-Platform Distribution & UI Architecture Spec

## 1. Overview

This RFC defines the distribution strategy for the ato platform across
desktop (macOS, Windows, Linux) and mobile (iOS, Android) targets. It
establishes two orthogonal concerns:

1. **Distribution (V1.1):** How the CLI and Desktop binaries are
   packaged and delivered to users as a single install.
2. **UI Architecture (V2+):** How the host chrome (command bar, sidebar,
   panels) is rendered on platforms where GPUI is not available.

The guiding principle is: **process isolation is non-negotiable; binary
packaging is a UX optimisation.**

## 2. Scope

### In Scope

- V1.1: macOS App Bundle with symlink-based CLI install + Homebrew Cask
- V1.1: Windows installer with PATH registration
- V1.1: Linux AppImage / deb / Homebrew-on-Linux
- V2+: Platform abstraction layer (HostShell trait) for mobile
- V2+: iOS shell (SwiftUI + Wry + capsule-core via UniFFI)
- V2+: Android shell (Jetpack Compose + Wry + capsule-core via UniFFI)

### Out of Scope

- Single-binary unification (rejected — see section 6)
- In-process CLI embedding / dylib (rejected — breaks process isolation)
- Web target (deferred to V3+)
- GPUI mobile port (blocked on upstream — see section 3.1)

## 3. Platform Analysis

### 3.1 GPUI Platform Support (as of 2026-04)

| Platform      | Status          | Backend          | Source                  |
|---------------|-----------------|------------------|-------------------------|
| macOS         | Stable          | Metal            | Zed upstream            |
| Linux         | Stable          | Vulkan (Blade)   | Zed upstream            |
| Windows       | Stable          | DirectX 11       | Zed upstream (Oct 2025) |
| iOS           | Experimental    | wgpu/Metal       | gpui-mobile (community) |
| Android       | Experimental    | wgpu/Vulkan      | gpui-mobile (community) |
| Web           | Not implemented | -                | Glass-HQ claims only    |

**Assessment:** Desktop is covered. Mobile is not production-ready and
depends on unpublished upstream crates. Building on gpui-mobile would be
a single-vendor risk with no production precedents.

### 3.2 Wry Platform Support

| Platform | Status          | WebView Engine    |
|----------|-----------------|-------------------|
| macOS    | Stable          | WKWebView         |
| Windows  | Stable          | WebView2 (Edge)   |
| Linux    | Stable          | WebKitGTK (X11)   |
| iOS      | Stable          | WKWebView         |
| Android  | Stable          | Android WebView   |

**Assessment:** Wry is production-ready on all targets. It is already
used by ato-desktop for guest capsule rendering and by Tauri 2.0 for
mobile apps (Hoppscotch, Spacedrive, etc.).

### 3.3 Alternative Frameworks Evaluated

| Framework  | Desktop | Mobile     | Verdict                              |
|------------|---------|------------|--------------------------------------|
| Tauri 2.0  | Stable  | Stable     | WebView-in-WebView nesting problem   |
| Dioxus     | Stable  | Android experimental | Too early for production      |
| Slint      | Stable  | Embedded only | No mobile support                 |
| Flutter    | Stable  | Stable     | Not Rust-native; separate ecosystem  |

## 4. Architecture

### 4.1 Current Stack (V1.0)

```
Ato Desktop.app/
  Contents/
    MacOS/ato-desktop       GPUI host chrome + Wry guest WebViews
    Helpers/ato             CLI binary (subprocess)
    Resources/assets/
    Info.plist
```

- GPUI renders the host chrome (command bar, sidebar, panels).
- Wry renders guest capsule content in sandboxed WebViews.
- `ato` CLI is spawned as a subprocess for all capsule lifecycle
  operations (`resolve`, `session start/stop`).
- Process isolation is enforced: capsule crashes cannot take down the
  host shell.

### 4.2 V1.1: Distribution Unification

#### 4.2.1 macOS: App Bundle + CLI Symlink

On first launch (or via a menu item "Install CLI"), the desktop creates:

```
/usr/local/bin/ato -> /Applications/Ato Desktop.app/Contents/Helpers/ato
```

If `/usr/local/bin` is not writable, fall back to `~/.local/bin/ato`
and prompt the user to add it to PATH.

Implementation: ~30 lines in `app.rs`, following the VS Code
"Install 'code' command in PATH" pattern.

#### 4.2.2 macOS: Homebrew Cask

```ruby
cask "ato" do
  version "1.1.0"
  sha256 "..."
  url "https://github.com/ato-run/releases/download/v#{version}/Ato-Desktop-#{version}-darwin-arm64.dmg"
  name "Ato Desktop"
  homepage "https://ato.run"
  binary "Ato Desktop.app/Contents/Helpers/ato"
  app "Ato Desktop.app"
end
```

`brew install --cask ato` installs both the GUI app and the CLI binary
in a single command. The `binary` stanza creates the symlink
automatically.

#### 4.2.3 Windows: MSI/NSIS Installer

- Installs `ato-desktop.exe` to `Program Files\Ato\`
- Installs `ato.exe` to `Program Files\Ato\bin\`
- Adds `Program Files\Ato\bin` to user PATH
- Registers `ato://` and `capsule://` URL schemes

#### 4.2.4 Linux: AppImage + Standalone CLI

- AppImage bundles both binaries (extracted via `--appimage-extract`)
- Standalone CLI: `curl https://ato.run/install | sh` (existing)
- Homebrew on Linux: `brew install ato` (CLI only), `brew install
  --cask ato` (Desktop + CLI)
- deb/rpm: future consideration

### 4.3 V2.0+: HostShell Abstraction for Mobile

#### 4.3.1 Trait Design

```rust
/// Platform-agnostic interface for the host chrome layer.
/// Each platform provides its own implementation; the shared
/// business logic (state, orchestrator, bridge) is pure Rust.
pub trait HostShell {
    type WebViewHandle;

    fn render_chrome(&mut self, state: &AppState);
    fn create_guest_webview(&mut self, config: WebViewConfig) -> Self::WebViewHandle;
    fn destroy_guest_webview(&mut self, handle: Self::WebViewHandle);
    fn dispatch_action(&mut self, action: AppAction);
    fn push_notification(&mut self, message: &str, tone: ActivityTone);
}
```

#### 4.3.2 Platform Implementations

```
capsule-core          (Pure Rust, all platforms)
ato-state             (Pure Rust, all platforms)
ato-orchestrator      (Pure Rust, subprocess IPC)
ato-bridge            (Pure Rust, JSON-RPC)
  |
  +-- ato-shell-gpui      (macOS/Windows/Linux) -- current DesktopShell
  +-- ato-shell-ios       (iOS) -- SwiftUI + UniFFI + Wry
  +-- ato-shell-android   (Android) -- Jetpack Compose + UniFFI + Wry
```

#### 4.3.3 Mobile: Shared Rust Core via UniFFI

The Rust business logic (capsule-core, state, orchestrator, bridge) is
exposed to Swift/Kotlin via Mozilla's UniFFI. The mobile shell calls
the same `resolve_and_start_capsule()` function as the desktop, but
through FFI instead of subprocess IPC.

Wry handles guest WebView rendering on mobile (WKWebView on iOS,
Android WebView on Android) — it is already production-ready on both.

The host chrome (command bar, sidebar, panels) is implemented natively:
- iOS: SwiftUI
- Android: Jetpack Compose

This gives each platform maximum native feel while sharing 100% of the
capsule lifecycle, state management, and security logic.

### 4.4 Design Principles (VS Code Lessons)

Three architectural patterns from VS Code's success apply directly to
ato-desktop's evolution. All three are partially implemented today; this
section codifies them as binding constraints for future work.

#### 4.4.1 Capsule Control Protocol (CCP) — LSP for capsule lifecycle

VS Code created LSP (Language Server Protocol) so the editor never
needs to understand any language's grammar — it sends JSON and gets JSON
back. The same principle applies to ato-desktop's communication with
ato-cli.

**Current state:** The JSON envelope is implemented ad-hoc in
`cli_envelope.rs` (E103 parser, fatal JSONL extraction). The `ato app`
subcommands (`resolve --json`, `session start --json`, `session stop
--json`) return structured JSON but the schema is defined only by Rust
struct serialization — no versioned specification exists.

**Constraint:** All desktop-to-CLI communication MUST go through a
versioned JSON protocol ("CCP"). The desktop MUST NOT import ato-cli
library code for lifecycle operations. The protocol MUST be:

- **Versioned:** `schema_version: "ccp/v1"` in every envelope.
- **Additive-only:** New fields may be added; existing fields are never
  removed or renamed within a major version.
- **Forward-compatible:** The desktop MUST ignore unknown fields
  (already enforced by `cli_envelope.rs`'s `serde(deny_unknown_fields)`
  absence and explicit tolerance tests at lines 238-240).

**V1.1 action:** Extract the implicit schema from `session.rs`
(`SessionStartEnvelope`, `ResolveEnvelope`) and `cli_envelope.rs`
(`CliErrorEvent`, `MissingEnvDetails`) into a standalone
`docs/specs/CCP_SPEC.md`.

#### 4.4.2 Capability-Gated Bridge — Extension Host for capsules

VS Code's Extension Host ensures extensions can only access the VS Code
API through a declared capability manifest. ato-desktop's `BridgeProxy`
(`bridge.rs:134-396`) already implements the same pattern:

- Guest JavaScript can only call the host through `/__ato/bridge` IPC.
- Every request is checked against a per-pane `allowlist` via
  `capability_allowed()` before dispatch.
- Capabilities are granted per-route at pane creation time
  (`CapabilityGrant::ReadFile`, `WorkspaceInfo`, `Automation`, etc.).

**Constraint:** No new host-side action (OS notification, clipboard,
file write, network proxy) may be exposed to capsules without:

1. A corresponding `CapabilityGrant` variant.
2. An explicit grant at pane creation time.
3. A `capability_allowed()` check in `BridgeProxy`.

This is the Zero-Trust boundary that makes ato an OS, not just a
launcher.

#### 4.4.3 Platform-Independent State — Monaco pattern

VS Code's Monaco Editor is browser-native, decoupled from Node.js. This
enabled `vscode.dev` (Web VS Code) with minimal effort. ato-desktop
follows the same pattern:

- `capsule-core` has **zero** GPUI imports (verified: `grep "use gpui"
  core/src/` returns 0 matches). It is pure platform-independent Rust.
- `AppState`, `Workspace`, `Pane`, `GuestRoute` are plain Rust structs
  with no rendering logic.
- The rendering layer (`ui/mod.rs`, `DesktopShell`) reads state and
  maps it to GPUI elements — a one-directional data flow.

**Constraint:** `capsule-core`, `state/mod.rs`, `orchestrator.rs`,
`bridge.rs`, and `config.rs` MUST NEVER import GPUI, Wry, or any
platform-specific crate. If a future contributor adds `use gpui::` to
any of these files, it is a build-breaking architectural violation.

This constraint is what makes the V2.0 `HostShell` trait migration
feasible: the entire business logic layer is already portable.

### 4.5 Rejected Approaches

| Approach | Reason for Rejection |
|---|---|
| **Single binary (C)** | macOS-only deps (gpui, objc2) infect CLI builds for Linux/Windows. Feature-flag isolation is fragile and makes CI complex. |
| **dylib / dlopen (E)** | Breaks process isolation. Guest panic kills the host. |
| **Wasm CLI (F)** | WASI cannot do networking/filesystem at the level CLI requires. |
| **Wait for GPUI mobile (E)** | gpui-mobile is experimental, depends on unpublished crates, zero production users. Single-vendor risk. |
| **Tauri full replacement (B)** | WebView-in-WebView nesting breaks the custom protocol sandboxing model (`capsule<partitionId>://`). |

## 5. Security Considerations

- Process isolation between host shell and CLI is maintained on all
  platforms. Mobile uses FFI for the Rust core but spawns capsule
  processes in separate sandboxed contexts.
- CLI binary integrity: on macOS, the CLI in `Helpers/ato` is covered
  by the same code signature as the app bundle. On other platforms,
  checksum verification is performed at install time.
- Symlink creation (`/usr/local/bin/ato`) requires user consent (first-
  launch dialog or explicit menu action). No silent PATH modification.

## 6. Known Limitations

- **V1.1:** Windows and Linux desktop builds depend on GPUI's stability
  on those platforms. Zed's Windows port reached stable in early 2026;
  Linux is stable. Both are usable but less battle-tested than macOS.
- **V2.0:** UniFFI bridge adds serialization overhead for hot-path
  calls. Profile before optimising; most capsule lifecycle operations
  are IO-bound and unlikely to be bottlenecked by FFI.
- **V2.0:** Mobile WebView-in-WebView may require platform-specific
  workarounds for the custom protocol scheme (`capsule<partitionId>://`).
  iOS WKWebView supports custom schemes natively; Android requires
  `WebViewClient.shouldInterceptRequest`.

## 7. Roadmap

| Phase | Scope | Milestone |
|-------|-------|-----------|
| **V1.0** | macOS only. GPUI + Wry. Manual CLI install. | Launch demo |
| **V1.1** | macOS/Windows/Linux. App Bundle + symlink (B). Homebrew Cask (G). | Public release |
| **V1.2** | Windows MSI installer. Linux AppImage. | Platform parity |
| **V2.0** | `HostShell` trait extraction. capsule-core UniFFI bindings. | Architecture pivot |
| **V2.1** | iOS: SwiftUI shell + Wry guests + capsule-core FFI. | iOS beta |
| **V2.2** | Android: Compose shell + Wry guests + capsule-core FFI. | Android beta |
| **V3.0** | Web target (if GPUI/Wasm or Dioxus matures). | Evaluation only |

## References

- `apps/ato-desktop/xtask/src/main.rs` — macOS .app bundle builder
- `apps/ato-desktop/src/orchestrator.rs:545-570` — `resolve_ato_binary()` / `bundled_ato_binary()`
- `apps/ato-desktop/src/main.rs` — Desktop entry point
- `apps/ato-cli/src/main.rs` — CLI entry point
- [Glass-HQ/gpui](https://github.com/Glass-HQ/gpui) — GPUI standalone fork
- [gpui-mobile](https://github.com/itsbalamurali/gpui-mobile) — Community mobile port
- [Wry](https://github.com/tauri-apps/wry) — Cross-platform WebView library
- [Tauri 2.0](https://v2.tauri.app/) — Desktop + mobile framework
- [Zed Windows port](https://zed.dev/docs/windows) — GPUI DirectX 11 backend
