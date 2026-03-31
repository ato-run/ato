# Ato: The Agentic Meta-Runtime

[![CI](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=dev)](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/releases)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org/)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-6f42c1)
[![Issues](https://img.shields.io/github/issues/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/issues)
[![Last Commit](https://img.shields.io/github/last-commit/ato-run/ato-cli/dev)](https://github.com/ato-run/ato-cli/commits/dev)

English | [日本語](README_JA.md)

> The Nix alternative for the AI era. A next-generation meta-runtime that autonomously spins up a secure execution environment from nothing more than a URL, in about a second.

Ato leaves behind Docker's long image-build waits and Nix's hard-to-learn configuration language.
It autonomously infers the runtime and dependencies a project needs, then safely materializes and runs them inside a Zero-Trust sandbox, [Nacelle](https://github.com/ato-run/nacelle).

---

## Ato's Three Magic Tricks

### 1. No install. No clone. Just run from a URL in isolation: `ato run`

`ato run` executes a GitHub repository, registry package, or single script immediately without polluting your local environment. Dependencies are resolved in a temporary area, giving you an ephemeral execution path.

```bash
# Run a GitHub repository directly. Fetch, infer, and execute in isolation in one step.
ato run github.com/user/my-app

# Run a package from a registry immediately without installing it first.
ato run publisher/awesome-tool

# Run a single script safely with zero configuration.
ato run scrape.py
```

### 2. Materialize a fully reproducible dev environment from someone else's repo in one second: `ato init`

You do not even need `git clone`. Pass a URL and Ato fetches and analyzes the source, then materializes a real development workspace with full reproducibility, including LSPs and toolchains.

```bash
# From fetch to environment setup and dev shell preparation.
ato init github.com/user/repo my-project
```

### 3. Publish apps securely and integrate them into the desktop: `ato publish / install`

Use `ato publish` to register the tool you built in a registry. Users can then run `ato install` to pin it locally and project it into the OS as a native desktop app or CLI they can use every day.

```bash
# Register your tool in a registry as an immutable artifact.
ato publish

# Pin the package locally and register it as a desktop app.
ato install publisher/awesome-app
```

---

## Supported Tools and Languages

Ato's inference engine autonomously detects the following languages and project structures, then builds the best-fit runtime environment.
Even when none of these apply, `ato run` still works as a general-purpose process executor, with `ato.lock.json` preserving reproducibility for arbitrary binaries.

- Single-file execution
  - Python (`.py`): parses PEP 723 inline metadata and resolves libraries automatically.
  - TypeScript / TSX (`.ts`, `.tsx`): infers dependencies automatically on top of Deno.
- Programming languages and runtimes
  - TypeScript / JavaScript:
    - Deno (recommended): standard `deno.json` execution with URL imports and `npm:` support.
    - Node.js: detects `package.json` and lockfiles, with compatibility-mode support.
  - Python: standardizes on `uv`, infers from files such as `pyproject.toml`, and builds an isolated virtual environment.
  - Rust / Go: detects `Cargo.toml` and `go.mod`.
  - WebAssembly / OCI: direct execution of `.wasm` binaries and sandboxed execution of existing Docker images.
- Desktop / web frameworks
  - Tauri / Electron / Wails: native desktop integration through projection.
  - Static Web: preview for `index.html`-centric sites with a built-in web server.

---

## Quick Start

This is the standard workflow for local source development.

```bash
# Build
cargo build -p ato-cli

# If the nacelle engine is not installed yet (recommended)
./target/debug/ato config engine install --engine nacelle

# Compatibility command
./target/debug/ato setup --engine nacelle

# Run a local directory
./target/debug/ato run .

# Hot reload during development
./target/debug/ato run . --watch

# Background management
./target/debug/ato run . --background
./target/debug/ato ps
./target/debug/ato logs --id <capsule-id> --follow
./target/debug/ato close --id <capsule-id>
```

---

## Key Command Reference

```bash
ato run [path|publisher/slug|github.com/owner/repo] [--registry <url>]
ato init [path] [--yes]
ato install <publisher/slug> [--registry <url>]
ato install --from-gh-repo <github.com/owner/repo>
ato build [dir] [--strict-v3] [--force-large-payload]
ato publish [--registry <url>] [--artifact <file.capsule>] [--scoped-id <publisher/slug>]
ato ps
ato close --id <capsule-id> | --name <name> [--all] [--force]
ato logs --id <capsule-id> [--follow]
ato inspect lock [path] [--json]
ato inspect preview [path] [--json]
ato inspect diagnostics [path] [--json]
ato inspect requirements <path|publisher/slug> [--json]
ato source sync-status --source-id <id> --sync-run-id <id>
ato source rebuild --source-id <id> [--ref <branch|tag|sha>] [--wait]
ato search [query]
ato registry serve --host 127.0.0.1 --port 18787 [--auth-token <token>]
```

> `ato inspect` includes useful commands for debugging and development support, such as configuration preview (`preview`), issue diagnosis (`diagnostics`), remediation suggestions (`remediation`), and requirements JSON output (`requirements`).

---

## Architecture and Core Features

### Lock-native input model

Instead of relying on ambiguous human-written config, Ato treats machine-readable `ato.lock.json` as the single source of truth. When this file exists, it guarantees restoration of the same environment regardless of machine differences. If you have not migrated yet, older config formats still work, and `ato init` can migrate you forward.

### Unified delivery pipeline

Ato unifies consumer flows (`run` / `install`) and producer flows (`build` / `publish`). Instead of downloading a huge image during `ato build`, it hard-links only the required binaries from CAS (Content-Addressable Storage), keeping overhead effectively near zero.

### Native Delivery: projecting desktop apps

This is Ato's native app delivery and integration feature for platforms such as macOS. From project structures such as Tauri and Electron, Ato automatically infers the entrypoint, such as `.app`, then autonomously resolves and records all native delivery settings into `ato.lock.json`. No handwritten manifest configuration file is required.
When a user runs `ato install` on an artifact that includes this metadata, Ato projects it into the OS application area through symlinks and similar mechanisms so it launches like a normal desktop app.

### Dynamic app capsules: Web plus services supervisor

You can package multiple services, such as an API, dashboard, and worker, into a single capsule. When `[services]` is defined, `ato run` orchestrates them by starting them in DAG order, waiting on readiness probes, prefixing logs, and shutting everything down together if any service exits.

### Registry and publish model

`ato publish` behaves differently depending on where you publish. Publishing runs up to six stages in order: Prepare, Build, Verify, Install, Dry-run, Publish.

1. Personal Dock
   If you are already logged in with `ato login`, publishing without `--registry` uploads automatically to your Personal Dock and runs Prepare through Publish.
2. Custom or private registry
   You can upload to your own internal store with `--registry <url>`. This is also useful for E2E development against a local HTTP registry created by `ato registry serve`.
3. Official Store (`https://api.ato.run`)
   To keep the pipeline secure, the official Store is always published through CI with OIDC authentication. Local direct upload is not used. Local execution stops at the handoff immediately before Publish by sending diagnostics. You can generate the integration GitHub Actions with `ato gen-ci`.

---

## Security and Execution Policy: Zero-Trust

Ato is strict by default and fail-closed, protecting your system from unintended execution.

- Process isolation: even for desktop apps projected into the OS or for local source code, Ato launches the target inside its lightweight sandbox, [Nacelle](https://github.com/ato-run/nacelle).
- Filesystem protection: access is restricted to read-only by default, so AI-generated code and unknown libraries can be tested within a safe boundary.
- Network control: communication to domains that were not explicitly allowed is blocked at runtime.
- Strict environment-variable handling: if a required variable listed in `required_env` is missing, Ato warns and stops immediately before launch.

---

## Runtime Isolation Policy (Tiers)

Different runtimes require different isolation levels. Node and Deno run as Tier 1, while Python and native environments require a stronger sandbox for safety.

| Runtime         | Tier  | Required setup                                                                         |
| --------------- | ----- | -------------------------------------------------------------------------------------- |
| `web/static`    | Tier1 | `driver = "static"` + free port assignment (no lockfile required)                      |
| `web/deno`      | Tier1 | `ato.lock.json` or `deno.lock` / `package-lock.json`                                   |
| `web/node`      | Tier1 | `ato.lock.json` or `package-lock.json` (runs automatically through Deno compatibility) |
| `web/python`    | Tier2 | `uv.lock` + sandbox launch recommended                                                 |
| `source/deno`   | Tier1 | `ato.lock.json` or `deno.lock` / `package-lock.json`                                   |
| `source/node`   | Tier1 | `ato.lock.json` or `package-lock.json` (runs automatically through Deno compatibility) |
| `source/python` | Tier2 | `uv.lock` + sandbox launch recommended                                                 |
| `source/native` | Tier2 | compiled executable binary                                                             |

Tier 1 runs without special flags. Node is also Tier 1, so you do not need bypass flags such as `--unsafe`.

Tier 2 (`source/native`, `source/python`, `web/python`) requires the [Nacelle](https://github.com/ato-run/nacelle) engine. If it is not installed, Ato stops fail-closed. Prepare it with one of the following:

```bash
ato config engine install --engine nacelle
ato run --nacelle <path>
# or set the NACELLE_PATH environment variable
```

---

## Environment Variables and Authentication

The CLI behavior and default endpoints are controlled by the following environment variables.

| Variable                    | Description                                          | Default               |
| --------------------------- | ---------------------------------------------------- | --------------------- |
| `CAPSULE_WATCH_DEBOUNCE_MS` | Debounce interval for `run --watch` in milliseconds  | `300`                 |
| `CAPSULE_ALLOW_UNSAFE`      | Set to `1` to allow `--dangerously-skip-permissions` | —                     |
| `ATO_TOKEN`                 | Auth token for local/private publishing and CI       | —                     |
| `ATO_STORE_API_URL`         | API endpoint used by `ato search` and `install`      | `https://api.ato.run` |
| `ATO_STORE_SITE_URL`        | Base URL of the Store web app                        | `https://ato.run`     |

### How authentication works: `ato login`

By default, credentials are stored in `${XDG_CONFIG_HOME:-~/.config}/ato/credentials.toml`. `ato` resolves authentication in the following order:

1. `ATO_TOKEN` environment variable
2. OS secure keyring
3. `~/.config/ato/credentials.toml`
4. Legacy `~/.ato/credentials.json` as a fallback

---

## Contributing

Thanks for your interest in contributing. For development details and internal architecture, refer to the core project documentation.

- Bug reports and feature requests are welcome in GitHub Issues.
- For discussion and questions, join the Discord community.

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
