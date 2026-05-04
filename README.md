# ato

[![Rust CI](https://github.com/ato-run/ato/actions/workflows/rust-ci.yml/badge.svg?branch=main)](https://github.com/ato-run/ato/actions/workflows/rust-ci.yml)

Run a project before you set it up.

`ato` is a command-line tool for trying local projects, GitHub repositories,
and shared app links in a controlled runtime. It detects what the project needs,
prepares the missing tools, and runs it without asking you to manually install
Python, Node, Rust, or other project-specific dependencies first.

```bash
ato run .                      # run the current project
ato run github.com/owner/repo  # try a GitHub repository
ato run https://ato.run/s/demo # open a shared ato app
```

`ato` is useful when you want to:

- try a repository without reading its setup instructions first
- share a runnable project with someone else
- run a project with a repeatable setup
- keep the project's runtime separate from your machine as much as possible

> ato is still pre-1.0. Some sandboxing and network controls are still being
> completed. See [Known limitations](crates/ato-cli/docs/known-limitations.md)
> before using ato with untrusted code.

## Install

macOS / Linux:

```bash
curl -fsSL https://ato.run/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://ato.run/install.ps1 | iex
```

Homebrew:

```bash
brew install ato-run/ato/ato-cli
```

From source:

```bash
cargo build -p ato-cli --release
```

Check that it works:

```bash
ato --help
```

To uninstall an `install.sh` deployment:

```bash
ato uninstall
ato uninstall --purge
ato uninstall --purge --include-config --include-keys --yes
```

## Quick Start

### Run the Current Directory

```bash
cd my-project
ato run .
```

ato will inspect the project, prepare what it needs, and start it.

### Run a GitHub Repository

```bash
ato run github.com/owner/repo
```

This is useful for trying examples, demos, small tools, or projects you do not
want to install globally.

### Create a Lock File

```bash
ato lock .
```

A lock file records the resolved runtime setup for the project. Commit it when
you want other people or CI to run the project the same way.

### Share a Project

```bash
ato encap .
```

`encap` captures the project into a shareable description.

```bash
ato decap https://ato.run/s/demo --into ./demo
```

`decap` materializes a shared project into a local directory.

## How It Works

ato turns a project into a runnable plan.

```text
project files
  |
  v
detect what the project needs
  |
  v
resolve tools and runtimes
  |
  v
write a lock file
  |
  v
run in a controlled environment
```

In practice, this means ato tries to answer these questions for you:

1. What kind of project is this?
2. What tools or runtimes are needed?
3. Can the result be recorded so the next run is repeatable?
4. What access should the running project have to the host machine?

The main file ato looks for is:

```text
capsule.toml
```

A `capsule.toml` describes how a project should run. If a project does not have
one, ato can try to infer a basic setup.

## Examples

### Python Script

```bash
ato run ./scripts/report.py
```

### Node App

```bash
ato run ./examples/web
```

### Rust Project

```bash
ato run ./crates/my-tool
```

### Shared App

```bash
ato run https://ato.run/s/demo
```

## What ato Is Not

ato is not a full replacement for every tool in your stack.

- It is not Docker. It does not require writing a Dockerfile first.
- It is not Nix. It focuses on running and sharing projects, not replacing your whole system environment.
- It is not just `npx` or `uvx`. It can run whole projects, not only single packages.
- It is not a remote development environment. It runs locally.

ato sits between these tools: it gives you a fast way to try, lock, and share a
project without turning the project into a container image or asking every user
to reproduce the setup by hand.

## Safety Model

ato is designed to reduce accidental access to your machine, but it should not
be treated as a perfect security boundary yet.

Current behavior:

- project files are run through ato's runtime path instead of directly on your host
- common secret files such as `.env`, `.env.*`, private keys, and credentials files
  are excluded from capsule archives by default
- some OS-level isolation is available for source runtimes
- deny-all networking is supported for supported runtime paths

Known gaps in the current version:

- hostname allowlists for source runtimes are not fully enforced yet
- missing required environment variables may warn instead of stopping the run
- stricter sandbox mode is not available for every runtime
- some Desktop builds are still beta-quality on non-macOS platforms

Read the full list here:

```text
crates/ato-cli/docs/known-limitations.md
```

When running code you do not trust, prefer:

```bash
ato run github.com/owner/repo --no-build
```

or inspect the repository first.

## Common Commands

```bash
ato run .                  # run a local project
ato run github.com/o/r     # run a GitHub repository
ato lock .                 # generate a lock file
ato encap .                # create a shareable project description
ato decap <share> --into . # materialize a shared project
ato ps                     # list running apps
ato stop --all             # stop running apps
ato logs                   # show logs
```

## Repository Layout

This repository contains the CLI, runtime libraries, desktop app, and supporting
tools.

```text
ato/
├── crates/
│   ├── ato-cli/          # command-line interface
│   ├── capsule-core/     # project detection, locking, packing, runtime logic
│   ├── capsule-wire/     # small shared message types
│   ├── ato-session-core/ # session process and state helpers
│   ├── ato-desktop/      # desktop app
│   └── nacelle/          # source runtime sandbox
├── sidecars/
│   └── ato-tsnetd/       # optional network sidecar
├── docs/
│   └── rfcs/             # design notes and proposals
└── .github/workflows/    # CI
```

Most users only need `ato-cli`.

## Develop

```bash
cargo check --workspace --all-targets
cargo test -p ato-cli
cargo test -p capsule-core
cargo run -p ato-cli -- run .
```

Run the desktop app:

```bash
cargo run -p ato-desktop
```

Build the CLI:

```bash
cargo build -p ato-cli --release
```

Bundle the desktop app:

```bash
cargo xtask bundle darwin-arm64
cargo xtask bundle windows-x86_64
cargo xtask bundle linux-x86_64
```

## Documentation

- [Known limitations](crates/ato-cli/docs/known-limitations.md)
- [Core architecture](docs/core-architecture.md)
- [Design RFCs](https://ato-run.github.io/ato/)
- [RFC sources](docs/rfcs/)
- [Glossary](docs/GLOSSARY.md)
- [Contributing guidelines](AGENTS.md)

## License

This repository uses per-component licensing:

| Component | License |
|---|---|
| `capsule-wire` | Apache-2.0 |
| `ato-cli` | Apache-2.0 OR MPL-2.0 |
| `capsule-core` | MPL-2.0 |
| `nacelle` | MPL-2.0 |
| `ato-desktop` | MPL-2.0 |
| Hosted registry/backend services | Private or separately commercial-licensed |
