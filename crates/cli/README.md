# ato

[![CI](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=main)](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/releases)
[![GitHub stars](https://img.shields.io/github/stars/ato-run/ato-cli?style=social)](https://github.com/ato-run/ato-cli/stargazers)
[![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MPL--2.0-blue)](LICENSE)

**Run any project instantly. Share it as a local recipe.**

Point `ato` at a Python script, a Node app, a Rust binary, or a GitHub repo — it figures out the runtime, bootstraps only what's needed, and runs it in a sandboxed runtime. No Dockerfile. No setup guide. No manual environment.

[Install](#install) · [Quick start](#quick-start) · [Why Ato](#why-ato) · [Commands](#core-commands) · [Contributing](#contributing)

## Demo

![Demo](assets/demo.svg)

## Install

### macOS / Linux

Default install:

```bash
curl -fsSL https://ato.run/install.sh | sh
```

On a graphical macOS/Linux session, this installs Ato Desktop. The Desktop
bundle includes the private helper binaries it needs: `ato`, `nacelle`, and
`ato-netd`. It does not separately install the standalone CLI archive.

To also make `ato` available in your terminal:

```bash
curl -fsSL https://ato.run/install.sh | sh -s -- --with-cli
```

For headless environments, SSH sessions, CI, or CLI-only usage:

```bash
curl -fsSL https://ato.run/install.sh | sh -s -- --cli-only
```

### Windows PowerShell

Default install:

```powershell
irm https://ato.run/install.ps1 | iex
```

On normal Windows desktop sessions, this installs the Ato Desktop MSI. The
Desktop bundle includes private `ato.exe`, `nacelle.exe`, and `ato-netd.exe`
helpers.

To ensure `ato` is available from PowerShell:

```powershell
irm https://ato.run/install.ps1 | iex -WithCli
```

For CI, server/headless environments, or CLI-only usage:

```powershell
irm https://ato.run/install.ps1 | iex -CliOnly
```

### Other channels

```bash
# Homebrew — CLI only
brew install ato-run/ato/ato-cli

# Build from source (Rust toolchain required)
cargo install --locked --git https://github.com/ato-run/ato-cli ato-cli
```

Prebuilt binaries are available on the [Releases page](https://github.com/ato-run/ato-cli/releases/latest).

## Quick start

```bash
# Install (one line)
curl -fsSL https://ato.run/install.sh | sh
```

**Consuming — run someone else's project:**

```bash
# Try once in a sandbox
ato run github.com/owner/repo
ato run ./share.spec.json             # runnable workspaces only (see note below)

# Keep it locally
ato workspace setup ./share.spec.json --into ./demo
ato run ./demo
```

**Producing — share your own project:**

```bash
# Run a Python script — no venv, no pip install
printf 'print("hello from ato")\n' > hello.py
ato run hello.py

# Capture the workspace into local share files
ato workspace share
# → Wrote share files: .ato/share/share.spec.json, .ato/share/share.lock.json, .ato/share/guide.md
```

## Why Ato

Every time you share a project, someone has to set up an environment before they can run it — virtualenvs, `node_modules`, container builds, README instructions that drift. Ato removes that layer entirely.

Ato reads your project directly — `pyproject.toml`, `package.json`, `deno.json`, `Cargo.toml`, a bare script — and materializes only the runtime it needs. No config to write. For Python and native binaries, the run phase routes through [Nacelle](https://github.com/ato-run/nacelle), a sandbox that applies OS-native filesystem and network isolation when your code executes (see [Security and isolation](#security-and-isolation) for what each platform enforces). `ato workspace share` captures a reproducible workspace descriptor (`share.spec.json` / `share.lock.json`) that anyone can restore with `ato workspace setup`.

### Mental model: Try → Keep → Share

Four commands map to two axes — the direction (consume vs. produce) and the persistence of the result (ephemeral vs. persistent):

|                                  | Just try it (ephemeral)                       | Set it up (persistent)                           |
|----------------------------------|-----------------------------------------------|--------------------------------------------------|
| **Consume** someone else's code  | `ato run <share file>` *(runnable workspaces only)* | `ato workspace setup <share file>` *(any workspace)* |
| **Produce** your own code        | `ato run .`                                   | `ato workspace share`                            |

And the classic pain point comparison:

| Without Ato | With Ato |
|---|---|
| Clone → read README → install deps → run | `ato run github.com/owner/repo` |
| Write Dockerfile or setup script to share | `ato workspace share` |
| Follow multi-step setup to reproduce | `ato workspace setup <share file>` |

Supported runtimes today: Python (`pyproject.toml`, `uv.lock`, single-file PEP 723), Node / TypeScript / Deno, Rust, Go, static web, WebAssembly, and shell scripts.

## Core commands

Commands are ordered by the Try → Keep → Share journey.

### Try it with `ato run`

`ato run` accepts a local path, a local share file, or a GitHub repository reference. It covers two distinct use cases:

**Run it — try a project in a sandboxed run environment (consume):**

```bash
ato run hello.py
ato run github.com/owner/repo
ato run ./share.spec.json
```

**Develop it — iterate on your own workspace (produce):**

```bash
ato run .
ato run . --watch
ato run . --background
ato ps
ato logs --id <capsule-id> --follow
ato stop --id <capsule-id>
```

`--watch` and `--background` are only available for local filesystem paths. `ato run <share file>` does not support them in the current MVP path.

> **When `run` works on a share file**
>
> `ato run <share file>` works only when the shared workspace is declared as runnable in its `capsule.toml` — specifically, `type = "app"` or `"tool"` with an entrypoint defined via `run`, `[targets.*]`, or `[services]`.
>
> Workspaces without an entrypoint (libraries, datasets, templates, `type = "library"`) are still shareable, but receivers must use `ato workspace setup` to expand them and run locally. `ato run` fails closed before launch if the share is not runnable. See [Runnable workspace](#runnable-workspace) for the full rules.

### Keep it with `ato workspace setup`

`ato workspace setup` materializes a share into a target directory, verifies it, and runs declared install steps (`--dev`). Use this when you want a persistent copy, or when the share is not runnable.

```bash
ato workspace setup ./share.spec.json --into ./my-project
ato workspace setup .ato/share/share.lock.json --into ./my-project
```

### Share it with `ato workspace share`

`ato workspace share` captures the current workspace as a portable local share descriptor and writes it under `.ato/share/`. Run it from the project directory — no arguments needed.

```bash
ato workspace share
```

Local capture output is written under `.ato/share/`:

- `share.spec.json`
- `share.lock.json`
- `guide.md`

Secrets are never included. Ato records contracts such as required environment files, but not secret values.

#### `workspace share` vs `publish`

- `ato workspace share` — turn a workspace into a **shareable local descriptor** (`share.spec.json` / `share.lock.json`). Intended for ad-hoc sharing and reproducibility.
- `ato publish` — release the workspace as a **capsule to the registry**. Involves versioning, signing, and CI integration. Intended for distribution as a named artifact.

Use `workspace share` for private/informal sharing; use `publish` when you want a durable, versioned release.

## Security and isolation

Ato's isolation applies to the **run phase** — when your code (or someone
else's) actually executes. It is layered, and the guarantees differ by platform
and runtime, so here is what is and isn't enforced today.

**What the run phase enforces**

- Sandbox isolation: Tier 2 targets such as `source/python`, `web/python`, and `source/native` execute through Nacelle, which applies OS-native sandboxing (Landlock on Linux, Seatbelt on macOS).
- Filesystem isolation: on Linux the source filesystem view is a deny-by-default allowlist — code sees only the paths explicitly granted. On macOS it is currently allow-by-default with a blocklist of sensitive paths (SSH keys, cloud credentials, and similar), not yet a strict allowlist; treat it as defense-in-depth rather than a hard boundary.
- Network control: deny-all egress (`network.enabled = false`) is fully enforced on both platforms. A hostname/IP `egress_allow` allowlist is advisory only on source runtimes today — do not rely on it to contain a process that already has network access.
- Environment handling: the run process starts from a reconstructed, isolated environment with host secrets excluded by default. `--prompt-env` can collect required values interactively.

**What is not sandboxed yet**

- The **build / prepare phase runs unsandboxed on the host**, with your normal environment, secrets, and network access. Dependency installs, `build`, and `prepare` lifecycle commands are ordinary host processes — the run-phase sandbox does not retroactively contain what they did. **Only build code you trust.**
- `required_env` and `egress_allow` are advisory in v0.x: a missing `required_env` entry warns but does not abort the launch, and `egress_allow` does not enforce a hostname allowlist on source runtimes. See [Known limitations](#known-limitations).

For normal local runs, Ato usually bootstraps a compatible Nacelle release automatically when Tier 2 execution requires it. In CI or offline environments, auto-bootstrap is intentionally restricted, so preinstall or register Nacelle ahead of time if needed.

## Build from source

`cargo install` is a first-class install path in v0.5 — no signing, no Gatekeeper, no Apple Developer ID required.

```bash
# CLI only
cargo install --locked --git https://github.com/ato-run/ato-cli ato-cli

# Desktop host (requires GPUI build deps: Metal on macOS, Vulkan on Linux, DX11 on Windows)
cargo install --locked --git https://github.com/ato-run/ato-desktop ato-desktop
```

For local development:

```bash
cargo build -p ato-cli
./target/debug/ato --help
./target/debug/ato run .
```

## Contributing

Bug reports and feature requests are welcome in [GitHub Issues](https://github.com/ato-run/ato-cli/issues).

If you are contributing code, use the standard Rust checks before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p ato-cli
```

See [TESTING.md](TESTING.md) for the full testing guide, including manual pre-release test suites.

## Known limitations

Some v0.5 behaviours differ from the full spec intent. See [docs/known-limitations.md](docs/known-limitations.md) for the full list. Key gaps:

- `egress_allow` is advisory on source runtimes (deny-all via `network.enabled = false` is enforced)
- `required_env` missing entries warn but do not abort execution
- `--sandbox` flag not yet supported for `source/python`

## Foundation readiness — 0 / 6

The Capsule Protocol defines open-governance transfer criteria (§11.2). Current status (0 of 6 KPIs met):

| KPI | Target | Status |
|-----|--------|--------|
| External conforming runtime | ≥1 | 0 / 1 |
| Conformance suite pass rate | ≥70% | skeleton only — see [`conformance/`](conformance/) |
| External maintainers | ≥3 | 0 / 3 |
| TSC non-ato majority | required | 0 / required |
| Publishers | ≥100 | 0 / 100 |
| Adversarial security reports | ≥5 | 0 / 5 |

Foundation transfer is not a v0.5 milestone. Published for transparency.

## License

Apache License 2.0 or Mozilla Public License 2.0 (SPDX: Apache-2.0 OR MPL-2.0).

## capsule.toml reference

Every capsule is declared by a `capsule.toml` manifest in the project root.

### Core fields

| Field | Required | Description |
|-------|----------|-------------|
| `schema_version` | ✓ | Manifest schema version. Use `"0.3"` |
| `name` | ✓ | Unique capsule identifier (lowercase, hyphens allowed) |
| `version` | | Semver string, e.g. `"0.1.0"` |
| `type` | ✓ | `"app"`, `"service"`, or `"tool"` |
| `run` | | Default run command (inference may set this automatically) |
| `runtime` | | Runtime hint: `"source/python"`, `"source/node"`, `"wasm"`, `"oci"` |
| `runtime_version` | | Pinned version, e.g. `"3.12"` or `"20"` |
| `description` | | Human-readable description |

Minimal example:

```toml
schema_version = "0.3"
name           = "my-capsule"
version        = "0.1.0"
type           = "app"
run            = "python main.py"
runtime        = "source/python"
```

### Runnable workspace

`ato run <share file>` launches a shared workspace directly only if its `capsule.toml` declares an entrypoint. A workspace is **runnable** when at least one of the following is true:

- A top-level `run = "..."` is defined
- `[targets.*]` declares at least one executable target
- `[services]` is defined

Workspaces that satisfy none of the above are still shareable via `ato workspace share`, but receivers must use `ato workspace setup` to expand them before executing anything locally. `type = "library"` is always treated as non-runnable regardless of other fields.

This is a contract enforced at the receiving end: the publisher decides whether their workspace is runnable by how they author `capsule.toml`, and `ato run` fails closed if the contract is not met.

### `[network]` — egress control

Controls outbound network access. By default (`network.enabled = false`) all egress is denied.

```toml
[network]
egress_allow = ["api.openai.com", "huggingface.co"]
```

| Field | Description |
|-------|-------------|
| `egress_allow` | Allowlisted hostnames for L7 proxy |
| `egress_id_allow` | Allowlisted IPs/CIDRs at L3, e.g. `[{type="cidr", value="10.0.0.0/8"}]` |

> **Note (v0.5):** `egress_allow` is advisory on source runtimes. `network.enabled = false` (deny-all) is fully enforced. See [known limitations](docs/known-limitations.md).

### `[isolation]` — host passthrough

Controls which host environment variables are passed into the capsule.

```toml
[isolation]
allow_env = ["HF_TOKEN", "CUDA_HOME", "LD_LIBRARY_PATH"]
```

### `[transparency]` — binary policy

Declares policy for binary files included in the capsule payload.

```toml
[transparency]
level           = "source-preferred"   # "source-only" | "source-preferred" | "opaque"
allowed_binaries = ["lib/**/*.so", "venv/bin/*"]
```

### `[targets]` — multi-target execution

Declares multiple runtime targets; the engine selects the best match at launch time.

```toml
[targets]
preference = ["wasm", "source", "oci"]

[targets.wasm]
file = "dist/capsule.wasm"

[targets.source]
runtime = "source/python"
run     = "python main.py"

[targets.oci]
image = "ghcr.io/owner/repo:latest"
```

Target-level `install` declares the dependency install lifecycle for the
target's dependency root. When at least one target sharing a dependency root
declares `install`, ato runs the explicit command once for that root and skips
the inferred package-manager provision command (`npm install`, `pnpm install`,
`yarn install`, `bun install`, etc.). Identical install commands on the same
root are deduped; conflicting commands on the same root are rejected.

For Bun monorepos that need to avoid root lifecycle scripts during dependency
installation, declare that explicitly:

```toml
[targets.app]
runtime = "source"
driver = "node"
install = "bun install --ignore-scripts"
build = "bunx prisma generate && bun run build:web && bun run build:seed"
run = "bun run prisma:migrate:deploy && bun run seed && bun run start:server:production"
```

### `[services]` — supervisor mode (multi-process)

Run multiple processes as a single capsule, with dependency ordering.

```toml
[services.db]
entrypoint = "postgres -D /data"

[services.api]
entrypoint = "python server.py"
depends_on = ["db"]
expose     = ["PORT"]
env        = { DATABASE_URL = "postgres://localhost/app" }

[services.api.readiness_probe]
http_get = "/health"
port     = "PORT"
```

### `[dependencies.*]` — capsule dependency contracts

Declare another capsule that ato should start automatically before this
one. The dependency is resolved by `capsule://` URL and bound to a
contract that defines its runtime exports + ready semantics. See
[CAPSULE_DEPENDENCY_CONTRACTS.md](../../docs/rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md)
for the full RFC.

```toml
# Manifest top-level: required_env that {{env.X}} in [dependencies.*]
# may reference (RFC §5.2).
required_env = ["PG_PASSWORD"]

[dependencies.db]
capsule  = "capsule://ato/postgres@16"
contract = "service@1"

# Identity-bearing parameters. These enter instance_hash and the
# dependency_derivation_hash on the consumer's v2 receipt.
[dependencies.db.parameters]
database = "myapp"

# Runtime-only credentials. Kept in the lock as the template form
# only; the resolved value is materialized via Rule M1 TempFile and
# never written to argv, env capture, or logs.
[dependencies.db.credentials]
password = "{{env.PG_PASSWORD}}"

# Per-parent state directory (RFC §7.7 path rule).
[dependencies.db.state]
name = "data"

[targets.app]
needs   = ["db"]   # block target start until `db` is ready
[targets.app.env]
DATABASE_URL = "{{deps.db.runtime_exports.DATABASE_URL}}"
```

Key semantics:

| Field | Identity? | Lockfile records | Value source |
|-------|-----------|------------------|--------------|
| `parameters.<key>` | YES | resolved value | author / `{{env.X}}` from `required_env` |
| `credentials.<key>` | NO | template form only | host env via `{{env.X}}` (literals lock-fail) |
| provider `runtime_exports.<key>` | NO (excluded from `intrinsic_keys`) | not in lock | provider, post-start |
| provider `identity_exports.<key>` | YES | resolved value | provider, lock-time from `parameters` |

Hard invariants enforced fail-closed at lock time:

- `credentials.<key>.default` is forbidden
- `{{credentials.X}}` may not appear in `identity_exports` values
- `{{env.X}}` in dep blocks must reference a key in manifest top-level
  `required_env`
- credential literals (non-template) in consumer manifest fail the lock
- `unix_socket = "auto"` and `ready.type = "http" | "unix_socket"` are
  reserved-only and fail the lock

### `[contracts."<name>@<major>"]` — provider side

Capsules that act as providers (e.g. `ato/postgres@16`) declare the
contract they implement:

```toml
[targets.server]
runtime = "source"
driver  = "native"
# {{port}} is allocated by the orchestrator. {{credentials.password}}
# is rewritten by the orchestrator to a Rule M1 TempFile path; the
# provider reads the password from that file (e.g. initdb --pwfile=).
run = "./bootstrap.sh {{state.dir}} {{port}} {{credentials.password}}"

[contracts."service@1"]
target = "server"
ready  = { type = "probe", run = "pg_isready -h 127.0.0.1 -p {{port}}", timeout = "30s" }

[contracts."service@1".parameters]
database = { type = "string", required = true }

[contracts."service@1".credentials]
password = { type = "string", required = true }

[contracts."service@1".identity_exports]
database = "{{params.database}}"
protocol = "postgresql"
major    = "16"

[contracts."service@1".runtime_exports]
PGHOST = "{{host}}"
PGPORT = "{{port}}"

[contracts."service@1".runtime_exports.DATABASE_URL]
value  = "postgresql://postgres:{{credentials.password}}@{{host}}:{{port}}/{{params.database}}"
secret = true   # redact in logs / receipts / explain output

[contracts."service@1".state]
required = true
version  = "16"   # state schema version (independent of provider capsule version)
```

`ready.type` accepts `tcp` and `probe` in v1; `http` and `unix_socket`
are reserved (lock-fail-closed). v1 implements one materialization
channel for credentials (TempFile); the parsed grammar reserves Stdin
and EnvVar variants for follow-up.

### `[foundation_requirements]` — conformance assertions

Declares which Foundation-approved runtime profile and engine versions this capsule requires. A conformant ato implementation rejects execution if it cannot satisfy these constraints.

```toml
[foundation_requirements]
profile  = "std.secure"
runtimes = ["python@>=3.11", "node@>=20"]
engines  = ["nacelle@>=0.4"]
```

### `[build]` — packaging behavior

Controls how the capsule is packaged at publish time.

```toml
[build]
gpu = true  # apply GPU-oriented packaging defaults

[build.lifecycle]
prepare = "pip install -r requirements.txt"
build   = "python compile.py"
package = "ato pack"

[build.inputs]
lockfiles    = ["requirements.lock"]
toolchain    = "python@3.12"
allow_network = false

[build.outputs]
capsule     = "dist/capsule.atoc"
sha256      = true
attestation = true
```

### `[pack]` — payload filter

```toml
[pack]
include = ["src/**", "requirements.txt"]
exclude = ["**/__pycache__", "*.pyc", "tests/**"]
```
