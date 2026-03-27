# ato-cli

[![CI](https://github.com/Koh0920/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=dev)](https://github.com/Koh0920/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/Koh0920/ato-cli)](https://github.com/Koh0920/ato-cli/releases)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org/)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-6f42c1)
[![Issues](https://img.shields.io/github/issues/Koh0920/ato-cli)](https://github.com/Koh0920/ato-cli/issues)
[![Last Commit](https://img.shields.io/github/last-commit/Koh0920/ato-cli/dev)](https://github.com/Koh0920/ato-cli/commits/dev)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/Koh0920/ato-cli)

English | [日本語](README_JA.md)

`ato` is a meta-CLI that interprets `ato.lock.json` as the canonical command-entry input, while still supporting compatibility and source-only bootstrap flows to execute, distribute, and install capsules.
It is designed around a Zero-Trust / fail-closed model: normal runs stay quiet, while consent prompts and policy violations are surfaced explicitly.

`ato init` now materializes a durable `ato.lock.json` baseline plus workspace-local `.ato/` inference state. The legacy prompt/manual manifest helpers remain available through `ato init --legacy prompt` and `ato init --legacy manual`.

For a single-file consolidated specification of current behavior, see `docs/current-spec.md`.

## Lock-Native Input Model

- `ato.lock.json` is authoritative when present.
- Compatibility and bootstrap inputs remain available for projects that have not yet been initialized into the canonical lock flow.
- `capsule.lock.json` is legacy compatibility data and is not accepted as a standalone command-entry input.
- If canonical and compatibility inputs coexist, `ato.lock.json` wins and compatibility inputs are advisory only.
- `ato inspect lock`, `ato run`, and `ato build` resolve through this precedence.

## Key Commands

```bash
ato run [path|publisher/slug|github.com/owner/repo] [--registry <url>]
ato ps
ato close --id <capsule-id> | --name <name> [--all] [--force]
ato logs --id <capsule-id> [--follow]
ato install <publisher/slug> [--registry <url>]
ato install --from-gh-repo <github.com/owner/repo>
ato build [dir] [--strict-v3] [--force-large-payload]
ato publish [--registry <url>] [--artifact <file.capsule>] [--scoped-id <publisher/slug>] [--allow-existing] [--prepare] [--build] [--deploy] [--legacy-full-publish] [--fix] [--no-tui] [--force-large-payload]
ato publish --dry-run
ato publish --ci
ato init [path] [--yes]
ato gen-ci
ato inspect requirements <path|publisher/slug> --json [--registry <url>]
ato inspect lock [path] [--json]
ato inspect preview [path] [--json]
ato inspect diagnostics [path] [--json]
ato inspect remediation [path] [--json]
ato search [query]
ato source sync-status --source-id <id> --sync-run-id <id> [--registry <url>]
ato source rebuild --source-id <id> [--ref <branch|tag|sha>] [--wait] [--registry <url>]
ato config engine install --engine nacelle [--version <ver>]
ato setup --engine nacelle [--version <ver>] # compatibility command (deprecated)
ato registry serve --host 127.0.0.1 --port 18787 [--auth-token <token>]
```

## Native Delivery (Experimental)

- Primary product surface stays `ato build`, `ato publish`, and `ato install`.
- For the current Tauri darwin/arm64 PoC, native delivery metadata is authored in the project manifest, but `ato.lock.json` is authoritative when present. A default target with `driver = "native"` and a `.app` `entrypoint` is enough for native build detection.
- Source projects must declare native delivery metadata in the project manifest. `ato.delivery.toml` is no longer accepted as authored input; `ato` stages internal compatibility metadata into the artifact instead.
- Native install JSON exposes `local_derivation` and `projection` envelopes. For this contract generation, `schema_version = "0.1"` is the stable machine-readable version for fetch/finalize/project/unproject/install metadata.
- `fetch`, `finalize`, `project`, and `unproject` remain advanced/debug surfaces. Most users should stay on the integrated `build` / `publish` / `install` flow.
- Local finalize is currently fail-closed and limited to macOS darwin/arm64 with `codesign`.
- Projection currently creates a macOS `~/Applications` symlink on macOS hosts, and a Linux `.desktop` launcher plus `~/.local/bin` symlink on Linux hosts.

### Native Delivery contract (current canonical form)

For native delivery authoring, the current canonical project manifest contract is:

```toml
schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
```

For this `.app`-entrypoint form, `ato` derives the current PoC defaults internally:

- `artifact.framework = "tauri"`
- `artifact.stage = "unsigned"`
- `artifact.target = "darwin/arm64"`
- `artifact.input = <targets.<default>.entrypoint>`
- `finalize.tool = "codesign"`
- `finalize.args = ["--deep", "--force", "--sign", "-", <artifact.input>]`

If the native target is command-driven (`entrypoint = "sh"` plus `cmd = [...]`), then the build still needs explicit delivery metadata today. The only supported source location for that metadata is inline in the project manifest as `[artifact]` + `[finalize]`. Source-side `ato.delivery.toml` is rejected fail-closed. Partial inline metadata is also rejected fail-closed.

### Compatibility sidecar and artifact metadata flow

- `ato.delivery.toml` is **not** a supported source-project manifest. It persists only as staged artifact metadata for local finalize/install/project flows.
- Build always stages `ato.delivery.toml` into the artifact payload, even when only the project manifest was authored. This keeps the artifact self-describing for local finalize/install without requiring the original source tree.
- `ato install`, `ato finalize`, and `ato project` read the staged artifact metadata plus `local-derivation.json`; they do not require the source checkout's sidecar to be present later.
- Current policy: source projects must use the project manifest; `ato.delivery.toml` remains only as artifact-internal compatibility metadata.

### Stable vs experimental machine-readable contract

For the current `schema_version = "0.1"` generation, the repo documents and test-guards the presence of these machine-readable fields:

- `fetch.json`: `schema_version`, `scoped_id`, `version`, `registry`, `parent_digest`
- build JSON: `build_strategy = "native-delivery"`, `schema_version`, `target`, `derived_from`
- finalize JSON: `schema_version`, `derived_app_path`, `provenance_path`, `parent_digest`, `derived_digest`
- `local-derivation.json`: `schema_version`, `parent_digest`, `derived_digest`, `framework`, `target`, `finalize_tool`, `finalized_at`
- project JSON: `schema_version`, `projection_id`, `metadata_path`, `projected_path`, `derived_app_path`, `parent_digest`, `derived_digest`, `state`
- unproject JSON: `schema_version`, `projection_id`, `metadata_path`, `projected_path`, `removed_projected_path`, `removed_metadata`, `state_before`
- install JSON: `install_kind`, `launchable`, `local_derivation`, `projection`
  - `install_kind = "NativeRequiresLocalDerivation"` means install succeeded, but the launchable path is the locally derived app bundle, not the stored `.capsule`
  - `launchable.path` is the path a caller should use to run
  - `local_derivation.provenance_path`, `parent_digest`, and `derived_digest` are the stable linkage between fetch/finalize/project/install
  - `projection.metadata_path` is the stable recovery handle for `ato unproject` and for launcher state inspection

These guarantees are intentionally narrow: additive fields may still appear, but removing or renaming the documented fields should be treated as a schema-version change.

Still experimental:

- exact on-disk directory layout under `~/.ato/fetches`, `~/.ato/apps`, and `~/.ato/native-delivery/projections`
- any future additional keys beyond the stable fields above
- advanced/debug command UX (`fetch`, `finalize`, `project`, `unproject`) beyond their current `schema_version = "0.1"` JSON envelopes
- host/tool support beyond the current macOS darwin/arm64 + `codesign` PoC

### Migration path

1. **Current**: the project manifest is the only supported authored source manifest for native delivery.
2. **Current**: `ato` still stages `ato.delivery.toml` into artifacts as internal metadata for local finalize/install/project flows.
3. **Later**: internal artifact metadata can be abstracted away from the user-facing sidecar name, while preserving the `schema_version = "0.1"` JSON/provenance contract for automation.

## Quick Start (Local)

```bash
# build
cargo build -p ato-cli

# install nacelle engine if not installed (recommended)
./target/debug/ato config engine install --engine nacelle

# compatibility: setup subcommand
./target/debug/ato setup --engine nacelle

# run
./target/debug/ato run .

# hot reload during development
./target/debug/ato run . --watch

# background process management
./target/debug/ato run . --background
./target/debug/ato ps
./target/debug/ato logs --id <capsule-id> --follow
./target/debug/ato close --id <capsule-id>
```

## Publish Model (Official / Dock / Custom)

- Official registries (`https://api.ato.run`, `https://staging.api.ato.run`):
  `ato publish` is CI-first (OIDC). Direct local uploads are not allowed.
  Default execution is Publish only (handoff/diagnostics).
- Personal Dock (default when logged in and no registry is specified):
  `ato publish` resolves the target from `ato login` and uploads directly to `https://api.ato.run/v1/local/capsules/...`.
  `--artifact` is recommended to avoid re-packing, and `--scoped-id` is auto-filled as `<handle>/<slug>`.
  `/d/<handle>` is a public UI page only and is no longer a registry URL.
- Custom/private registries (any other `--registry`):
  `ato publish --registry ...` performs direct uploads. `--artifact` is recommended to avoid re-packing.
  `--artifact` supports standalone artifact flow (no local project manifest required).
  `--allow-existing` is available only when the final Publish stage is selected.

`ato publish` uses the shared producer pipeline:
`Prepare -> Build -> Verify -> Install -> Dry-run -> Publish`

- Default (official registries): start and stop at Publish.
- Default (private/local registries): start at Prepare and run through Publish.
- `--prepare`, `--build`, and `--deploy` are stop points, not free-form phase toggles.
- `--prepare` stops after Prepare.
- `--build` stops after Verify. With source input, this means build then verify; with `--artifact`, it becomes Verify only.
- `--deploy` stops after Publish.
- `--artifact` changes the start phase to Verify.
- `official + --deploy` remains Publish only (handoff, no local upload).
- `private/local + --deploy` can auto-resolve earlier phases from source input, or run `Verify -> Publish` when `--artifact` is provided.
- `--artifact --prepare` is invalid because the start phase would be after the selected stop point.
- `--legacy-full-publish` (official only) temporarily restores the legacy default behavior, is deprecated, and is scheduled for removal in the next major release.
- `--ci` / `--dry-run` cannot be combined with phase flags.

Implementation note during migration:

- phase selection, stop-point validation, and phase ordering are already owned by the application producer pipeline
- the current CLI entry routes through `cli::dispatch::publish`, which hosts the phase runner wiring for publish
- `application::pipeline::phases::publish` owns the wrapper APIs for private and official publish execution, and private remote uploads now flow through `DestinationPort`
- build-backed private publish now resolves source vs artifact input in `application::pipeline::phases::publish` before handing off to that same upload boundary

Official registry helpers:

- `ato gen-ci` generates the fixed GitHub Actions workflow for OIDC publish.
- `ato publish --fix` applies the official workflow fix once, then reruns diagnostics.
- `ato publish --no-tui` disables the interactive handoff UI and prints CI guidance directly.

### Migration Notes

- `ato publish --build` now stops after Verify, not immediately after Build.
- `ato run --skill` and `ato run --from-skill` have been removed.

## Dock-first Flow (Personal Dock)

The Dock-first path uses existing commands (no new subcommands):

1. Run `ato login` once and create/connect your Dock from Store Web `/publish`.
2. Build artifact locally: `ato build .`
3. Publish to your Dock:
   `ato publish --artifact ./<name>.capsule`
4. Share your public Dock page: `/d/<handle>` (`api.ato.run` install/search とは別)
5. When ready for the official Store, use `ato publish --registry https://api.ato.run` or `ato publish --ci`.
6. Final review/submission continues from Dock Control Tower (`Submit to Official Marketplace`).

```bash
# login once, then publish to your Personal Dock (recommended default)
ato login
ato build .
ato publish --artifact ./<name>.capsule

# pre-build + direct publish to a custom/private registry
ato build .
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./<name>.capsule

# stop-point examples
ato publish --prepare
ato publish --build                               # Prepare -> Build -> Verify
ato publish --artifact ./<name>.capsule --build  # Verify only
ato publish --artifact ./<name>.capsule          # default target: My Dock
ATO_TOKEN=pwd ato publish --deploy --artifact ./<name>.capsule --registry http://127.0.0.1:18787
ato publish --registry https://api.ato.run           # default: Publish only
ato publish --registry https://api.ato.run --build   # explicit local build + verify, then stop
ato publish --deploy --registry https://api.ato.run

# temporary compatibility flag (official only; deprecated and will be removed in next major)
ato publish --registry https://api.ato.run --legacy-full-publish

# idempotent retry for the same version/content (CI retry best practice)
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./<name>.capsule --allow-existing
```

## Proto Regeneration (Maintenance Only)

`protoc` is not required for normal builds.
Run this only when `core/proto/tsnet/v1/tsnet.proto` changes.

```bash
./core/scripts/gen_tsnet_proto.sh
```

## Source Sync Operations

Use these commands for source-backed registry workflows:

```bash
# inspect a sync run
ato source sync-status --source-id <source-id> --sync-run-id <sync-run-id> --registry <url>

# trigger rebuild / re-sign and optionally wait for status
ato source rebuild --source-id <source-id> --ref <branch|tag|sha> --wait --registry <url>
```

Notes:

- `sync-status` is read-only and can emit JSON with `--json`.
- `rebuild` can be used without `--ref`; the registry default ref is used.
- `rebuild --wait` triggers then polls the resulting sync run status.

## Local Registry E2E

```bash
# Terminal 1: start local HTTP registry
ato registry serve --host 127.0.0.1 --port 18787

# Terminal 2: build -> publish(artifact) -> install -> run
ato build .
ATO_TOKEN=pwd ato publish --artifact ./<name>.capsule --registry http://127.0.0.1:18787
ato install <publisher>/<slug> --registry http://127.0.0.1:18787
ato run <publisher>/<slug> --registry http://127.0.0.1:18787 --yes
```

Notes:

- Write operations (`publish`) require `ATO_TOKEN` when `registry serve --auth-token` is enabled.
- Read operations (search/install/download) can remain unauthenticated.
- Use `18787` in local verification to avoid collision with app services that also use `8787` (for example, worker HTTP ports).
- `publish --artifact` is the recommended path for local/private workflows.
- `--scoped-id` can override publisher/slug for artifact upload.
- `--allow-existing` is not a blind conflict ignore; it is an idempotent operation gated by artifact hash/manifest consistency checks.
- In enterprise CI, attach `--allow-existing` to retry paths to make reruns deterministic and safe.
- Version conflict is reported as `E202` with next actions (`bump version`, `--allow-existing`, or reset local registry).

Local registry Web UI:

- The detail page stores per-target runtime config under `/v1/local/.../runtime-config`.
- You can save target-specific `env` and `port` overrides from the UI.
- Tier2 targets can also persist execution permission mode (`sandbox` or `dangerous`) and reuse it on later runs.

## Cross-Device Publish (VPN / Tailscale)

```bash
# Server side: non-loopback exposure requires --auth-token
ato registry serve --host 0.0.0.0 --port 18787 --auth-token pwd

# Client side: install/run do not require token (read APIs)
ato install <publisher>/<slug> --registry http://100.x.y.z:18787
ato run <publisher>/<slug> --registry http://100.x.y.z:18787

# Token required only for publish
ATO_TOKEN=pwd ato publish --registry http://100.x.y.z:18787 --artifact ./<name>.capsule
```

## Required Environment Variable Checks (Pre-Run)

`ato run` validates required environment variables before startup.
If missing or empty, execution stops fail-closed.

- `targets.<label>.required_env = ["KEY1", "KEY2"]` (recommended)
- Backward compatibility: `targets.<label>.env.ATO_ORCH_REQUIRED_ENVS = "KEY1,KEY2"`

## Inspect Requirements JSON

`ato inspect requirements <path|publisher/slug> --json` returns a stable machine-readable
requirements contract derived from the project manifest.

The same `ato inspect` family now covers lock-first troubleshooting:

- `ato inspect lock [path] [--json]` shows field-level lock paths, provenance, unresolved markers, fallback use, and approval or selection gate involvement
- `ato inspect preview [path] [--json]` previews durable workspace write-back and run-attempt ephemeral materialization paths without mutating files
- `ato inspect diagnostics [path] [--json]` emits lock-path diagnostics and includes follow-up `inspect` / `preview` commands
- `ato inspect remediation [path] [--json]` suggests lock-path-first remediation steps and attaches source mapping when provenance can identify it

- the project manifest is the only source of truth for requirement discovery
- local paths and remote `publisher/slug` refs return the same top-level JSON shape
- state-related requirements are exposed under `requirements.state` (state-first), not `storage`
- success prints JSON only to `stdout`
- `--json` failures print structured JSON to `stderr` and exit non-zero

Success shape:

```json
{
  "schemaVersion": "1",
  "target": {
    "input": "./examples/foo",
    "kind": "local",
    "resolved": {
      "path": "/abs/path/to/examples/foo"
    }
  },
  "requirements": {
    "secrets": [],
    "state": [],
    "env": [],
    "network": [],
    "services": [],
    "consent": []
  }
}
```

Failure shape:

```json
{
  "error": {
    "code": "CAPSULE_TOML_NOT_FOUND",
    "message": "project manifest was not found",
    "details": {
      "input": "./examples/foo"
    }
  }
}
```

## Build Strictness

`ato build --strict-v3` disables fallback when `source_digest` / CAS(v3 path) is unavailable.
Use it when you want build diagnostics to fail immediately instead of falling back to a looser manifest path.

## Dynamic App Capsule Recipe (Web + Services Supervisor)

For multi-service apps (for example: dashboard + API + worker), use a single `web/deno` target with top-level `[services]`. `ato run` starts services in DAG order, waits on readiness probes, prefixes logs, and fail-fast stops all services when one exits.

1. Pre-bundle artifacts before packing (for example: `next build`, worker build, lockfiles).
2. Include only runtime artifacts via `[pack].include` (do not package raw `node_modules`, `.venv`, caches).
3. Build once, then publish with `--artifact` to avoid re-packing.

Minimal project manifest pattern:

```toml
schema_version = "0.2"
name = "my-dynamic-app"
version = "0.1.0"
default_target = "default"

[pack]
include = [
  "project-manifest.toml",
  "capsule.lock.json",
  "apps/dashboard/.next/standalone/**",
  "apps/dashboard/.next/static/**",
  "apps/control-plane/src/**",
  "apps/control-plane/pyproject.toml",
  "apps/control-plane/uv.lock",
  "apps/worker/src/**",
  "apps/worker/wrangler.dev.jsonc"
]
exclude = [
  ".deno/**",
  "node_modules/**",
  "**/__pycache__/**",
  "apps/dashboard/.next/cache/**"
]

[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10", uv = "0.4.30" }
port = 4173
required_env = ["CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID"]

[services.main]
entrypoint = "node apps/dashboard/.next/standalone/server.js"
depends_on = ["api"]
readiness_probe = { http_get = "/health", port = "PORT" }

[services.api]
entrypoint = "python apps/control-plane/src/main.py"
env = { API_PORT = "8000" }
readiness_probe = { http_get = "/health", port = "API_PORT" }
```

Recommended flow:

```bash
# 1) pre-bundle app artifacts
npm run capsule:prepare

# 2) package once
ato build .

# 3) publish artifact (private/local registry)
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./my-dynamic-app.capsule

# 4) install + run
ato install <publisher>/<slug> --registry http://127.0.0.1:18787
ato run <publisher>/<slug> --registry http://127.0.0.1:18787
```

Notes:

- For Next.js standalone, copy `.next/static` (and `public` if used) into standalone output before `ato build`.
- `ato run` stops before startup if `required_env` keys are missing.
- `services.main` is required in services mode and receives `PORT=<targets.<label>.port`.
- `targets.<label>.entrypoint = "ato-entry.ts"` is deprecated and rejected.
- If a service command starts with `node`, `python`, or `uv`, pin the matching version in `runtime_tools`.

## Runtime Isolation Policy (Tiers)

- `web/static`: Tier1 (`driver = "static"` + `targets.<label>.port` required; no `capsule.lock.json` needed)
- `web/deno`: Tier1 (`capsule.lock.json` + `deno.lock` or `package-lock.json`)
- `web/node`: Tier1 (Deno compat execution; requires `capsule.lock.json` + `package-lock.json`)
- `web/python`: Tier2 (requires `uv.lock`; `--sandbox` recommended)
- `source/deno`: Tier1 (`capsule.lock.json` + `deno.lock` or `package-lock.json`)
- `source/node`: Tier1 (Deno compat execution; requires `capsule.lock.json` + `package-lock.json`)
- `source/python`: Tier2 (requires `uv.lock`; `--sandbox` recommended)
- `source/native`: Tier2 (`--sandbox` recommended)

Notes:

- Node is Tier1 and does not require `--unsafe`.
- Tier2 (`source/native|python`, `web/python`) requires the `nacelle` engine.
  If not configured, execution stops fail-closed. Configure via `ato engine register`, `--nacelle`, or `NACELLE_PATH`.
- Legacy compatibility flags (`--unsafe`, `--unsafe-bypass-sandbox`) remain but are discouraged.
- Unsupported or out-of-policy Node/Python behavior does not auto-fallback; it stops fail-closed.
- `runtime=web` requires `driver` (`static|node|deno|python`).
- `public` is deprecated for `runtime=web`.
- For `runtime=web`, CLI prints the URL and does not automatically launch a browser.

## UX Policy (Silent Runner)

- Minimal output on success (tool stdout-first)
- Prompt only when explicit consent is required
- In non-interactive environments, `-y/--yes` auto-approves consent
- Policy violations and unmet requirements are emitted as `ATO_ERR_*` JSONL to `stderr`

## Security and Execution Policy (Zero-Trust / Fail-closed)

- Required env validation: startup fails if `targets.<label>.required_env` (or `ATO_ORCH_REQUIRED_ENVS`) is missing/empty
- Dangerous flag guard: `--dangerously-skip-permissions` is rejected unless `CAPSULE_ALLOW_UNSAFE=1`
- Local registry write auth: when `registry serve --auth-token` is enabled, `publish` requires `ATO_TOKEN`
- Engine auto-install: checksum retrieval/verification failures stop execution fail-closed

## Environment Variable Reference (Core)

- `CAPSULE_WATCH_DEBOUNCE_MS`: debounce interval for `run --watch` (ms, default: `300`)
- `CAPSULE_ALLOW_UNSAFE`: explicit allow for `--dangerously-skip-permissions` (only `1` is valid)
- `ATO_TOKEN`: auth token for local/private registry publish
- `ATO_STORE_API_URL`: API base URL for `ato search` / install flows (default: `https://api.ato.run`)
- `ATO_STORE_SITE_URL`: store web base URL (default: `https://store.ato.run`)
- `ATO_TOKEN`: session token for headless/CI environments

## Search and Auth

```bash
ato search ai
ato login
ato login --headless
ato whoami
```

Default endpoints:

- `ATO_STORE_API_URL` (default: `https://api.ato.run`)
- `ATO_STORE_SITE_URL` (default: `https://ato.run`)
- `ATO_TOKEN`
- canonical auth file: `${XDG_CONFIG_HOME:-~/.config}/ato/credentials.toml`

Auth precedence:

- `ATO_TOKEN`
- OS keyring
- `${XDG_CONFIG_HOME:-~/.config}/ato/credentials.toml`
- legacy `~/.ato/credentials.json` (read-only fallback)

## Development Tests

```bash
cargo test -p capsule-core execution_plan:: --lib
cargo test -p ato-cli --test local_registry_e2e -- --nocapture
```

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
