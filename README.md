# ato

[![Rust CI](https://github.com/ato-run/ato/actions/workflows/rust-ci.yml/badge.svg?branch=main)](https://github.com/ato-run/ato/actions/workflows/rust-ci.yml)

Run software from recipes.

`ato` is a source-native runtime for running local projects, GitHub repositories,
and shared recipes in a controlled local environment. `ato run` is an ephemeral
local production rehearsal: it resolves, materializes, launches, and records a
session without registering a durable installed app.

A recipe describes how source inputs become an execution: which source tree to
use, which tools and runtimes to prepare, what to build, what services to start,
what environment to expose, what filesystem and network access to allow, and
which command to launch.

```bash
ato run .                      # run the current source with its local recipe
ato run github.com/owner/repo  # try a GitHub repository
ato run https://ato.run/s/demo # open a shared recipe
```

`ato` is useful when you want to:

- try a repository without reconstructing its setup by hand
- share a runnable interpretation of source code
- run the same source in different ways: web app, CLI, desktop app, demo, or service
- keep runtimes, dependencies, and state separate from your machine
- compare launch conditions through execution identity

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

Ato looks for a local recipe such as `capsule.toml`. If one is not present, it
can infer a basic recipe from the source tree. This creates a run session, not
an installed app.

### Run a GitHub Repository

```bash
ato run github.com/owner/repo
```

Ato resolves the source, finds or infers a compatible recipe, prepares the needed
tools and runtimes, and launches it in a controlled session.

### Create a Lock File

```bash
ato lock .
```

A lock file records the resolved setup for a recipe. Commit it when you want
other people or CI to resolve the same launch conditions.

### Share a Recipe

```bash
ato encap .
```

`encap` captures a runnable description of the current source and recipe.

```bash
ato decap https://ato.run/s/demo --into ./demo
```

`decap` materializes a shared recipe into a local directory.

## Recipes

A recipe is the unit Ato shares, stores, reviews, and runs.

A source repository is only the raw material. A recipe is the executable
interpretation of that source. The same repository can have multiple recipes:
one for the web app, one for the CLI, one for a desktop shell, one for a demo
with a local database, and one for a safer no-network mode.

In local projects, a recipe is usually written as:

```text
capsule.toml
```

Despite the file name, `capsule.toml` is not a one-to-one description of a
repository. A repository may contain zero, one, or many recipes. Recipes may also
live outside the repository and be shared through the Ato Store.

```text
source inputs
  + recipe
  + user environment
  = execution
```

This is why Ato shares recipes rather than opaque images. Different recipes can
turn the same source into different runnable forms.

## How It Works

Ato turns source plus recipe into a launch graph.

```text
source inputs
  |
  v
select or infer recipe
  |
  v
project into the user's environment
  |
  v
resolve tools, runtimes, dependencies, services, and policy
  |
  v
materialize a managed session
  |
  v
record execution identity and receipt
```

In practice, Ato tries to answer these questions:

1. What source inputs are being used?
2. Which recipe interprets those inputs?
3. What tools, runtimes, services, and build outputs are needed?
4. What environment, filesystem, network, and host capabilities are allowed?
5. Can the resolved launch be recorded and compared later?
6. Should this launch attach to an existing healthy session or start a new one?

The local recipe file is usually:

```text
capsule.toml
```

A `capsule.toml` describes how source inputs should be arranged, built,
configured, and launched. It is a recipe, not a permanent one-to-one identity
for the repository.

## Execution Identity

Ato does not only identify source code. It identifies launches.

An execution identity is a stable fingerprint of the resolved launch world:
the recipe snapshot, source input snapshots, dependency outputs, runtime identity,
environment closure, filesystem view, network policy, capability policy,
entrypoint, arguments, working directory, and state bindings.

This lets you ask:

```text
Did we launch the same world?
```

That is different from asking whether two users cloned the same repository.
The same source can produce different executions when the recipe, environment,
policy, runtime, or state changes.

Execution receipts are stored by `execution_id` and can be used by collaborators,
CI, Desktop, and agents to compare launch conditions.

## Run vs Install

`ato run` is for rehearsing a resolved launch locally. It may leave session
records, logs, receipts, and reusable cache/materialization entries, but it does
not silently register a durable app.

`ato install` is for keeping an app locally. It creates or updates installed-app
identity, profile state, and immutable install revisions that future launches
can address through the installed-app lifecycle.

`ato dev` is not part of this contract yet. File watching and hot reload remain
experimental run options or target-owned behavior until a separate development
mode is specified.

## Ato Desktop

Ato Desktop is the graphical shell for managed Ato sessions.

In 0.6.0, Desktop is centered on the running recipe execution: the app view,
session status, logs, lifecycle controls, capsule details, and execution identity
are shown in one focused surface.

Desktop is not a separate execution engine. It delegates execution to the same
CLI launch pipeline as `ato run` and `ato session start`.

```text
Desktop
  |
  v
ato CLI
  |
  v
recipe + source inputs
  |
  v
launch graph
  |
  v
managed session
```

A window presents a session; it does not necessarily own the process lifecycle.
Closing a window can detach from a session, while stopping a session explicitly
terminates the managed process.

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

### Shared Recipe

```bash
ato run https://ato.run/s/demo
```

### Project With a Dependency Service

A capsule can declare service dependencies that ato starts automatically.
For example, a FastAPI backend that needs Postgres:

```toml
# capsule.toml
schema_version = "0.3"
name           = "myapp"
type           = "app"
required_env   = ["PG_PASSWORD"]   # host env passthrough for dep credentials

[dependencies.db]
capsule  = "capsule://ato/postgres@16"
contract = "service@1"

[dependencies.db.parameters]
database = "myapp"

[dependencies.db.credentials]
password = "{{env.PG_PASSWORD}}"

[dependencies.db.state]
name = "data"

[targets.app]
runtime = "source"
driver  = "python"
run     = "python -m uvicorn main:app --host 127.0.0.1 --port 8000"
needs   = ["db"]

[targets.app.env]
DATABASE_URL = "{{deps.db.runtime_exports.DATABASE_URL}}"
```

```bash
export PG_PASSWORD=$(openssl rand -hex 16)
ato run .
```

ato starts Postgres for you, allocates a port, derives a per-project
state directory, materializes the credential into a 0600 temp file,
runs `pg_isready`, then injects the resolved `DATABASE_URL` into your
target's environment. Rotating `PG_PASSWORD` does not invalidate the
existing data — credentials are kept out of the lock identity by
construction.

See [`docs/rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md`](docs/rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md)
for the full grammar and safety model. Provider authoring for
`service@1` is documented in the same RFC, §11.2.

## Store and Shared Recipes

The Ato Store shares recipes.

A recipe can target a public GitHub repository, a source snapshot, generated
build outputs, or other declared inputs. Store entries are not just apps and not
just source code; they are reviewed ways to run source.

This makes community recipes possible:

```text
same source repository
  ├─ web demo recipe
  ├─ CLI recipe
  ├─ desktop recipe
  ├─ local database recipe
  └─ no-network recipe
```

Because recipes can request environment variables, filesystem access, network
access, services, and host capabilities, recipe authorship and requested
permissions are part of the trust model.

## What Ato Is Not

Ato is not a full replacement for every tool in your stack.

- It is not Docker. It does not ask you to turn source into an opaque image first.
- It is not Nix. It focuses on source-native launch recipes, not replacing your whole system environment.
- It is not just `npx` or `uvx`. It can run whole projects, services, and multi-target recipes.
- It is not a remote development environment. It runs locally.

Ato sits in the launch layer. It takes source inputs and recipes, resolves them
against the user's environment, and records the resulting execution identity.

## Safety Model

Ato is designed to make host access explicit, but it should not be treated as a
perfect security boundary yet.

A recipe is executable configuration. It can define build commands, run commands,
services, environment access, filesystem grants, network policy, and host bridge
capabilities. Treat recipes from other people with the same care you would apply
to source code.

Current behavior:

- project files are run through Ato's runtime path instead of directly on your host
- common secret files such as `.env`, `.env.*`, private keys, and credentials files
  are excluded from capsule archives by default
- some OS-level isolation is available for source runtimes
- deny-all networking is supported for supported runtime paths
- requested permissions are part of the launch graph and execution identity

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
ato run .                  # rehearse a local project in an ephemeral session
ato run github.com/o/r     # rehearse a GitHub repository
ato install publisher/slug # register a durable local app
ato lock .                 # generate a lock file
ato encap .                # create a shareable recipe description
ato decap <share> --into . # materialize a shared recipe
ato ps                     # list running sessions
ato stop --all             # stop running sessions
ato logs                   # show logs
```

## Repository Layout

This repository contains the CLI, runtime libraries, desktop app, and supporting
tools.

```text
ato/
├── crates/
│   ├── ato-cli/          # command-line interface
│   ├── capsule-core/     # recipe parsing, locking, packing, runtime logic
│   ├── capsule-wire/     # small shared message types
│   ├── ato-session-core/ # session process and state helpers
│   ├── ato-desktop/      # desktop session shell
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

- [Run](docs/run.md)
- [Execution Identity](docs/execution-identity.md)
- [Desktop](docs/desktop.md)
- [Sandbox](docs/sandbox.md)
- [Known limitations](crates/ato-cli/docs/known-limitations.md)
- [Core architecture](docs/core-architecture.md)
- [Capsule dependency contracts](docs/rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md) — declaring service deps in `capsule.toml`
- [Docs site](https://ato-run.github.io/ato/)
- [Design RFCs](docs/rfcs/)
- [Glossary](docs/glossary-reference.md)
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
