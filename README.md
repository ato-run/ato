# Ato: The Agentic Meta-Runtime 🚀

[![CI](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=dev)](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/releases)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org/)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-6f42c1)
[![Issues](https://img.shields.io/github/issues/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/issues)
[![Last Commit](https://img.shields.io/github/last-commit/ato-run/ato-cli/dev)](https://github.com/ato-run/ato-cli/commits/dev)

English | [日本語](README_JA.md)

> The Nix alternative for the AI era. Pass a URL, get a secure runnable environment in seconds.

Ato turns source code into a runnable environment without asking users to maintain a heavy container build or learn a custom package language. It infers the runtime and dependencies, materializes only what is needed, and runs with fail-closed defaults. For Tier 2 targets such as Python and native binaries, Ato uses the Nacelle sandbox and will normally bootstrap a compatible engine automatically when it is needed.

This README focuses on the MVP path: `run`, `encap`, and `decap`. Advanced store and publishing flows stay out of scope here.

---

## Three commands

### 1. Run anything now with `ato run`

`ato run` executes a share URL, a GitHub repository, or a local script without polluting your machine. Dependencies are resolved in an isolated path and the execution is treated as disposable by default.

```bash
# Run a shared workspace directly
ato run https://ato.run/s/demo@r1

# Run a GitHub repository directly
ato run github.com/user/my-app

# Run a single local script
ato run scrape.py
```

Use `ato run` when you want to try something immediately. If you want the files on disk, use `ato decap` instead.

### 2. Share your current workspace with `ato encap`

`ato encap` captures the current workspace as a portable share descriptor, writes the local share files, and can upload them to a share URL.

```bash
# Capture the current workspace and upload it
ato encap . --share
# -> https://ato.run/s/myproject@r1
```

The local capture is written under `.ato/share/`:

- `share.spec.json`
- `share.lock.json`
- `guide.md`

Secrets are never uploaded. Ato records contracts such as required env files, but not secret values.

### 3. Rebuild the workspace locally with `ato decap`

`ato decap` materializes a shared workspace into a target directory. This is more than unpacking an archive: Ato restores the workspace layout, verifies the share, and runs the declared install steps.

```bash
# Materialize from a share URL
ato decap https://ato.run/s/myproject@r1 --into ./my-project

# Materialize from a local share descriptor
ato decap .ato/share/share.spec.json --into ./my-project
```

---

## What Ato handles well

Ato's inference engine already covers common cases such as:

- Share URLs using `https://ato.run/s/...`
- GitHub repositories using `github.com/owner/repo`
- Single-file Python scripts, including PEP 723 metadata
- TypeScript and JavaScript projects detected from `deno.json`, `package.json`, and lockfiles
- Python projects detected from `pyproject.toml` and `uv.lock`
- Rust, Go, static web, WebAssembly, and other lock-first project layouts

When Ato can identify a reproducible execution path, it routes the workspace through the same capsule-oriented runtime model.

---

## Quick start

Install `ato` with the one-line installer:

```bash
curl -fsSL https://ato.run/install.sh | sh
```

Or download a prebuilt binary from the [GitHub Releases page](https://github.com/ato-run/ato-cli/releases/latest) and place it on your `PATH`.

For contributors or local development from source:

```bash
# Build the CLI
cargo build -p ato-cli

# Run the current directory
./target/debug/ato run .

# Watch mode for local development
./target/debug/ato run . --watch

# Background process management
./target/debug/ato run . --background
./target/debug/ato ps
./target/debug/ato stop --id <capsule-id>
./target/debug/ato logs --id <capsule-id> --follow
```

---

## Primary command reference

The default CLI help intentionally highlights the smallest useful surface area:

```text
Usage: ato [OPTIONS] <COMMAND>

Primary Commands:
  run      Try something now
  decap    Set up a workspace locally
  encap    Share your current workspace

Management:
  ps       List running capsules
  stop     Stop a running capsule
  logs     Show logs of a running capsule
```

---

## Security model

Ato is fail-closed by default.

- Sandbox isolation: Tier 2 targets run through [Nacelle](https://github.com/ato-run/nacelle).
- Filesystem protection: unknown code can be run without giving it unrestricted host access by default.
- Network control: unapproved network access is blocked under strict enforcement.
- Environment handling: missing required environment variables stop execution before launch, and `--prompt-env` can collect them interactively.

For normal local runs, Ato will usually bootstrap a compatible Nacelle release automatically if Tier 2 execution requires it. In CI or offline environments, auto-bootstrap is intentionally restricted, so preinstall or register Nacelle ahead of time if needed.

---

## Runtime isolation tiers

Different runtimes require different isolation levels.

| Runtime family | Tier | Notes |
| --- | --- | --- |
| `web/static` | Tier 1 | Static preview and simple web targets |
| `web/deno`, `web/node`, `source/deno`, `source/node` | Tier 1 | Runs without manual sandbox bootstrap in the common path |
| `source/python`, `web/python`, `source/native` | Tier 2 | Requires Nacelle; normally auto-bootstrapped outside CI and offline modes |

Tier 1 targets run without bypass flags. Tier 2 targets use the stronger sandbox path.

---

## Contributing

Bug reports and feature requests are welcome in [GitHub Issues](https://github.com/ato-run/ato-cli/issues).

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
