# Desktop Real-App AODD — does ato-desktop actually manage researched OCI apps?

**Scenario**: `desktop-real-app-aodd`
**Date**: 2026-05-24
**Branch**: `test/desktop-real-app-aodd`
**Bins under test**:
- `crates/ato-desktop/target/debug/ato-desktop` (v0.5.2)
- `crates/ato-desktop/target/debug/ato-desktop-mcp` (v0.5.2)
- `target/debug/ato` (v0.5.2) — backend only

This AODD evaluates whether the **real ato-desktop binary**, driven through its
**MCP automation socket** (`ato-desktop-mcp` over stdio), can launch, surface,
open, and stop researched OCI apps the way a first-time operator would. CLI is
used as backend only (per task brief — `CLIはバックエンドとしてのみ使う`).

## Summary table

| App         | Source type      | Desktop launch | Desktop discovery | Endpoint   | Desktop stop | Cleanup | Status  | Notes |
|-------------|------------------|----------------|-------------------|------------|--------------|---------|---------|-------|
| memos       | recipe (samples) | blocked        | blocked           | blocked    | n/a          | pass    | blocked | Generic github auto-detect picked Go runtime; `stat main.go` failed |
| uptime-kuma | recipe (samples) | blocked        | blocked           | blocked    | n/a          | pass    | blocked | AI-inferred manifest halted at `ATO_ERR_MANUAL_INTERVENTION_REQUIRED` (env vars) |
| n8n         | recipe (samples) | blocked        | blocked           | blocked    | n/a          | pass    | blocked | Same generic github auto-detect path; no session |
| dify        | recipe (samples) | blocked        | blocked           | blocked    | n/a          | pass    | blocked | Same; multi-service stop verification not reached |
| blinko      | (no recipe)      | blocked        | blocked           | blocked    | n/a          | pass    | blocked | Specified for docker-run-script import; no Desktop UX surface for that today |
| affine      | (no recipe)      | blocked        | blocked           | blocked    | n/a          | pass    | blocked | 🚨 Featured App `capsule://affine` returns `unsupported handle 'affine'`; github fallback fails the same way as others |

**Strong-pass criteria from the brief were NOT met.** Zero apps reached the
session-created stage through the operator-natural Desktop UX. The pattern is
consistent enough that this report's value is in characterising the gap, not
in counting passes.

## Environment

- macOS Darwin 24.6.0, arm64
- Podman 5.7.1 (machine running, `applehv`, 4 CPU / 8 GiB)
- `ATO_HOME=/tmp/ato-desktop-real-apps` (fresh, hermetic, short path to avoid
  macOS Unix-socket length limits)
- `ATO_DESKTOP_ASSETS_DIR=…/crates/ato-desktop/assets` (required — see
  Desktop-specific findings below)
- `ato-desktop --skip-onboarding` (to bypass first-run guidance window)
- Desktop launched in background; MCP commands driven via
  `ato-desktop-mcp` subprocess receiving JSON-RPC on stdin.

## App-by-app results

Each app has a full per-app receipt under
`.tmp/aodd-receipts/desktop-real-apps/<app>.yaml`. Highlights:

### memos (control, single-service)
- Operator typed canonical `capsule://github.com/usememos/memos` into the
  omnibar via `host_dispatch_action[NavigateToUrl]`.
- "Review Before Launch" consent dialog appeared.
- `host_dispatch_action[ForceApprovePending]` approved.
- Backend (CLI invoked by Desktop) auto-detected the repo as a Go source
  and ran a smoke test which failed: `stat main.go: no such file or directory`.
- No session created; no panes; clean.
- Receipt: [.tmp/aodd-receipts/desktop-real-apps/memos.yaml](../../.tmp/aodd-receipts/desktop-real-apps/memos.yaml)

### uptime-kuma (control, redirect/login)
- Same Desktop UX entry, same consent + approve.
- Auto-detect went down the Compose/NPM path, AI-inferred a draft
  `capsule.toml`, then halted with `ATO_ERR_MANUAL_INTERVENTION_REQUIRED`
  asking the user to set `NODE_ENV`, `POSTGRES_PASSWORD`, `MYSQL_ROOT_PASSWORD`
  and rerun via CLI.
- Receipt: [.tmp/aodd-receipts/desktop-real-apps/uptime-kuma.yaml](../../.tmp/aodd-receipts/desktop-real-apps/uptime-kuma.yaml)

### n8n (control, workflow app)
- Same Desktop UX entry; same failure mode; no session.
- Receipt: [.tmp/aodd-receipts/desktop-real-apps/n8n.yaml](../../.tmp/aodd-receipts/desktop-real-apps/n8n.yaml)

### dify (multi-service candidate)
- Same Desktop UX entry; same failure mode; no session.
- Multi-service stop verification not reached. Spec's "≥1 multi-service app
  stopped cleanly from Desktop" criterion remains unverified.
- Receipt: [.tmp/aodd-receipts/desktop-real-apps/dify.yaml](../../.tmp/aodd-receipts/desktop-real-apps/dify.yaml)

### blinko (attempt — no recipe)
- Confirmed no recipe under `samples/recipes/blinko`.
- Operator tried the canonical github handle directly; same failure mode.
- Receipt: [.tmp/aodd-receipts/desktop-real-apps/blinko.yaml](../../.tmp/aodd-receipts/desktop-real-apps/blinko.yaml)

### affine (attempt — no recipe, BUT a Featured App in Start window)
- Operator saw AFFiNE in the Featured Apps row of the Start window.
  Confirmed via source: `assets/system/ato-start/src/components/FeaturedApps.astro`
  uses `data-handle="capsule://<bare-name>"`.
- Tried `capsule://affine` via `host_dispatch_action[NavigateToUrl]` — the
  consent dialog appeared, then approve failed with
  `E999 Configuration error: unsupported handle 'affine'`.
- Tried `capsule://github.com/toeverything/AFFiNE` fallback; same generic
  failure as memos/n8n/etc.
- Receipt: [.tmp/aodd-receipts/desktop-real-apps/affine.yaml](../../.tmp/aodd-receipts/desktop-real-apps/affine.yaml)

## Desktop-specific findings

These are the most actionable items the AODD surfaced — they apply to every
app and to most future AODD runs against the Desktop shell.

### 🚨 D1. Featured Apps Launch buttons are non-functional

The Start window (`assets/system/ato-start/`) advertises three Featured Apps
with prominent **Launch** buttons:

- AFFiNE — `data-handle="capsule://affine"`
- Open WebUI — `data-handle="capsule://open-webui"`
- Excalidraw — `data-handle="capsule://excalidraw"`

All three handles resolve through `ato app resolve <name> --json` which
returns `E999 Configuration error: unsupported handle '<name>'` — i.e. no
resolver is wired for bare-name capsules. The Launch buttons are a
**user-facing trust break**: the launcher displays a clickable affordance
for apps that cannot be launched.

This was unambiguously reproduced with `capsule://affine`; the same
resolution path is used for `open-webui` and `excalidraw`.

### D2. `samples/recipes/<name>` has no operator-natural Desktop UX path

The Desktop has three operator-visible launch surfaces:
1. **Omnibar / NavigateToUrl** — accepts `capsule://github.com/<owner>/<repo>`
   and `capsule://<host>/<owner>/<name>` URLs.
2. **Store window** (`OpenStoreWindow`) — backed by a local registry server
   at `http://127.0.0.1:8787` (`ato registry serve`).
3. **OpenGithubRunWindow** — accepts a github URL.

None of these can launch a curated `samples/recipes/<name>` recipe without
operator-invisible CLI setup. Specifically:

- The omnibar's `capsule://github.com/<canonical>` form falls through to
  generic source auto-detection. For repos whose upstream layout is not an
  ato capsule (i.e. all six tested apps), this either picks the wrong
  runtime (memos → Go → `stat main.go`) or AI-infers a `capsule.toml` that
  needs manual env setup and tells the user to **drop to CLI**.
- The Store window requires `ato registry serve` to be running on 8787 with
  published `.capsule` artifacts. Neither setup step is reachable through
  the Desktop UX.
- `OpenGithubRunWindow` shares the omnibar's resolution path.

**There is no Desktop UX path that turns `samples/recipes/memos/capsule.toml`
into a running session.** This is the central AODD finding.

### D3. `NavigateToUrl` rejects local filesystem paths

The omnibar input field advertises `Run by name, local path, capsule URL,
or command` but the MCP entry point `host_dispatch_action[NavigateToUrl]`
runs URL parsing and rejects absolute paths with `relative URL without a
base`. Either:
- the omnibar text input does extra normalisation that the MCP path skips,
  or
- the placeholder text overpromises.

Either way, the MCP / omnibar surface for "Run a local recipe directory"
does not work end-to-end.

### D4. `approve_execution_plan_consent` MCP tool fails in Focus mode

The dedicated MCP tool for the very dialog blocking the operator returns
`automation command Discriminant(22) is not supported in Focus mode (no
WebView pane)`. The only working path is `host_dispatch_action[ForceApprovePending]`
— a generic GPUI action. The two paths cover the same user intent but only
one works in the default (Focus) mode, which is a smell worth fixing.

### D5. Start window / launcher is unautomatable via `browser_*`

`browser_snapshot`, `browser_click`, etc. operate on registered WebView
panes. The Start window and Store window are separate GPUI windows with
their own WebViews that are NOT registered as panes. Consequence: an
operator agent cannot read the Featured Apps content, cannot click a Launch
button, cannot search the store. The only automation reach is via
`host_take_screenshot`, `host_dispatch_action`, and (with Accessibility
permission) `host_press_key`.

### D6. No `list_sessions` MCP tool

`AutomationCommand::ListSessions` exists in the enum but is not exposed in
the MCP `tools/list` output. Without it, "Desktop discovery" can only be
verified visually (screenshot) or by falling back to `ato ps --json` —
which the operator persona is explicitly not supposed to use. This makes
the spec's `desktop_discovery=pass` classification effectively unreachable
through MCP in any future AODD run, not just this one.

### D7. `ATO_DESKTOP_ASSETS_DIR` is required when launching from outside the crate root

`ato-desktop` panics at startup with `failed to resolve ato-desktop assets
directory` when its cwd is anywhere other than the crate root. This is
ergonomic friction for AODD scripting; documented behaviour, but the panic
message could direct the user to `--assets-dir` / `ATO_DESKTOP_ASSETS_DIR`
in a less alarming way.

## Runtime / recipe-specific findings

### R1. github auto-resolution treats non-capsule repos as generic sources

When the omnibar receives `capsule://github.com/<repo>`, the resolver
clones the repo and runs source-detection. For repos that are not shaped
as ato capsules:
- **Go-shaped detection** runs `stat main.go` → fails when the repo's Go
  code is under a subdirectory (memos).
- **Compose-shaped detection** invokes the AI inference pipeline which
  produces a draft `capsule.toml` requiring manual env intervention (uptime-kuma).
- **Other shapes** likely fall through similarly.

This is intentional behaviour, but for curated `samples/recipes/<name>`
apps it produces a **worse** launch path than just shipping a manifest.
The operator typing `capsule://github.com/usememos/memos` would benefit
far more from "we know about memos — here's our recipe" than from "we'll
try to infer something".

### R2. n8n recipe ships a hardcoded encryption key

`samples/recipes/n8n/capsule.toml` has
`N8N_ENCRYPTION_KEY = "changeme-demo-encryption-key"`. The recipe's own
description acknowledges this ("NOTE: single-user / demo mode — not
recommended for production"). Unrelated to the Desktop UX gap but worth
noting; the AODD didn't reach the stage where this leaks into a session
record.

### R3. dify recipe acknowledges B1 / B4 explicitly

`samples/recipes/dify/capsule.toml` has comments noting:
- B1 (ingress / `CONSOLE_API_URL`) remains
- `SECRET_KEY = "sk-demo-changeme-not-for-production-use-only"` (demo)
- redis/api/worker share a `difyai123456` password (acceptable for demo)

The AODD didn't reach the stage where the Dify UI's relative-URL routing
would have been validated through Desktop's WebView.

## Blockers and follow-up issues

| # | Title | Severity | Owner-suggestion |
|---|---|---|---|
| B1 | Featured Apps Launch buttons (capsule://affine, capsule://open-webui, capsule://excalidraw) are non-functional | 🚨 trust-breaking, user-facing | desktop / launcher team |
| B2 | `samples/recipes/<name>` has no Desktop UX path; omnibar/store/github paths all fail or require CLI | 🚨 blocks every "real OCI app from catalog" flow | desktop + cli + registry |
| B3 | `NavigateToUrl` MCP rejects local filesystem paths despite UI placeholder advertising them | 🟡 inconsistency | desktop automation |
| B4 | `approve_execution_plan_consent` fails in Focus mode; only `ForceApprovePending` works | 🟡 MCP API duplication smell | desktop automation |
| B5 | No `list_sessions` MCP tool; Desktop discovery is only verifiable by screenshot | 🟡 blocks future AODDs from claiming `desktop_discovery=pass` | desktop automation |
| B6 | Start window / Store window WebViews are not registered panes; unautomatable via `browser_*` | 🟢 architectural | desktop |
| B7 | `ato-desktop` panic message when assets dir missing should be friendlier | 🟢 ergonomics | desktop |

## What was not tested

- **Backend recipe sanity via CLI.** The brief restricted CLI to "backend
  only"; I did not run `ato run samples/recipes/<name>` to confirm the
  recipes themselves work. Other docs
  (`docs/recipes/initial-batch-1.md`, `docs/recipes/exhaustive-aodd-matrix.md`)
  already cover that. If the recipes were broken the failure mode would be
  obvious in the Desktop launch logs; here, Desktop never got far enough
  to invoke the recipe, so this run does not regress or confirm recipe
  health.
- **Open WebUI / Excalidraw Featured Apps.** I only confirmed AFFiNE's
  Featured App handle fails. The other two share the bare-name handle
  pattern and the same resolver, so the conclusion extends, but I did
  not literally hit each URL.
- **`ato registry serve` + Store window flow.** Bringing the local
  registry up and publishing the curated recipes would in principle make
  the Store window functional. I did not test this because it is a
  CLI-driven setup that an operator wouldn't discover from the Desktop
  UX. If `ato init` / first-run ever auto-starts the local registry with
  the bundled recipes, that becomes the right path to validate.
- **Permission prompts.** macOS Screen Recording and Accessibility
  permissions were already granted to the terminal running the MCP, so
  the first-invocation prompts did not gate this run. AODDs run from a
  fresh machine would need to manually grant both before MCP screenshots
  / keystrokes work.
- **Behaviour when Desktop is running and the user is signed-in.** All
  runs were unsigned-in; `auth_status` returned
  `{"signed_in":false,"api_base_url":"https://api.ato.run","account_hint":null}`.
  Sign-in paths (My Dock, cloud-published capsules) may unlock different
  Desktop UX behaviour for installed apps.
