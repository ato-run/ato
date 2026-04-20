# Ato CLI

[![CI](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=dev)](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/releases)
[![GitHub stars](https://img.shields.io/github/stars/ato-run/ato-cli?style=social)](https://github.com/ato-run/ato-cli/stargazers)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue)](LICENSE)

Ato is a CLI for running shared workspaces, GitHub repositories, and local projects in isolated environments. It infers the runtime from the project, bootstraps only what it needs, and defaults to fail-closed execution so you can try, share, and rebuild software without hand-written containers or custom package recipes.

Use `ato run` when you want to try code now, `ato encap` when you want to capture and share a workspace, and `ato decap` when you want to rebuild that workspace locally.

[Quick start](#quick-start) · [Why Ato](#why-ato) · [Core commands](#core-commands) · [Contributing](#contributing) · [License](#license)

## Demo

[![Demo](https://img.shields.io/badge/demo-asciinema-orange)](assets/ato-demo.cast)

Illustrative terminal walkthrough: install Ato, run a local script, then capture the workspace.

## Install

Install the latest Ato release in one line:

```bash
curl -fsSL https://ato.run/install.sh | sh
```

Prefer a manual install path? Download a prebuilt binary from the [GitHub Releases page](https://github.com/ato-run/ato-cli/releases/latest) and place `ato` on your `PATH`.

## Quick start

This path stays local, takes about a minute, and shows the core value of `ato run`.

```bash
curl -fsSL https://ato.run/install.sh | sh

mkdir hello-ato
cd hello-ato
printf 'print("hello from ato")\n' > hello.py

ato run hello.py
```

Next steps after that first run:

```bash
# Capture the current workspace
ato encap . --share

# Rebuild a shared workspace later
ato decap https://ato.run/s/<share-id>@r1 --into ./hello-ato-copy
```

## Why Ato

Most developer workflows ask you to write a container, package recipe, or project-specific setup guide before someone else can run the code. Ato starts from the project itself. It recognizes common Python, Node, Deno, Rust, static web, and single-script layouts, then materializes only what the target needs.

That makes Ato useful in three moments that usually get split across different tools: trying code immediately, sharing a runnable workspace, and rebuilding that workspace later with the same declared boundaries. For higher-risk targets such as Python and native binaries, Ato routes execution through [Nacelle](https://github.com/ato-run/nacelle) and keeps fail-closed defaults around filesystem, network, and environment access.

## Core commands

### Run something now with `ato run`

`ato run` accepts a local path, a share URL, or a GitHub repository reference.

```bash
ato run .
ato run hello.py
ato run github.com/owner/repo
ato run https://ato.run/s/demo@r1
```

For local filesystem paths, Ato also supports `--watch` and `--background`.

```bash
ato run . --watch
ato run . --background
ato ps
ato logs --id <capsule-id> --follow
ato stop --id <capsule-id>
```

`ato run <share-url>` does not support `--watch` or `--background` in the current MVP path.

### Share a workspace with `ato encap`

`ato encap` captures the current workspace as a portable share descriptor, writes local share files, and can upload them to a share URL.

```bash
ato encap . --share
```

Local capture output is written under `.ato/share/`:

- `share.spec.json`
- `share.lock.json`
- `guide.md`

Secrets are never uploaded. Ato records contracts such as required environment files, but not secret values.

### Rebuild a workspace with `ato decap`

`ato decap` materializes a share into a target directory, verifies the share, and runs declared install steps.

```bash
ato decap https://ato.run/s/myproject@r1 --into ./my-project
ato decap .ato/share/share.spec.json --into ./my-project
```

## What Ato can run well today

Ato's inference path already covers common cases such as:

- share URLs using `https://ato.run/s/...`
- GitHub repositories using `github.com/owner/repo`
- single-file Python scripts, including PEP 723 metadata
- TypeScript and JavaScript projects detected from `deno.json`, `package.json`, and lockfiles
- Python projects detected from `pyproject.toml` and `uv.lock`
- Rust, Go, static web, WebAssembly, and other lock-first project layouts

When Ato can identify a reproducible execution path, it routes the workspace through the same capsule-oriented runtime model.

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

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
