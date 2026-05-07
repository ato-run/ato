# What's Ato?

<p align="center"><strong>Run a project before you set it up.</strong></p>

![Concept](concept-image.png)

`ato` is a command-line tool for running local projects, GitHub repositories, and shared app links *before* you set them up by hand.

It detects what the project needs, prepares missing tools and runtimes, and starts it in a controlled environment — without asking you to install Python, Node, Rust, or any project-specific dependency first.

---

## The problem

A friend shares a project with you. You clone it. Then the ritual begins.

```
node: command not found
```

You install Node. Wrong version. You install `nvm`. You run `nvm use`. Now it wants Python. And then a native build tool you've never heard of. Forty minutes later, the app still won't start — and you haven't even looked at the code yet.

You know the alternatives. **Docker** would work, but you don't want a daemon running on your laptop just to try someone's side project. **Nix** would be reproducible, but learning Nix to run one repo feels like going to culinary school to make toast. So you keep fighting the environment — one missing dependency at a time.

**This is the problem Ato was built to solve.**

---

## The insight

The frustration isn't really about missing tools. It's about a gap between *"here's the code"* and *"here's the world that code expects to run in."*

Most sharing workflows hand you the source and leave you to reconstruct the rest. A README that says `npm install && npm run dev` assumes you already have the right Node, the right native toolchain, the right env vars. When you don't, you get errors — and the errors rarely tell you what's actually missing.

Ato treats execution as a **first-class artifact**, not an afterthought:

> *Software execution is not just a command — it is a **launch graph**.*

That graph includes your source, runtimes, tools, dependencies, environment, filesystem view, network policy, and entrypoint. Ato resolves that graph automatically and launches the project in a controlled local session — **without requiring you to install Docker, learn Nix, or read the README first.**

---

## Why this matters even in an AI world

AI coding assistants have made it easier than ever to debug setup errors. Paste the stack trace, get a fix, repeat. But that loop has a cost — **every iteration burns tokens and your attention.** The model doesn't know your machine; it guesses.

If execution is **deterministic** — meaning the same project always resolves to the same launch conditions — you don't need to debug setup at all. There's nothing to paste. The project either runs or it tells you exactly why it can't.

Ato records an **execution identity** for each launch: a stable fingerprint of the full launch graph. This means:

- A collaborator can verify they ran *the same world* as you — no "works on my machine"
- CI can compare launch conditions across runs, not just source hashes
- An AI agent can delegate execution to Ato and skip the environment-guessing loop entirely, saving tokens for the actual problem

---

## Quick demo

```bash
# run the project in the current directory
ato run .

# run a GitHub repository — no clone required
ato run github.com/ato-run/hello-astro

# run a shared app link
ato run capsule://hello-astro@1.0.0
```

The same handle works whether the target is a local checkout, a remote repository, a Store reference, or a canonical capsule. See [Run](run.md) for the full surface.

---

## How it works

Ato turns a project into a **launch graph**.

```text
project or capsule handle
  │
  ▼
construct launch graph
  │
  ▼
resolve tools, runtimes, dependencies, and policy
  │
  ▼
materialize an isolated session
  │
  ▼
record execution identity and receipt
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

## Execution Identity

*Execution Identity* is the launch-envelope identity Ato uses to answer:

> **Did we launch the same world?**

It is computed **before launch** and covers the full launch condition — not only the source tree. The current default receipt path emits schema v2 and stores each launch under `~/.ato/executions/<execution_id>/receipt.json`.

See [Execution Identity](execution-identity.md) for the field-by-field specification.

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

## Documentation

The public surface of this directory is **topic-first, with roles separated inside each page**. Each topic page contains:

1. **Overview**
2. **How it works**
3. **Specification**
4. **Design Notes**

### Topics

- [Run](run.md) — the front door for executing a project
- [Capsule](capsule.md) — the unit Ato can identify and ship
- [Sandbox](sandbox.md) — isolation, filesystem, and network model
- [Execution Identity](execution-identity.md) — launch-envelope identity
- [Desktop](desktop.md) — graphical session shell for managed projects

### Reference

- [Core Architecture](core-architecture.md)
- [Glossary](glossary-reference.md)
- [RFCs](rfcs/README.md)
- [Topic Page Template](topic-page-template.md)

### Internal docs

Plans, research notes, handoffs, and dashboards belong under [`internal/`](internal/README.md). They are workspace artifacts, not part of the main public navigation.

## Source of truth

**Code is the source of truth.** These topic pages should track the current implementation in `crates/`, while RFCs remain the deeper contract and design history.
