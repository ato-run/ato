# Ato Desktop

## Overview

Ato Desktop is the focused graphical shell for managed recipe executions.

It is not a separate execution engine and it is not a replacement for the CLI.
Desktop uses the same launch model as `ato run` and `ato run -b`: Ato
constructs a launch graph from a recipe and its source inputs, materializes it
as a managed session, records session state, and presents the running execution
through a desktop-native surface.

**The CLI remains the execution worker. Desktop provides the user plane.**

```text
Ato Desktop
  │
  ▼
ato CLI
  │
  ▼
recipe + source inputs
  │
  ▼
launch graph
  │
  ▼
managed session
  │
  ▼
app view / logs / execution identity / lifecycle controls
```

## What changed in 0.6.0

Ato Desktop is no longer a dock-first or window-list-first interface.

The main Desktop surface is the running recipe execution itself: app view,
session status, logs, lifecycle controls, capsule/recipe details, and execution
identity are shown in one focused surface.

**Removed / replaced:**

- dock-first capsule management screen
- separate open-window management as the main Desktop model
- window lifecycle as the owner of process lifecycle

**Added / emphasized:**

- focused app surface centered on the current recipe execution
- execution identity display
- session status and readiness
- logs and diagnostics
- restart / stop controls
- capsule and recipe details
- attach/reuse of existing healthy sessions
- window close independent from session stop

A window presents a managed session. It does not necessarily own the process
lifecycle. Closing a window may detach from a session; stopping a session
explicitly terminates the managed process.

> **This is a breaking change for Desktop UX.** The previous dock-first model is
> replaced by a focused session shell.

## Why Desktop?

Many source-native projects are not just terminal commands. They become local web apps, tools, dashboards, editors, agents, notebooks, or small services.

The CLI is the best interface for trying and automating those projects. Desktop is the best interface for **keeping a recipe execution open, inspecting it, stopping it, and interacting with it as a local application**.

Desktop makes Ato sessions feel like apps without changing the underlying execution model.

> *Desktop is not a second runtime. It is a focused graphical shell over the same execution graph.*

## How it works

Desktop delegates execution to the CLI.

The Desktop process is the user-facing root. It starts the `ato` binary as a child process and passes launch context to it. The CLI resolves the project, constructs the launch graph, materializes the session, and returns structured session information.

```text
Desktop click / capsule open
  │
  ▼
spawn ato (internal session launch)
  │
  ▼
construct declared execution graph
  │
  ▼
resolve tools, runtimes, dependencies, services, and policy
  │
  ▼
materialize session
  │
  ▼
return URL, pid, readiness, logs, and session metadata
```

This keeps the architecture simple: Desktop owns presentation, while CLI owns execution.

### Session lifecycle

A Desktop app is backed by a managed Ato session. A session is not just a process — it is a materialized launch graph with lifecycle state:

- **Session id and execution identity**
- **Process id and start time**
- **Readiness status and local URLs**
- **Dependency providers and state directories**
- **Logs, teardown order, and owner / watcher relationship**

Desktop can show, reuse, and stop sessions because the CLI records them as session records (inspect them with `ato ps`, `ato logs`, `ato stop`). This is also why Desktop and CLI session behavior should stay unified — `ato run -b` and Desktop launches should all use the same session core.

### WebView and bridge

For web targets, Desktop presents the running session through a WebView. The WebView is not the execution boundary — the session is. The WebView is the presentation surface attached to a local URL produced by the session.

Some projects need controlled access back to the host: reading a file, opening a dialog, communicating with a local model service. That access goes through an explicit bridge.

```text
guest app
  │
  ▼
Desktop bridge
  │
  ▼
host capability
```

Bridge access is capability-gated. A process allowed to call a host bridge and a process denied that bridge are not the same launch.

## Specification

- Desktop delegates all execution to `ato` CLI; it does not construct launch graphs directly
- A Desktop launch MUST produce the same execution identity as an equivalent CLI launch for the same project
- Sessions launched from Desktop use the same session record format as CLI-originated sessions
- Desktop MUST respect the launch graph's capability policy; bridge permissions are part of the graph
- If the launch graph is unchanged and the previous session is still healthy, Desktop MAY reuse the existing session
- If the launch graph changed, Desktop MUST materialize a new session

### Desktop vs CLI

| Interface | Best for | Execution model |
|---|---|---|
| `ato run` | trying a project now | foreground session |
| `ato run -b` | keeping a project running | background managed session |
| `ato ps` / `ato logs` / `ato stop` | inspecting and managing sessions | managed session records |
| **Ato Desktop** | graphical interaction and app-like UX | managed session with desktop presentation |

The interface changes. The execution model does not.

## Design Notes

### Implementation

Ato Desktop is implemented as a Rust desktop application using GPUI and Wry. It does not link against the CLI as a library — instead, Desktop spawns the CLI as the execution worker. This preserves a clear process boundary:

```text
Desktop → CLI → nacelle / runtime process
```

Desktop is responsible for UI, orchestration, WebView management, and user-facing lifecycle state. CLI is responsible for manifest handling, lock handling, execution planning, sandbox setup, process launch, session records, and typed errors.

### Bundled helpers vs. shell CLI vs. private sidecar

Desktop ships `ato` and `nacelle` *inside* the application bundle. Three roles
must not be conflated:

| Term | What it is | On the user's `PATH`? |
|---|---|---|
| **Bundled helper** (`ato`) | A private copy of the CLI shipped inside Desktop for Desktop-internal orchestration. Desktop spawns it directly by absolute path. | No. |
| **User shell CLI** (`ato`) | The `ato` command a user runs in a terminal. Available **only** after an explicit CLI expose/install step — installing or running Desktop does not add `ato` to your shell. | Only after an explicit expose step. |
| **Private sidecar** (`nacelle`) | The internal runtime engine. The bundled `ato` resolves it adjacent to itself (portable mode); it is never exposed on `PATH`. | No. |

Runtime resolution preserves a strict precedence so the bundled copies win:

```text
packaged Desktop  → resolves bundled `ato` first  (Contents/Helpers/ato, …)
bundled `ato`     → resolves adjacent `nacelle` first (portable, next to ato)
PATH lookup       → secondary fallback only
```

**Distribution (build-time, not runtime).** Release bundles do not rebuild the
helpers. The Desktop release pipeline consumes the already-published `ato` /
`nacelle` artifacts from the CLI release (cargo-dist) and stages them into the
bundle, then signs/notarizes/packages. There is **no runtime download** of
helpers — they are present offline in the bundle. Local development still builds
the helpers from source (`cargo xtask bundle <target>`, default
`--helper-source=local`); release uses `--helper-source=release`. `ato-netd` is
always built locally because it is not a released artifact.

Per-platform bundled helper layout:

| Platform | `ato` | `nacelle` |
|---|---|---|
| macOS | `Ato Desktop.app/Contents/Helpers/ato` | `…/Contents/Helpers/nacelle` |
| Windows | `Ato\bin\ato.exe` | `Ato\bin\nacelle.exe` |
| Linux (AppImage) | `AppDir/usr/bin/ato` | `AppDir/usr/bin/nacelle` |

### Long-term goal

> Every launch becomes one managed execution graph, no matter whether it starts from CLI foreground, CLI background, automation, or Desktop.

### Current limitations

Ato Desktop is still pre-1.0. Current limitations may include:

- Platform-specific WebView behavior
- Incomplete capability prompt UX
- Evolving bridge schema
- Beta-quality non-macOS builds
- Session lifecycle edge cases while the session model is being unified

Desktop should not be treated as a stronger security boundary than the CLI. The security boundary is defined by the launch graph, sandbox policy, bridge permissions, and runtime enforcement.
