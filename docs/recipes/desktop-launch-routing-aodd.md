# Desktop launch routing AODD — Test Set A reach-rate measurement

**Branch:** `test/launch-routing-aodd` (worktree)
**Base:** `dev` @ `1758a981` (includes PR #251 ingress router + PR #252 sample recipe routing)
**Date:** 2026-05-24
**Time budget per app:** 20 min (used: ~5 min per app; root cause shared across all 5)

## Usecase

| Field | Value |
|---|---|
| `usecase` | Operator opens a recipe-backed app from Desktop natural launch and reaches **session-created** |
| `actor` | First-time operator using Desktop UX (MCP automation as click/keystroke stand-in; CLI is backend only) |
| `goal` | `capsule://…` → sample_recipe resolution → Review Before Launch → consent approved → session row in `ato ps --json` |
| `entry_point` | `ATO_HOME=/tmp/ato-launch-routing-aodd`, `ato-desktop --skip-onboarding`, Focus View |
| `out_of_scope` | CLI as primary driver; MCP automation gaps B3–B7 from prior AODD; recipe runtime quality |

## Headline result

| App | `ato app resolve` | Desktop NavigateToUrl | session-created reached |
|---|---|---|---|
| memos | ✅ `kind=sample_recipe` | ❌ preflight error → silent stall | ❌ |
| uptime-kuma | ✅ `kind=sample_recipe` | ❌ preflight error | ❌ |
| n8n | ✅ `kind=sample_recipe` | ❌ preflight error | ❌ |
| open-webui | ✅ `kind=sample_recipe` | ❌ preflight error | ❌ |
| excalidraw | ✅ `kind=sample_recipe` | ❌ preflight error (+ bad image pin downstream) | ❌ |

**Session-created reach rate: 0 / 5.**

Previous AODD (PR #249) was also 0/6. The improvement from #252 is real but invisible to a real
operator: the CLI surface resolves correctly, but the Desktop calls a different surface that
still misses the sample recipe catalog.

## Root cause: a second routing surface PR #252 didn't touch

PR #252 patched `crates/ato-cli/src/app_control/resolve.rs::normalize_handle` so that
`ato app resolve <handle>` consults `sample_recipes::resolve_sample_recipe_for_input` and
`resolve_sample_recipe_for_github` before falling back to GitHub or registry inference.

The Desktop's `Focus-mode NavigateToUrl` handler does **not** call `app resolve`. It calls
`ato internal preflight <handle>` to gate the consent screen. That subcommand was not updated
and still falls through to the GitHub-clone path inference (`external-capsules/github/<owner>/<repo>/capsule.toml`)
or rejects bare aliases as "registry handles".

The Desktop catches the preflight failure and logs `consent preflight unavailable for remote handle
— continuing with launch fallback`, but the fallback is effectively a no-op for the operator:
nothing else happens, no session appears, no error is shown.

### Evidence (single repro, identical across all 5 apps)

```text
# Desktop UX path (via MCP NavigateToUrl):
$ ato-desktop --skip-onboarding   # then MCP host_dispatch_action[NavigateToUrl url=capsule://github.com/usememos/memos]
INFO  focus dispatcher routes action action=NavigateToUrl
INFO  Focus-mode NavigateToUrl url=capsule://github.com/usememos/memos
WARN  consent preflight unavailable for remote handle — continuing with launch fallback
      handle="github.com/usememos/memos"
      error=ato internal preflight failed (exit status 2):
        preflight collection failed: manifest path does not exist:
        /tmp/ato-launch-routing-aodd/external-capsules/github/usememos/memos/capsule.toml
# … no further log activity. ato ps --json → []. podman ps → no ato containers.

# Equivalent CLI surfaces (inspector):
$ ato app resolve capsule://github.com/usememos/memos --json
{ "resolution": { "kind": "sample_recipe", "source": "sample_recipe",
                  "snapshot": { "resolved_path": "/tmp/.../sample-recipes/memos/capsule.toml" } } }

$ ato internal preflight capsule://github.com/usememos/memos
E999 preflight collection failed: manifest path does not exist:
     /tmp/ato-launch-routing-aodd/external-capsules/github/usememos/memos/capsule.toml

$ ato internal preflight memos
E999 preflight collection failed: unsupported preflight target 'memos':
     registry handles are not supported by side-effect-free preflight;
     install the capsule first, then run `--plan-only` against the resulting local path.
```

The bare-alias form (`capsule://memos`) produces a slightly different failure mode in the
Desktop — `WARN consent preflight failed — wizard shows error state` — but the user still
can't reach session-created. So both natural omnibar forms are blocked, and both Featured
Apps cards (memos, uptime-kuma, excalidraw) which dispatch the canonical GitHub form are
silently blocked too.

## What changed vs PR #249 baseline

- **Resolved (#252 worked at its layer)**: `ato app resolve` no longer falls back to generic
  GitHub/Go-source inference for the 5 catalog handles. The CLI surface returns the bundled
  recipe and its OCI runtime info.
- **Resolved (#251)**: out of scope here; ingress router landed but is not exercised by this
  slice because no session reaches the ingress layer.
- **Still blocked**: Desktop natural launch — preflight surface (a different binary subcommand)
  was not updated to consult the sample recipe catalog.

## Suggested next slice (B1/B2 follow-up)

1. **Patch `ato internal preflight`** to mirror the early-return in
   `app_control::resolve::normalize_handle`: call `resolve_sample_recipe_for_input` for bare
   aliases and `resolve_sample_recipe_for_github` for `capsule://github.com/<owner>/<repo>`
   inputs, and short-circuit to the bundled manifest path before falling through to the
   GitHub-clone / registry classifications.
2. **Make the launch-fallback path honest**: when preflight fails, the Desktop currently logs
   a WARN and goes silent. Either surface a visible error in the Focus View Control Bar, or
   make the fallback genuinely complete the launch from the resolver's snapshot. A silent
   stall is the worst UX of the three options.
3. **Acceptance test** in ato-cli: assert that
   `ato internal preflight capsule://github.com/usememos/memos` exits 0 and reports the
   sample recipe manifest path, with parallel assertions for `uptime-kuma`, `n8n`,
   `open-webui`, `excalidraw`.

These are the only changes required to take this AODD from 0/5 to a measurable Desktop
session-created rate. Recipe runtime quality (e.g. excalidraw's bad image tag, the n8n
encryption-key warning, `state.data` explicit-binding wiring through Desktop's consent flow)
remain separate follow-ups.

## Out of scope but worth filing separately

- **Recipe runtime — excalidraw bad image pin**: `samples/recipes/excalidraw/capsule.toml`
  pins `excalidraw/excalidraw:0.17.6`; Docker Hub only exposes `latest` and `sha-*` tags for
  that image. Image pull will fail even after the preflight fix lands. Separate PR territory.
- **Desktop consent flow — explicit state binding**: even when the resolver is reached
  end-to-end via CLI (`ato app session start capsule://github.com/usememos/memos`), the
  session fails with `state 'data' requires an explicit persistent binding before it can be
  attached`. The Desktop's consent flow is normally responsible for providing the binding;
  worth confirming once the preflight fix unblocks the natural path.
- **MCP automation gaps B3–B7** from PR #249: the user explicitly deferred these so they
  don't crowd out the user-visible launch routing fix. `approve_execution_plan_consent` still
  errors with `Discriminant(22) is not supported in Focus mode` — recorded here only as
  confirmation the gap persists, not as a request to fix it.

## Receipts

Per-app YAML receipts under `.tmp/aodd-receipts/launch-routing/` (gitignored — included by
reference for the human reviewer who runs this AODD locally):

- `memos.yaml` — primary evidence file (full transcript + cross-check CLI inspector)
- `uptime-kuma.yaml`, `n8n.yaml`, `open-webui.yaml`, `excalidraw.yaml` — pointers to memos.yaml
  for the shared root cause + per-app specifics

## Environment

```text
ATO_HOME=/tmp/ato-launch-routing-aodd                              # hermetic
ATO_DESKTOP_ASSETS_DIR=.../crates/ato-desktop/assets               # required to find Start window
PATH=.../target/release:$PATH                                       # ato 0.5.2, nacelle 0.5.2
ato-desktop 0.5.2 PID 62225, Focus View mode, --skip-onboarding
Automation socket: /tmp/ato-launch-routing-aodd/run/ato-desktop-62225.sock
podman applehv machine running, 5 OCI images pre-pulled
  (memos, uptime-kuma, n8n, open-webui present; excalidraw:0.17.6 unavailable — recipe runtime issue)
```
