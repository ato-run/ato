# What's Ato?

<p align="center"><strong>Run a project before you set it up.</strong></p>

<p align="center">
  <a href="https://github.com/ato-run/ato"><img src="https://img.shields.io/github/stars/ato-run/ato?style=social" alt="GitHub stars"></a>
  &nbsp;
  <a href="https://github.com/ato-run/ato/releases"><img src="https://img.shields.io/github/v/release/ato-run/ato?display_name=tag&color=6d57e6" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/license-Apache--2.0-111827" alt="License: Apache-2.0">
  <a href="https://github.com/ato-run/ato/discussions"><img src="https://img.shields.io/badge/community-Discussions-5865f2" alt="Community"></a>
</p>

<p align="center">
  <a href="#/?id=install"><strong>Install</strong></a> ·
  <a href="#/?id=quick-start"><strong>Quick start</strong></a> ·
  <a href="#/run"><strong>Run</strong></a> ·
  <a href="https://github.com/ato-run/ato"><strong>GitHub</strong></a> ·
  <a href="https://github.com/ato-run/ato/discussions"><strong>Community</strong></a>
</p>

![Concept](concept-image.png)

`ato` is a command-line tool and desktop app for running local projects, GitHub repositories, and shared app links *before* you set them up by hand.

It detects what the project needs, prepares missing tools and runtimes, and starts it in a controlled environment — without asking you to install Python, Node, Rust, or any project-specific dependency first.

---

## Install

**macOS / Linux**

```bash
curl -fsSL https://ato.run/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm https://ato.run/install.ps1 | iex
```

On a graphical desktop session the installer installs **Ato Desktop** (bundling the private `ato` runtime). Add `--with-cli` (`-WithCli` on Windows) to expose the `ato` command in your terminal, or `--cli-only` (`-CliOnly`) for headless/CI use. Homebrew (`brew install ato-run/ato/ato-cli`) installs the CLI only.

See the [project README](https://github.com/ato-run/ato#install) for version pinning, source builds, and uninstall.

## Quick start

```bash
# run the project in the current directory
ato run .

# run a GitHub repository — no clone required
ato run github.com/ato-run/hello-astro

# run a shared app link
ato run capsule://hello-astro@1.0.0
```

The same handle works whether the target is a local checkout, a remote repository, a Store reference, or a canonical capsule. Ato resolves the project, prepares its runtime, and launches it:

```text
$ ato run github.com/ato-run/hello-astro
✓ Resolving source
✓ Detecting project type
✓ Preparing runtime
✓ Installing dependencies
✓ Starting project

Running at http://localhost:5173
```

See [Run](run.md) for the full surface.

---

## The problem

A friend shares a project with you. You clone it. Then the ritual begins.

```
node: command not found
```

You install Node. Wrong version. You install `nvm`. You run `nvm use`. Now it wants Python. And then a native build tool you've never heard of. Forty minutes later, the app still won't start — and you haven't even looked at the code yet.

The usual escape hatches don't fit a quick "let me just try this":

- **Docker** works, but you don't want a daemon running on your laptop just to try someone's side project.
- **Nix** is reproducible, but learning Nix to run one repo feels like going to culinary school to make toast.

So you keep fighting the environment — one missing dependency at a time. **This is the problem Ato was built to solve.**

## The insight

The frustration isn't really about missing tools. It's about a gap between *"here's the code"* and *"here's the world that code expects to run in."* A README that says `npm install && npm run dev` assumes you already have the right Node, the right native toolchain, the right env vars. When you don't, the errors rarely tell you what's actually missing.

Ato treats execution as a **first-class artifact**, not an afterthought:

> *Software execution is not just a command — it is a **launch graph**.*

**Ato is built on three ideas:**

- **A recipe, not a script.** A source repository is only raw material; a *recipe* is the executable interpretation of that source. The same repo can have many recipes — a web app, a CLI, a desktop shell, a demo with a local database, a no-network mode.
- **A launch graph, not a command.** That graph includes your source, runtimes, tools, dependencies, environment, filesystem view, network policy, and entrypoint. Ato resolves it automatically.
- **Recipes, not opaque images.** Ato shares the inspectable recipe, then launches the project in a controlled local session — without requiring you to install Docker, learn Nix, or read the README first.

## Why this matters even in an AI world

AI coding assistants have made it easier than ever to debug setup errors. Paste the stack trace, get a fix, repeat. But that loop has a cost — **every iteration burns tokens and your attention.** The model doesn't know your machine; it guesses.

If execution is **deterministic** — the same project always resolves to the same launch conditions — you don't need to debug setup at all. There's nothing to paste. The project either runs or it tells you exactly why it can't.

Ato records an **execution identity** for each launch: a stable fingerprint of the full launch graph, stored at `~/.ato/executions/<execution_id>/receipt.json`.

```json
{
  "schema": "v2",
  "execution_id": "blake3:9f2c…e41",
  "runtime": { "driver": "node", "resolved": "20.11.1", "complete": true },
  "network": { "policy": "deny-all" },
  "filesystem": { "view": "project-scoped" },
  "env_closure": ["NODE_ENV", "PORT"],
  "entrypoint": { "run": "npm run dev", "cwd": "." }
}
```

This means:

- A **collaborator** can verify they ran *the same world* as you — no "works on my machine"
- **CI** can compare launch conditions across runs, not just source hashes
- An **AI agent** can delegate execution to Ato and skip the environment-guessing loop entirely, saving tokens for the actual problem

See [Execution Identity](execution-identity.md) for the field-by-field specification.

## How it works

Ato turns source inputs plus a recipe into a **launch graph** — the four stages on the diagram above (Declare → Resolve → Materialize → Record):

```text
source inputs
  │
  ▼
select or infer recipe          (1) Declare
  │
  ▼
project into the user's env      (2) Resolve
  │
  ▼
resolve tools, runtimes, deps,
services, and policy
  │
  ▼
materialize a managed session    (3) Materialize
  │
  ▼
record execution identity        (4) Record
```

A launch graph describes the **world a process is about to see**:

- **Source tree** — your project files
- **Runtime and tool binaries** — resolved and versioned
- **Dependency outputs** — built and cached
- **Environment variables** — explicit allowlist
- **Filesystem view** — what the process can and cannot see
- **Network and capability policy** — egress, ingress, bridge capabilities
- **Services and dependency providers** — sidecar processes and data
- **Entrypoint, arguments, and working directory**

This is different from only hashing source code, only writing a package lock, or only shipping a container image. Ato tracks the **launch conditions** under which source code becomes a running process.

## What Ato is not

Ato is **not** a full replacement for Docker, Nix, or package managers.

| Tool | What it does | What Ato does instead |
|---|---|---|
| **Docker** | Identifies and runs images | Identifies source-native launches |
| **Nix** | Makes build inputs and store outputs explicit | Makes launch conditions explicit |
| **Package managers** | Lock dependency choices | Also records runtime, environment, filesystem, policy, entrypoint, and state |
| **`npx` / `uvx`** | Run packages | Runs whole projects and service graphs |

Ato sits in the **launch layer**.

## Safety model

Ato is designed to make **host access explicit**.

> A process *with* host filesystem access and a process *without* it are not the same launch.

Ato treats filesystem grants, network policy, environment allowlists, and bridge capabilities as part of the launch graph.

**Current behavior:**

- Project files run through Ato's runtime path instead of directly on your host
- Common secret files (`.env`, private keys, credential files) are excluded from archives by default
- Source runtimes can use OS-level isolation through nacelle
- Network access can be denied or restricted on supported runtime paths

> **Note:** Ato is still pre-1.0. Do not treat it as a perfect security boundary for untrusted code. See [Sandbox](sandbox.md) for the current isolation model.

---

## Documentation

The public surface of this directory is **topic-first, with roles separated inside each page**. Each topic page contains: **Overview → How it works → Specification → Design Notes**.

### Topics

| | |
|---|---|
| [**Run →**](run.md) | The front door for executing source with a recipe |
| [**Recipes →**](recipe.md) | The unit Ato shares, stores, and runs |
| [**Capsule →**](capsule.md) | The unit Ato can identify and ship |
| [**Sandbox →**](sandbox.md) | Isolation, filesystem, and network model |
| [**Execution Identity →**](execution-identity.md) | Launch-envelope identity |
| [**Desktop →**](desktop.md) | Focused session shell for managed recipe executions |
| [**Publishing to the Ato Store →**](publishing-to-ato-store.md) | Ship your own app to the Store |

### Reference

- [Core Architecture](core-architecture.md)
- [Glossary](glossary-reference.md)
- [RFCs](rfcs/README.md)
- [Topic Page Template](topic-page-template.md)

### Internal docs

Plans, research notes, handoffs, and dashboards belong under [`internal/`](internal/README.md). They are workspace artifacts, not part of the main public navigation.

## Community & support

- **Questions and ideas** → [GitHub Discussions](https://github.com/ato-run/ato/discussions)
- **Bugs and feature requests** → [GitHub Issues](https://github.com/ato-run/ato/issues)
- **Source code** → [github.com/ato-run/ato](https://github.com/ato-run/ato)

**Code is the source of truth.** These topic pages track the current implementation in `crates/`, while RFCs remain the deeper contract and design history.

---

<p align="right"><a href="#/run"><strong>Next: Run →</strong></a></p>
