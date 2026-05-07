# Ato Desktop

## Overview

Ato Desktop is the graphical session shell for Ato.

It is not a separate execution engine and it is not a replacement for the CLI. Desktop uses the same launch model as `ato run` and `ato session start`: Ato constructs a launch graph, materializes it as a managed session, records session state, and presents the running project through a desktop-native surface.

**The CLI remains the execution worker. Desktop provides the user plane.**

```text
Ato Desktop
  │
  ▼
ato CLI
  │
  ▼
launch graph
  │
  ▼
managed session
  │
  ▼
WebView / logs / bridge / lifecycle
```

## Why Desktop?

Many source-native projects are not just terminal commands. They become local web apps, tools, dashboards, editors, agents, notebooks, or small services.

The CLI is the best interface for trying and automating those projects. Desktop is the best interface for **keeping them open, inspecting them, stopping them, and interacting with them as local applications**.

Desktop makes Ato sessions feel like apps without changing the underlying execution model.

> *Desktop is not a second runtime. It is a graphical shell over the same execution graph.*

## How it works

Desktop delegates execution to the CLI.

The Desktop process is the user-facing root. It starts the `ato` binary as a child process and passes launch context to it. The CLI resolves the project, constructs the launch graph, materializes the session, and returns structured session information.

```text
Desktop click / capsule open
  │
  ▼
spawn ato session start
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

Desktop can show, reuse, and stop sessions because the CLI records them as session records. This is also why Desktop and CLI session behavior should stay unified — `ato run -b`, `ato session start`, and Desktop launches should all use the same session core.

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
| `ato session start` | lifecycle-native automation | managed session API |
| **Ato Desktop** | graphical interaction and app-like UX | managed session with desktop presentation |

The interface changes. The execution model does not.

## Design Notes

### Implementation

Ato Desktop is implemented as a Rust desktop application using GPUI and Wry. It does not link against the CLI as a library — instead, Desktop spawns the CLI as the execution worker. This preserves a clear process boundary:

```text
Desktop → CLI → nacelle / runtime process
```

Desktop is responsible for UI, orchestration, WebView management, and user-facing lifecycle state. CLI is responsible for manifest handling, lock handling, execution planning, sandbox setup, process launch, session records, and typed errors.

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
