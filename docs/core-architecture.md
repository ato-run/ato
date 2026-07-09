# Core Architecture

## Overview

The core of the Ato implementation is split across four layers:

- `crates/cli`: the human-facing CLI surface (also hosts the Connected Runner agent)
- `crates/capsule`: manifest, lock, routing, engine bridge, and execution identity
- `crates/nacelle`: the machine-oriented execution engine
- `crates/desktop`: the GPUI/Wry desktop shell that consumes session metadata

Around that core, the workspace carries the **Ready-State snapshot subsystem**
and shared plumbing:

- `crates/snapshot`: backend-agnostic build/restore of Ready-State warm state
  (real Firecracker/KVM backend; QEMU and Kata stubs; seal + no-secret scan)
- `crates/capsulefs`: content-addressed, chunked local store for Ready-State
  capsule layers (rootfs / runtime / deps / app / vmstate / memory)
- `crates/snapshot-builder`: the builder daemon that claims capsule-snapshot
  jobs from the control plane, builds and seals artifacts, and acks (dev/ops
  tool, not shipped to end users)
- `crates/guest-agent`: the in-guest agent — vsock binding-lease control plane
  and v1.2 supervisor mode
- `crates/protocol`: capsule wire types shared by `cli` (producer) and
  `desktop` (consumer)
- `crates/netd`: the session-scoped network broker daemon
- `crates/cli/lock-draft-engine`: the LEIP lock-draft hashing engine
- `sidecars/ato-tsnetd`: Go tailnet sidecar (kept outside the cargo workspace,
  like `crates/desktop`)

If you want to understand how Ato behaves today, start from `cli`, then
follow the handoff into `capsule`, and only then drop into `nacelle`. Visit the
snapshot crates only when working on the Ready-State path.

## How it works

### Entry points

- `crates/cli/src/main.rs` -> `ato_cli::main_entry()`
- `crates/nacelle/src/main.rs` -> `cli::execute().await`
- `crates/desktop/src/main.rs` -> `app::run()`

### Main execution path

The current `ato run` path is:

1. CLI parse and command selection in `cli/root.rs`
2. top-level dispatch in `src/lib.rs` and `cli/dispatch/mod.rs`
3. run-like input normalization in `cli/dispatch/run.rs`
4. environment assistance and run-command bridging in
   `application/engine/install/support.rs`
5. hourglass execution in `cli/commands/run.rs`
6. install / prepare / build / verify / dry-run / execute phase logic in
   `application/pipeline/phases/run.rs`
7. manifest or lock routing in `capsule/src/routing/router.rs`
8. engine resolution in `capsule/src/engine/engine_impl.rs`
9. machine-oriented execution in `nacelle internal exec`

### Responsibility split

| Layer | Responsibility |
|---|---|
| `cli` | user CLI, input normalization, reporter UX, orchestration, Connected Runner agent (`ato runner`) |
| `capsule` | manifest model, lock model, runtime routing, host isolation context, execution receipts |
| `nacelle` | internal engine protocol, sandbox enforcement, process execution |
| `desktop` | local desktop shell, webview orchestration, session / receipt display |
| `snapshot` + `capsulefs` + `snapshot-builder` + `guest-agent` | Ready-State snapshot build, storage, restore, and in-guest control plane |
| `protocol` / `netd` | cli ⇄ desktop wire types / session network broker |

### Current run model

The current implementation is no longer a thin “load manifest then launch”
stack. The run path is an hourglass:

1. **Install**: resolve the target, materialize dependencies, and prepare an
   isolated run workspace
2. **Prepare**: select authoritative manifest / lock input, build the prepared
   run context, and reject invalid capsule shapes such as `type = "library"`
   for `ato run`
3. **Build**
4. **Verify**
5. **DryRun**
6. **Execute**

Execution identity and receipt building sit beside this flow rather than below
it: they describe the launch envelope that is about to run.

## Specification

- the public execution handle is `cli`; `nacelle` is internal plumbing
- manifest and lock resolution MUST happen before engine execution
- the canonical `discover_nacelle()` path disables PATH fallback; other
  nacelle-resolution paths (`resolve_nacelle_binary`, `find_nacelle_binary`)
  may fall back to PATH as a last resort
- `capsule` is the contract layer for manifest shape, routing, and
  execution identity
- `desktop` is a consumer of session / receipt metadata, not the source of
  execution truth

References:

- [`crates/cli/src/main.rs`](https://github.com/ato-run/ato/blob/main/crates/cli/src/main.rs)
- [`crates/cli/src/cli/root.rs`](https://github.com/ato-run/ato/blob/main/crates/cli/src/cli/root.rs)
- [`crates/cli/src/cli/commands/run.rs`](https://github.com/ato-run/ato/blob/main/crates/cli/src/cli/commands/run.rs)
- [`crates/cli/src/application/pipeline/phases/run.rs`](https://github.com/ato-run/ato/blob/main/crates/cli/src/application/pipeline/phases/run.rs)
- [`crates/capsule/src/routing/router.rs`](https://github.com/ato-run/ato/blob/main/crates/capsule/src/routing/router.rs)
- [`crates/capsule/src/engine/engine_impl.rs`](https://github.com/ato-run/ato/blob/main/crates/capsule/src/engine/engine_impl.rs)
- [`crates/nacelle/src/cli/mod.rs`](https://github.com/ato-run/ato/blob/main/crates/nacelle/src/cli/mod.rs)

## Design Notes

This split preserves Ato's main design constraint: one user-facing handle, one
execution model, one lower-level engine boundary. The implementation can stay
complex internally, but the mental model should remain simple from the outside.
