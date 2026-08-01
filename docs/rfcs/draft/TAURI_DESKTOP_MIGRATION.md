---
title: "Tauri Desktop Migration"
status: draft
date: 2026-08-02
author: "@Koh0920"
related:
  - "accepted/ATO_CLI_SPEC.md"
  - "draft/ADR-008-desktop-ipc-boundary-principal-visibility.md"
  - "draft/SURFACE_CLOSE_SEMANTICS.md"
---

# Tauri Desktop Migration

## Context

The GPUI + Wry Desktop remains the reference implementation for installed-app,
revision, and session lifecycle behavior. The Tauri migration must not fork
capsule execution or introduce a second owner of `~/.ato` state.

The original `feat/tauri-migration` branch established useful boundaries but
predated the current lifecycle model. The migration is therefore reconstructed
from current `main`; the old branch is a donor, not a branch to rebase.

## Decision

1. `ato` CLI remains the sole capsule execution and lifecycle owner.
2. `runner` contains host-independent process supervision and retention policy.
3. `protocol` contains transport-neutral DTO and intent vocabulary.
4. `desktop-tauri` is a composition root and native adapter only.
5. `ato-pwa` produces a separate offline Launcher entrypoint. The browser PWA
   and Desktop Launcher do not share routing, auth state, or window lifecycle.
6. The GPUI Desktop is retained until parity is demonstrated.

```mermaid
flowchart TD
    L["Local Launcher<br/>ato-pwa desktop entrypoint"] -->|"typed invoke"| T["desktop-tauri adapter"]
    H["Remote Home<br/>https://app.ato.run"] -->|"validated ato URI"| T
    T --> R["runner supervision"]
    R --> C["ato CLI lifecycle owner"]
    T --> P["protocol DTOs"]
    R --> P
```

Dependency direction is inward: Tauri depends on `runner` and `protocol`;
neither crate depends on Tauri, GPUI, Wry, React, or WebView types.

## Window trust model

| Window | Content | Native capability | Allowed top-level navigation |
|---|---|---|---|
| `main` | bundled Launcher assets | yes | local asset origin only |
| `home` | `https://app.ato.run` | no | exact trusted HTTPS origin only |
| `app:*` | capsule session surface | no | future session-specific policy |

The `main` capability is defense in depth, not the sole authorization check.
Every custom command verifies the caller label. Remote Home communicates using
an `ato://` navigation intercepted and classified by Rust; JavaScript cannot
submit a pre-classified privileged enum.

External HTTP(S) navigations are cancelled in the WebView and sent to the OS
browser. `atoview://` is unavailable as a top-level navigation. Its native HTTP
client accepts only one-label `*.app.ato.run` session hosts over HTTPS and
revalidates every redirect target.

## Frontend materialization

`ato-pwa` owns the source and produces `dist-desktop/index.html` through
`npm run build:desktop`. `desktop-tauri/build.rs` runs that declared build and
copies the result into its ignored `frontend/` materialization directory before
Tauri embeds the assets.

The source checkout therefore contains declarations and source only; generated
assets are materialized at build time. `ATO_DESKTOP_PWA_DIR` may point to a
non-sibling checkout. `ATO_DESKTOP_SKIP_FRONTEND_BUILD=1` is an explicit opt-in
for release jobs that already staged a verified frontend.

## Testing strategy

- `protocol`: DTO serialization and intent parser unit tests.
- `runner`: fake-host supervision tests plus native process-group teardown.
- `desktop-tauri`: caller-label, exact-origin, URI fail-closed, redirect, and
  proxy host-allowlist unit tests.
- `ato-pwa`: TypeScript checking and a production Desktop build.
- Integration: build with no pre-existing `desktop-tauri/frontend` directory.
- Parity gate: existing GPUI Desktop test result must remain unchanged.

## Consequences

### Positive

- Framework and UI migration cannot change capsule execution semantics.
- Remote web content has no direct native command surface.
- The Launcher starts offline and can evolve independently from Home.
- Existing GPUI lifecycle behavior remains available for parity comparison.

### Cost

- The Desktop build currently requires sibling `ato` and `ato-pwa` checkouts or
  an explicit `ATO_DESKTOP_PWA_DIR`.
- Installed Apps commands, session control, app windows, and release bundling
  remain follow-up phases.

## Phase 0/1 acceptance

- [x] Reconstruct integration branches from current `main`.
- [x] Restore `runner`, shared protocol DTOs, and `desktop-tauri` without
  changing the GPUI Desktop implementation.
- [x] Produce a separate offline Desktop Launcher build.
- [x] Separate local Launcher and remote Home windows.
- [x] Deny native commands from non-`main` callers.
- [x] Pin Launcher and Home top-level navigation origins.
- [x] Revalidate `atoview://` redirects.
- [ ] Add Installed Apps read model and session control through CLI contracts.
- [ ] Add `app:*` windows and lifecycle parity.
- [ ] Add multi-platform bundle/release CI.
