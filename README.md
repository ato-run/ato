# ato-cli

[![CI](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=dev)](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/releases)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org/)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-6f42c1)
[![Issues](https://img.shields.io/github/issues/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/issues)
[![Last Commit](https://img.shields.io/github/last-commit/ato-run/ato-cli/dev)](https://github.com/ato-run/ato-cli/commits/dev)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/ato-run/ato-cli)

English | [日本語](README_JA.md)

`ato` is a command-line tool for running, sharing, and installing capsules.

When you run `ato`, it reads your `ato.lock.json` and uses it as the main source of truth. If you haven't created one yet, older config formats still work — run `ato init` to migrate when you're ready.

`ato` is quiet by design. It only shows output when something needs your attention, such as a missing permission or a policy violation.

For the full specification, see `docs/current-spec.md`.

## Lock-Native Input Model

`ato.lock.json` is the single source of truth. When it exists, `ato` uses it and everything else becomes secondary.

Here's how the different formats interact:

- **`ato.lock.json`**: The main config file. Always takes priority when present.
- **Legacy and bootstrap formats**: Still work for projects that haven't migrated yet. Run `ato init` to move to the canonical format.
- **`capsule.lock.json`**: An older compatibility format. `ato` can read it alongside `ato.lock.json`, but it can't be used on its own.

`ato inspect lock`, `ato run`, and `ato build` all follow this priority order.

## Key Commands

```bash
ato run [path|publisher/slug|github.com/owner/repo] [--registry <url>]
ato search [query]
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
ato inspect lock [path] [--json]
ato inspect diagnostics [path] [--json]
ato inspect remediation [path] [--json]
```

## Native Delivery (Experimental)

> **Note:** The core commands — `ato build`, `ato publish`, and `ato install` — are stable. Native Delivery is experimental functionality built on top of them.

Native Delivery lets you package and distribute native desktop apps. The current implementation supports Tauri apps on macOS darwin/arm64.

**To get started:**

Add a native target to your project manifest. Set `driver = "native"` and point `entrypoint` to your `.app` bundle. That's enough for `ato` to recognize it as a native app.

```toml
[targets.desktop]
driver = "native"
entrypoint = "MyApp.app"
```

**What `ato` fills in automatically:**

For `.app`-style entrypoints, `ato` derives these defaults internally so you don't have to specify them:

- `artifact.framework = "tauri"`
- `artifact.stage = "unsigned"`
- `artifact.target = "darwin/arm64"`
- `artifact.input = <targets.<default>.entrypoint>`
- `finalize.tool = "codesign"`
- `finalize.args = ["--deep", "--force", "--sign", "-", <artifact.input>]`

**Current limitations:**

- macOS darwin/arm64 with `codesign` only.
- Local fetch/finalize/projection phases are internal implementation details. The supported user workflow stays on `build` / `publish` / `install`.
- Local finalize stops immediately on any error (fail-closed).
- On macOS, projection creates a `~/Applications` symlink. On Linux, it creates a `.desktop` launcher and a `~/.local/bin` symlink.

When you run an installed desktop-native capsule, `ato run <publisher>/<slug>` opens the locally derived app bundle from `~/.ato/apps/.../derived-*` and returns once the platform launch request succeeds. It does not treat the GUI app as a service that must emit readiness events.

**For command-driven targets:**

If your target is command-driven (`entrypoint = "sh"` with `cmd = [...]`), you need to write explicit delivery metadata in `[artifact]` and `[finalize]`. Partial configs are rejected. Source-side `ato.delivery.toml` is always rejected — write everything in the project manifest.

### Native Delivery contract (current canonical form)

The full minimal project manifest for native delivery:

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

### How artifact metadata flows

`ato.delivery.toml` is managed internally by `ato`. You don't write it. Here's what happens behind the scenes:

1. When you run `ato build`, it packages your app and embeds a `ato.delivery.toml` into the artifact.
2. This embedded file makes the artifact self-describing so `ato install` can complete native delivery without the original source tree.
3. The remaining fetch/finalize/projection steps are internal parts of the install pipeline.

**Rule:** Write native delivery config in your project manifest only. Do not create `ato.delivery.toml` directly.

### Stable vs experimental machine-readable contract

The following fields are stable for `schema_version = "0.1"`. They are documented and test-guarded in the repo. Removing or renaming any of them is a breaking schema change.

- `fetch.json`: `schema_version`, `scoped_id`, `version`, `registry`, `parent_digest`
- build JSON: `build_strategy = "native-delivery"`, `schema_version`, `target`, `derived_from`
- finalize JSON: `schema_version`, `derived_app_path`, `provenance_path`, `parent_digest`, `derived_digest`
- `local-derivation.json`: `schema_version`, `parent_digest`, `derived_digest`, `framework`, `target`, `finalize_tool`, `finalized_at`
- project JSON: `schema_version`, `projection_id`, `metadata_path`, `projected_path`, `derived_app_path`, `parent_digest`, `derived_digest`, `state`
- unproject JSON: `schema_version`, `projection_id`, `metadata_path`, `projected_path`, `removed_projected_path`, `removed_metadata`, `state_before`
- install JSON: `install_kind`, `launchable`, `local_derivation`, `projection`
  - `install_kind = "NativeRequiresLocalDerivation"` — install succeeded, but the app must be launched from the locally derived bundle (not the `.capsule`)
  - `launchable.path` — the path to use when launching the app
  - `local_derivation.provenance_path`, `parent_digest`, `derived_digest` — links the internal derivation steps to the install result
  - `projection.metadata_path` — metadata for launcher state inspection

Still experimental:

- On-disk layout under `~/.ato/fetches`, `~/.ato/apps`, and `~/.ato/native-delivery/projections`
- Any fields beyond the stable set above
- The internal fetch/finalize/projection implementation details
- Support for platforms other than macOS darwin/arm64

### Migration path

1. **Now**: Write native delivery config in the project manifest only.
2. **Now**: `ato` still embeds `ato.delivery.toml` in artifacts for internal finalize/install/project flows.
3. **Later**: The internal metadata format may change, but the `schema_version = "0.1"` JSON contract will be preserved.

## Quick Start (Local)

```bash
# build
cargo build -p ato-cli

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

When you run `ato publish`, it goes through up to 6 stages in order:

**`Prepare → Build → Verify → Install → Dry-run → Publish`**

Which stages run — and where the capsule ends up — depends on which registry you're targeting.

---

### Publishing to the official store

**Registry:** `https://api.ato.run` or `https://staging.api.ato.run`

The official store uses OIDC for authentication. You can't upload directly from your local machine. Publishing happens through CI.

Default behavior: runs the **Publish** stage only. This is a handoff — `ato` sends diagnostics to the registry and your CI pipeline handles the actual upload.

---

### Publishing to your Personal Dock

**Registry:** default when you're logged in (no `--registry` needed)

If you've run `ato login`, `ato publish` automatically uploads to your Dock.

Default behavior: runs all stages from Prepare through Publish.

Current managed direct-upload limitation:

- Personal Dock direct upload currently uses the managed Store direct-upload path.
- Artifacts larger than the current conservative preflight limit of 95 MB are rejected before upload.
- `--force-large-payload` and `--paid-large-payload` are not available on this path.
- If you need a larger direct upload today, use a custom/private registry instead.

Experimental P1 migration path:

- `ATO_PUBLISH_UPLOAD_STRATEGY=presigned` opts into the new presigned upload strategy for compatible registries.
- The default remains `direct` until registry capability discovery and unverified Personal Dock parity are in place.
- The presigned strategy requires an authenticated publisher session and the local publisher signing key created during publisher onboarding.

Tips:

- Use `--artifact <file>` to skip rebuilding. Upload a file you've already built.
- `--scoped-id` defaults to `<your-handle>/<slug>` automatically.
- Your Dock page is at `/d/<handle>`. This is a UI page, not a registry endpoint.

---

### Publishing to a custom or private registry

**Registry:** any `--registry <url>` value not listed above

Default behavior: runs all stages from Prepare through Publish, with direct upload.

Tips:

- `--artifact <file>` works here too. You can publish without a local project manifest.
- `--allow-existing` is available for the final Publish stage only.
- `--force-large-payload` and `--paid-large-payload` remain available here because custom/private direct registries are not forced onto the managed Store direct-upload policy.

---

### Controlling which stages run

The `--prepare`, `--build`, and `--deploy` flags are _stop points_. They tell `ato` where to stop, not which individual stages to run.

| Flag                | Stops after       | Notes                                                                                             |
| ------------------- | ----------------- | ------------------------------------------------------------------------------------------------- |
| `--prepare`         | Prepare           |                                                                                                   |
| `--build`           | Verify            | With source: runs Build then Verify. With `--artifact`: runs Verify only.                         |
| `--deploy`          | Publish           | Official: handoff only. Private/local: auto-resolves, or runs Verify → Publish with `--artifact`. |
| `--artifact <file>` | _(changes start)_ | Skips Prepare and Build. Starts pipeline at Verify.                                               |

Other things to know:

- `--ci` and `--dry-run` cannot be combined with stage flags.
- `--artifact --prepare` is invalid — the start stage would come after the stop point.
- `--legacy-full-publish` (official only) restores the old default behavior. **Deprecated** — will be removed in the next major release.

Official registry helpers:

- `ato publish --fix` — fixes a broken workflow once, then reruns diagnostics.
- `ato publish --no-tui` — skips the interactive UI and prints CI output directly.

### Publish payload limitation (E212)

`E212` means the managed Store publish path rejected or disallowed the current payload configuration.

Typical causes:

- the artifact is larger than the current conservative preflight limit for managed direct upload
- `--force-large-payload` or `--paid-large-payload` was used with the managed direct-upload path
- the remote managed upload path returned `413 Payload Too Large`

Suggested next actions:

- reduce artifact size
- publish to a custom/private registry if direct upload is required now
- use the official CI-first flow for the official Store
- wait for managed presigned upload support for larger artifacts

### Migration Notes

- `ato publish --build` now stops after Verify, not immediately after Build.
- `ato run --skill` and `ato run --from-skill` have been removed.

## Dock-first Flow (Personal Dock)

The typical workflow for publishing to your Dock:

1. **Log in:** `ato login`
   Then create or connect your Dock from the Store Web `/publish` page.
2. **Build:** `ato build .`
3. **Publish to your Dock:** `ato publish --artifact ./<name>.capsule`
4. **Share your page:** Your public Dock URL is `/d/<handle>`.
5. **Ready for the official store?** Use `ato publish --registry https://api.ato.run` or `ato publish --ci`.
6. **Submit for review:** Go to Dock Control Tower and click `Submit to Official Marketplace`.

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
ato publish --registry https://api.ato.run --build   # local build + verify, then stop
ato publish --deploy --registry https://api.ato.run

# temporary compatibility flag (official only; deprecated and will be removed in next major)
ato publish --registry https://api.ato.run --legacy-full-publish

# retry safely with the same version (CI best practice)
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./<name>.capsule --allow-existing
```

## Proto Regeneration (Maintenance Only)

You don't need `protoc` for normal builds. Only run this when `core/proto/tsnet/v1/tsnet.proto` changes:

```bash
./core/scripts/gen_tsnet_proto.sh
```

## Required Environment Variable Checks (Pre-Run)

Before launching a capsule, `ato run` checks that all required environment variables are set. If any are missing or empty, it stops immediately.

Declare required variables in your manifest:

```toml
# recommended
targets.<label>.required_env = ["KEY1", "KEY2"]

# legacy compatibility
targets.<label>.env.ATO_ORCH_REQUIRED_ENVS = "KEY1,KEY2"
```

## Inspect and Troubleshoot Locks

The public `ato inspect` workflow for lock-first debugging is:

- `ato inspect lock [path] [--json]` — shows what each lock field resolved to, where it came from, and what's still unresolved
- `ato inspect diagnostics [path] [--json]` — shows what's wrong with your lock config and links to the right fix commands
- `ato inspect remediation [path] [--json]` — suggests specific fixes, with source location mapping where possible

A few notes:

- Requirements are still derived from the project manifest, not the lock file.
- `ato inspect requirements` and `ato inspect preview` remain available for compatibility and internal/debug use, but they are not part of the primary public troubleshooting surface.
- Local paths and remote `publisher/slug` refs return the same JSON shape.
- State-related requirements appear under `requirements.state`, not `storage`.
- On success, output goes to `stdout`. On failure with `--json`, structured output goes to `stderr` and the process exits non-zero.

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

By default, `ato build` falls back to a looser manifest path if the content-addressed source digest isn't available.

Use `--strict-v3` to disable that fallback. With this flag, the build fails immediately if `source_digest` or the CAS v3 path is unavailable — useful when you want to catch problems early rather than silently fall back.

```bash
ato build --strict-v3
```

## Dynamic App Capsule Recipe (Web + Services Supervisor)

For apps with multiple services — for example, a dashboard, an API server, and a worker — you can package everything into a single capsule using `[services]`.

When you run the capsule, `ato run`:

- Starts services in dependency order (DAG)
- Waits for each service to pass its readiness probe
- Prefixes all log output with the service name
- Stops everything immediately if any service exits unexpectedly

**Before you package:**

1. Build your artifacts first (e.g. `next build`, worker bundling, lockfile generation). Don't include source code in the capsule.
2. In `[pack].include`, list only what's needed at runtime. Skip `node_modules`, `.venv`, and caches.
3. Build once with `ato build`, then use `--artifact` to publish without rebuilding.

Minimal project manifest:

```toml
schema_version = "0.2"
name = "my-dynamic-app"
version = "0.1.0"
default_target = "default"

[pack]
include = [
  "capsule.toml",
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

Recommended workflow:

```bash
# 1) pre-bundle app artifacts
npm run capsule:prepare

# 2) package once
ato build .

# 3) publish artifact (private/local registry)
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./my-dynamic-app.capsule

# 4) install and run
ato install <publisher>/<slug> --registry http://127.0.0.1:18787
ato run <publisher>/<slug> --registry http://127.0.0.1:18787
```

A few gotchas:

- For Next.js standalone, copy `.next/static` (and `public` if needed) into the standalone output before running `ato build`.
- `ato run` stops before startup if any `required_env` key is missing.
- `services.main` is required in services mode. It receives `PORT=<targets.<label>.port>`.
- `targets.<label>.entrypoint = "ato-entry.ts"` is deprecated and rejected.
- If a service command starts with `node`, `python`, or `uv`, pin the matching version in `runtime_tools`.

## Runtime Isolation Policy (Tiers)

Different runtimes have different isolation requirements:

| Runtime         | Tier  | What you need                                                        |
| --------------- | ----- | -------------------------------------------------------------------- |
| `web/static`    | Tier1 | `driver = "static"` + port configured; no `capsule.lock.json` needed |
| `web/deno`      | Tier1 | `capsule.lock.json` + `deno.lock` or `package-lock.json`             |
| `web/node`      | Tier1 | `capsule.lock.json` + `package-lock.json` (runs via Deno compat)     |
| `web/python`    | Tier2 | `uv.lock`; `--sandbox` recommended                                   |
| `source/deno`   | Tier1 | `capsule.lock.json` + `deno.lock` or `package-lock.json`             |
| `source/node`   | Tier1 | `capsule.lock.json` + `package-lock.json` (runs via Deno compat)     |
| `source/python` | Tier2 | `uv.lock`; `--sandbox` recommended                                   |
| `source/native` | Tier2 | `--sandbox` recommended                                              |

**Tier1** runs without special flags. Node is Tier1 — you do not need `--unsafe`.

**Tier2** (`source/native`, `source/python`, `web/python`) requires the `nacelle` engine. If it's not installed, `ato` stops fail-closed. Set it up with one of:

```bash
ato engine register     # register the engine path
ato run --nacelle ...   # pass path at runtime
# or set NACELLE_PATH environment variable
```

Other notes:

- Legacy flags `--unsafe` and `--unsafe-bypass-sandbox` still exist but are discouraged.
- Unsupported or out-of-policy behavior doesn't fall back silently — it stops with an error.
- `runtime=web` requires a `driver` value: `static`, `node`, `deno`, or `python`.
- `public` is deprecated for `runtime=web`.
- For `runtime=web`, the CLI prints the URL but doesn't open a browser automatically.

## UX Policy (Silent Runner)

`ato` is designed to stay out of your way:

- **On success**: minimal output. Tool stdout is printed directly.
- **Consent prompts**: only when truly required.
- **Non-interactive mode**: pass `-y` / `--yes` to auto-approve.
- **Errors**: policy violations and unmet requirements are written as `ATO_ERR_*` JSONL to `stderr`.

## Security and Execution Policy (Zero-Trust / Fail-closed)

`ato` is strict by default:

- **Required env validation**: If a variable listed in `targets.<label>.required_env` (or `ATO_ORCH_REQUIRED_ENVS`) is missing or empty, `ato run` stops before launching.
- **Dangerous flags**: `--dangerously-skip-permissions` is rejected unless `CAPSULE_ALLOW_UNSAFE=1` is set.
- **Registry write auth**: Publishing to a registry started with `--auth-token` requires `ATO_TOKEN`.
- **Engine verification**: If checksum verification fails during engine auto-install, execution stops.

## Environment Variable Reference (Core)

| Variable                    | Description                                          | Default               |
| --------------------------- | ---------------------------------------------------- | --------------------- |
| `CAPSULE_WATCH_DEBOUNCE_MS` | Debounce interval for `run --watch`, in milliseconds | `300`                 |
| `CAPSULE_ALLOW_UNSAFE`      | Set to `1` to allow `--dangerously-skip-permissions` | —                     |
| `ATO_TOKEN`                 | Auth token for local/private publish and CI sessions | —                     |
| `ATO_STORE_API_URL`         | API base URL for `ato search` and install            | `https://api.ato.run` |
| `ATO_STORE_SITE_URL`        | Store web base URL                                   | `https://ato.run`     |

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
- Canonical auth file: `${XDG_CONFIG_HOME:-~/.config}/ato/credentials.toml`

Auth is resolved in this order:

1. `ATO_TOKEN` environment variable
2. OS keyring
3. `${XDG_CONFIG_HOME:-~/.config}/ato/credentials.toml`
4. Legacy `~/.ato/credentials.json` (read-only fallback)

## Development Tests

```bash
cargo test -p capsule-core execution_plan:: --lib
cargo test -p ato-cli --test local_registry_e2e -- --nocapture
```

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
