# ato

[![CI](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=main)](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/releases)
[![GitHub stars](https://img.shields.io/github/stars/ato-run/ato-cli?style=social)](https://github.com/ato-run/ato-cli/stargazers)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue)](LICENSE)

**Run any project instantly. Share it with one URL.**

Point `ato` at a Python script, a Node app, a Rust binary, or a GitHub repo — it figures out the runtime, bootstraps only what's needed, and runs in a sandboxed environment. No Dockerfile. No setup guide. No manual environment.

[Install](#install) · [Quick start](#quick-start) · [Why Ato](#why-ato) · [Commands](#core-commands) · [Contributing](#contributing)

## Demo

![Demo](assets/demo.svg)

## Install

```bash
curl -fsSL https://ato.run/install.sh | sh
```

Or download a prebuilt binary from the [Releases page](https://github.com/ato-run/ato-cli/releases/latest) and place `ato` on your `PATH`.

## Quick start

```bash
# Install (one line)
curl -fsSL https://ato.run/install.sh | sh
```

**Consuming — run someone else's project:**

```bash
# Try once in a sandbox
ato run github.com/owner/repo
ato run https://ato.run/s/demo@r1       # runnable workspaces only (see note below)

# Keep it locally
ato decap https://ato.run/s/demo@r1 --into ./demo
ato run ./demo
```

**Producing — share your own project:**

```bash
# Run a Python script — no venv, no pip install
printf 'print("hello from ato")\n' > hello.py
ato run hello.py

# Capture the workspace and get a shareable URL
ato encap
# → Share URL: https://ato.run/s/hello-ato@r1
```

## Why Ato

Every time you share a project, someone has to set up an environment before they can run it — virtualenvs, `node_modules`, container builds, README instructions that drift. Ato removes that layer entirely.

Ato reads your project directly — `pyproject.toml`, `package.json`, `deno.json`, `Cargo.toml`, a bare script — and materializes only the runtime it needs. No config to write. For Python and native binaries, execution routes through [Nacelle](https://github.com/ato-run/nacelle), a sandboxed runtime that blocks unapproved filesystem and network access by default. `ato encap` captures a reproducible workspace descriptor that anyone can restore with `ato decap`.

### Mental model: Try → Keep → Share

Four commands map to two axes — the direction (consume vs. produce) and the persistence of the result (ephemeral vs. persistent):

|                                  | Just try it (ephemeral)                       | Set it up (persistent)                |
|----------------------------------|-----------------------------------------------|---------------------------------------|
| **Consume** someone else's code  | `ato run <url>` *(runnable workspaces only)*  | `ato decap <url>` *(any workspace)*   |
| **Produce** your own code        | `ato run .`                                   | `ato encap` *(→ `ato publish` later)* |

And the classic pain point comparison:

| Without Ato | With Ato |
|---|---|
| Clone → read README → install deps → run | `ato run github.com/owner/repo` |
| Write Dockerfile or setup script to share | `ato encap` |
| Follow multi-step setup to reproduce | `ato decap <share-url>` |

Supported runtimes today: Python (`pyproject.toml`, `uv.lock`, single-file PEP 723), Node / TypeScript / Deno, Rust, Go, static web, WebAssembly, and shell scripts.

## Core commands

Commands are ordered by the Try → Keep → Share journey.

### Try it with `ato run`

`ato run` accepts a local path, a share URL, or a GitHub repository reference. It covers two distinct use cases:

**Run it — try any project in a sandbox (consume):**

```bash
ato run hello.py
ato run github.com/owner/repo
ato run https://ato.run/s/demo@r1
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

`--watch` and `--background` are only available for local filesystem paths. `ato run <share-url>` does not support them in the current MVP path.

> **When `run` works on a share URL**
>
> `ato run <share-url>` works only when the shared workspace is declared as runnable in its `capsule.toml` — specifically, `type = "app"` or `"tool"` with an entrypoint defined via `run`, `[targets.*]`, or `[services]`.
>
> Workspaces without an entrypoint (libraries, datasets, templates, `type = "library"`) are still shareable, but receivers must use `ato decap` to expand them and run locally. `ato run` fails closed before launch if the share is not runnable. See [Runnable workspace](#runnable-workspace) for the full rules.

### Keep it with `ato decap`

`ato decap` materializes a share into a target directory, verifies it, and runs declared install steps. Use this when you want a persistent copy, or when the share is not runnable.

```bash
ato decap https://ato.run/s/myproject@r1 --into ./my-project
ato decap .ato/share/share.spec.json --into ./my-project
```

### Share it with `ato encap`

`ato encap` captures the current workspace as a portable share descriptor, uploads it, and prints a share URL. Run it from the project directory — no arguments needed.

```bash
ato encap
```

To control visibility:

```bash
ato encap --internal   # organisation-internal access
ato encap --private    # authenticated owner only
ato encap --local      # local save only, no upload
```

Local capture output is written under `.ato/share/`:

- `share.spec.json`
- `share.lock.json`
- `guide.md`

Secrets are never uploaded. Ato records contracts such as required environment files, but not secret values.

#### `encap` vs `publish`

- `ato encap` — turn a workspace into a **shareable descriptor**. Choose between a local save (`--local`) and an upload that returns a share URL. Intended for ad-hoc sharing and reproducibility.
- `ato publish` — release the workspace as a **capsule to the registry**. Involves versioning, signing, and CI integration. Intended for distribution as a named artifact.

Use `encap` for private/informal sharing; use `publish` when you want a durable, versioned release.

## Security and isolation

Ato is fail-closed by default.

- Sandbox isolation: Tier 2 targets such as `source/python`, `web/python`, and `source/native` run through Nacelle.
- Filesystem protection: unknown code does not get unrestricted host access by default.
- Network control: unapproved network access is blocked under strict enforcement.
- Environment handling: missing required environment variables stop execution before launch, and `--prompt-env` can collect them interactively.

For normal local runs, Ato usually bootstraps a compatible Nacelle release automatically when Tier 2 execution requires it. In CI or offline environments, auto-bootstrap is intentionally restricted, so preinstall or register Nacelle ahead of time if needed.

## From source

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

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).

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

`ato run <share-url>` launches a shared workspace directly only if its `capsule.toml` declares an entrypoint. A workspace is **runnable** when at least one of the following is true:

- A top-level `run = "..."` is defined
- `[targets.*]` declares at least one executable target
- `[services]` is defined

Workspaces that satisfy none of the above are still shareable via `ato encap`, but receivers must use `ato decap` to expand them before executing anything locally. `type = "library"` is always treated as non-runnable regardless of other fields.

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
