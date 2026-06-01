<div align="center">
  <br />
  <img src="./docs/ato-logo.png" alt="Ato logo" width="132" height="132" />
  <h1>Ato</h1>
  <p>
    <strong>GitHub projects, runnable without setup.</strong>
  </p>
  <p>
    A local-first, source-native execution runtime for apps and developer workflows.<br />
    Discover. Run. Share. All without leaving your machine.
  </p>

  <p>
    <a href="https://github.com/ato-run/ato/actions/workflows/rust-ci.yml">
      <img alt="Rust CI" src="https://github.com/ato-run/ato/actions/workflows/rust-ci.yml/badge.svg?branch=main" />
    </a>
    <img alt="Local-first" src="https://img.shields.io/badge/local--first-yes-e91e63" />
    <img alt="Source-native" src="https://img.shields.io/badge/source--native-runtime-f97316" />
    <img alt="Desktop + CLI" src="https://img.shields.io/badge/Desktop%20%2B%20CLI-supported-f59e0b" />
    <img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-111827" />
  </p>

  <p>
    <a href="#install"><strong>Install</strong></a> ·
    <a href="#quick-start"><strong>Quick start</strong></a> ·
    <a href="#how-it-works"><strong>How it works</strong></a> ·
    <a href="#safety-model"><strong>Safety</strong></a> ·
    <a href="#develop"><strong>Develop</strong></a>
  </p>
  <br />
</div>

---

## Why Ato?

<table>
  <tr>
    <td width="25%">
      <h3>Run GitHub projects</h3>
      <p>Try a repository before you clone, install, or patch your machine.</p>
    </td>
    <td width="25%">
      <h3>Share capsules</h3>
      <p>Package a runnable project description and let someone else run the same thing.</p>
    </td>
    <td width="25%">
      <h3>Repeatable environments</h3>
      <p>Record the resolved runtime setup so a run can be inspected and repeated.</p>
    </td>
    <td width="25%">
      <h3>Desktop + CLI</h3>
      <p>Use a polished desktop control plane or stay in the terminal.</p>
    </td>
  </tr>
</table>

Ato is a command-line tool and desktop runtime for trying local projects, GitHub repositories, and shared app links in a controlled runtime. It detects what the project needs, prepares missing tools, and runs it without asking you to manually install Python, Node, Rust, or other project-specific dependencies first.

```bash
ato run .                      # run the current project
ato run github.com/owner/repo  # try a GitHub repository
ato run https://ato.run/s/demo # open a shared Ato app
```

Ato is useful when you want to try a repository without reading its setup instructions first, share a runnable project with someone else, run a project with a repeatable setup, or keep the project's runtime separate from your machine as much as possible.

> Ato is still pre-1.0. Some sandboxing and network controls are still being completed. See [Known limitations](crates/ato-cli/docs/known-limitations.md) before using Ato with untrusted code.

## Install

```bash
curl -fsSL https://ato.run/install.sh | sh
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
```

## Quick Start

<table>
  <tr>
    <td width="50%">
      <h3>Run the current directory</h3>

```bash
cd my-project
ato run .
```

Ato inspects the project, prepares what it needs, and starts it.
    </td>
    <td width="50%">
      <h3>Run a GitHub repository</h3>

```bash
ato run github.com/owner/repo
```

Useful for examples, demos, small tools, or projects you do not want to install globally.
    </td>
  </tr>
</table>

### Create a lock file

```bash
ato lock .
```

A lock file records the resolved runtime setup for the project. Commit it when you want other people or CI to run the project the same way.

### Share a project

```bash
ato workspace share
```

`workspace share` captures the project into a shareable description.

```bash
ato workspace setup https://ato.run/s/demo --into ./demo
```

`workspace setup` materializes a shared project into a local directory.

> Older docs may refer to `ato encap` and `ato decap`. Prefer `ato workspace share` and `ato workspace setup` for new examples.

## How it works

<div align="center">

```text
Discover              Run                         Share
   │                   │                            │
   ▼                   ▼                            ▼
source/project ──▶ execution plan ──▶ controlled runtime ──▶ capsule/share
```

</div>

Ato turns a project into a runnable plan.

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

In practice, Ato tries to answer these questions:

1. What kind of project is this?
2. What tools or runtimes are needed?
3. Can the result be recorded so the next run is repeatable?
4. What access should the running project have to the host machine?

The main file Ato looks for is:

```text
capsule.toml
```

A `capsule.toml` describes how a project should run. If a project does not have one, Ato can try to infer a basic setup.

## What is a capsule?

A capsule is a portable, inspectable description of how a project should run. It keeps source, runtime requirements, entrypoints, state expectations, and policy close enough together that another machine can reconstruct the launch without repeating the setup work by hand.

```toml
[targets.main]
runtime = "source"
driver = "node"
run = "npm run dev"
port = 5173
```

## Examples

### Python script

```bash
ato run ./scripts/report.py
```

### Node app

```bash
ato run ./examples/web
```

### Rust project

```bash
ato run ./crates/my-tool
```

### Shared app

```bash
ato run https://ato.run/s/demo
```

## What Ato is not

Ato is not a full replacement for every tool in your stack.

- It is not Docker. It does not require writing a Dockerfile first.
- It is not Nix. It focuses on running and sharing projects, not replacing your whole system environment.
- It is not just `npx` or `uvx`. It can run whole projects, not only single packages.
- It is not a remote development environment. It runs locally by default.

Ato sits between these tools: it gives you a fast way to try, lock, and share a project without turning the project into a container image or asking every user to reproduce the setup by hand.

## Safety Model

Ato is designed to reduce accidental access to your machine, but it should not be treated as a perfect security boundary yet.

Current behavior:

- project files are run through Ato's runtime path instead of directly on your host
- common secret files such as `.env`, `.env.*`, private keys, and credentials files are excluded from capsule archives by default
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

## Common commands

```bash
ato run .                       # run a local project
ato run github.com/o/r          # run a GitHub repository
ato lock .                      # generate a lock file
ato workspace share             # create a shareable project description
ato workspace setup <share>     # materialize a shared project
ato ps                          # list running apps
ato stop --all                  # stop running apps
ato logs                        # show logs
```

## Repository layout

This repository contains the CLI, runtime libraries, desktop app, and supporting tools.

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
cargo run -p ato-cli -- --help
```

Desktop bundle examples:

```bash
cd crates/ato-desktop
cargo xtask bundle darwin-arm64
cargo xtask bundle windows-x86_64
```

## Contributing

Issues and pull requests are welcome. For larger changes, open an issue first so the runtime, CLI, Desktop, and capsule semantics can be discussed together.

## License

Apache-2.0
