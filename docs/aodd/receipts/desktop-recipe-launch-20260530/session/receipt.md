# Desktop Recipe Launch Matrix — Session Receipt

Date: 2026-05-30
Tester: @koh
Repo: ato
Branch: dev (a1c0c516, Wed May 27 2026)
Issue: #369

## Build Metadata

| Component | Version | Source |
|---|---|---|
| ato-desktop | 0.5.5 (debug) | crates/ato-desktop/target/debug/ato-desktop.exe |
| ato-desktop-mcp | 0.5.5 (debug) | crates/ato-desktop/target/debug/ato-desktop-mcp.exe |
| ato CLI | 0.5.4 | C:\Program Files\Ato\bin\ato.exe |
| ato CLI (debug) | 0.5.5 | crates/ato-cli/target/debug/ato.exe |

## Environment

| Property | Value |
|---|---|
| OS | Windows 11 (10.0.26100) |
| Shell | PowerShell 7.5.0 |
| Containers | Docker Desktop (dockerd) + Podman v5 (RedHat) |
| Desktop config | focus_view_enabled: true, capsule_open_mode: window, startup_surface: start |
| ATO_HOME | C:\Users\koh\.ato |
| MCP socket | \\.\pipe\ato-desktop-10760 |
| Desktop logs | C:\Users\koh\.ato\logs\ato-desktop.log.2026-05-30 |

## Test Method

1. Build ato-desktop + ato-desktop-mcp from `crates/ato-desktop/` (debug)
2. Start ato-desktop, confirm MCP socket listening
3. Use `ato-desktop-mcp` as stdio MCP tool server; invoke `host_dispatch_action(NavigateToUrl...)` for each recipe
4. Check Desktop state via `auth_status`, `browser_tabs`, `browser_snapshot`
5. Log analysis from `~/.ato/logs/ato-desktop.log.2026-05-30`
6. CLI preflight verification: `ato run --plan-only samples/recipes/<slug> --yes` (plan-only bypasses Docker pull)

## Results Summary

### Matrix Overview

| Slug | Tier | Preflight (CLI) | Desktop Launch | Primary Blocker |
|---|---|---|---|---|
| memos | A | BLOCKED | — | state binding: Unix path /var/opt/memos |
| uptime-kuma | A | BLOCKED | — | state binding: Unix path /app/data |
| pocketbase | A | BLOCKED | — | state binding: Unix path /pb_data |
| homepage | A | BLOCKED | — | state binding: Unix paths /app/config, /app/public/icons |
| blinko | A | BLOCKED | — | state binding: Unix paths (postgres + app) |
| linkwarden | A | BLOCKED | — | state binding: Unix paths (3 services) |
| langflow | A | BLOCKED | — | state binding: Unix paths + Windows shell |
| excalidraw | B | PASS | FAIL | podman DNS + Desktop no WebView pane |
| n8n | B | BLOCKED | — | state binding: Unix path /home/node/.n8n |
| affine | B | BLOCKED | — | state binding: Unix paths (3 services) |
| open-webui | B | BLOCKED | — | state binding: Unix path /app/backend/data |
| pgweb | B | PASS | FAIL | podman DNS + Desktop no WebView pane |
| adminer | B | PASS | FAIL | podman DNS + Desktop no WebView pane |
| shiori | B | BLOCKED | — | state binding: Unix path /shiori |
| filebrowser | B | BLOCKED | — | state binding: Unix paths /database, /srv |
| dify | B | BLOCKED | — | state binding: Unix paths (6+ services) |

### Tier Summary

- **Tier A**: 7/7 BLOCKED — all hit state binding Unix path validation
- **Tier B**: 3/9 PASS preflight, 0/9 PASS Desktop launch
  - 3 PASS preflight but fail runtime (podman DNS + Desktop no WebView pane)
  - 6 BLOCKED at preflight (state bindings)

## Root Cause Analysis

### Blocker 1: State binding Unix path validation on Windows
**Scope**: ALL recipes with `[[state_bindings]]` (10/16)
**File**: `capsule::manifest` validator
**Message**: `"Invalid state binding for service 'main': target '...' must be an absolute path"`
**Mechanism**: The validator checks if the target path starts with `/` for absolute-pathness. On Windows, Unix paths like `/var/opt/memos` are syntactically valid but semantically wrong. The validator should accept Windows-style absolute paths (`C:\...`) OR the state binding system should provide a platform-agnostic mount mechanism.
**Fix difficulty**: Medium — requires manifest schema + validator change to support platform-specific path formats.

### Blocker 2: podman DNS resolution failure on Windows
**Scope**: ALL OCI container capsules (6/16 that pass preflight)
**Tool**: podman v5 (RedHat) used by `ato` CLI as container runtime
**Error**: `dial tcp: lookup registry-1.docker.io: Temporary failure in name resolution`
**Cause**: RedHat Podman on Windows runs in its own VM; DNS configuration doesn't propagate from Docker Desktop's WSL2-based networking. Docker Desktop (dockerd) works fine.
**Workaround**: Configure ato to use Docker CLI instead of podman, or fix podman DNS in the podman machine VM.
**Fix difficulty**: Low (config change) — but needs product decision on container runtime strategy for Windows.

### Blocker 3: Desktop Focus-mode no capsule WebView pane
**Scope**: ALL Desktop-initiated capsule launches (even if CLI preflight passes)
**Component**: `ato_desktop::window::focus_dispatcher`
**Log pattern**: `"ForceApprovePending: consuming pending target route=..."` → `"open_boot_window"` + `"start_boot_launch"` → no WebView pane created → `browser_tabs` returns `{"panes":[]}`
**Cause**: The Focus-mode dispatcher consumes the `PendingLaunchTarget`, opens a boot window (possibly offscreen or in headless rendering mode), and calls `start_boot_launch`. But the capsule WebView pane is never created — likely because `CapsuleAppWindow` creation is conditioned on running `CapsuleSessionState::Running` callback that creates the pane, which never fires in Focus mode due to missing orchestrator wiring.
**Fix difficulty**: Medium — requires tracing why the session state transition to Running doesn't trigger `CapsuleAppWindow` creation in Focus mode.

## Attestations
- [x] Desktop launch initiated from Ato Desktop UI (via host_dispatch_action NavigateToUrl)
- [x] CLI plan-only used as supplementary data source for preflight verification
- [x] Matrix CSV saved to docs/aodd/desktop_recipe_launch_matrix.csv
- [x] Distinct blockers identified with error messages and fix guidance
