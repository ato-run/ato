# Core Architecture

## Overview

The current Ato implementation is split across four public layers:

- `crates/ato-cli`: the human-facing CLI surface
- `crates/capsule-core`: manifest, lock, routing, engine bridge, and execution identity
- `crates/nacelle`: the machine-oriented execution engine
- `crates/ato-desktop`: the GPUI/Wry desktop shell that consumes session metadata

If you want to understand how Ato behaves today, start from `ato-cli`, then
follow the handoff into `capsule-core`, and only then drop into `nacelle`.

## How it works

### Entry points

- `crates/ato-cli/src/main.rs` -> `ato_cli::main_entry()`
- `crates/nacelle/src/main.rs` -> `cli::execute().await`
- `crates/ato-desktop/src/main.rs` -> `app::run()`

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
7. manifest or lock routing in `capsule-core/src/routing/router.rs`
8. engine resolution in `capsule-core/src/engine/engine_impl.rs`
9. machine-oriented execution in `nacelle internal exec`

### Responsibility split

| Layer | Responsibility |
|---|---|
| `ato-cli` | user CLI, input normalization, reporter UX, orchestration |
| `capsule-core` | manifest model, lock model, runtime routing, host isolation context, execution receipts |
| `nacelle` | internal engine protocol, sandbox enforcement, process execution |
| `ato-desktop` | local desktop shell, webview orchestration, session / receipt display |

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

- the public execution handle is `ato-cli`; `nacelle` is internal plumbing
- manifest and lock resolution MUST happen before engine execution
- engine discovery MUST prefer explicit / configured paths and MUST NOT fall back
  to PATH search
- `capsule-core` is the contract layer for manifest shape, routing, and
  execution identity
- `ato-desktop` is a consumer of session / receipt metadata, not the source of
  execution truth

References:

- [`crates/ato-cli/src/main.rs`](https://github.com/ato-run/ato/blob/main/crates/ato-cli/src/main.rs)
- [`crates/ato-cli/src/cli/root.rs`](https://github.com/ato-run/ato/blob/main/crates/ato-cli/src/cli/root.rs)
- [`crates/ato-cli/src/cli/commands/run.rs`](https://github.com/ato-run/ato/blob/main/crates/ato-cli/src/cli/commands/run.rs)
- [`crates/ato-cli/src/application/pipeline/phases/run.rs`](https://github.com/ato-run/ato/blob/main/crates/ato-cli/src/application/pipeline/phases/run.rs)
- [`crates/capsule-core/src/routing/router.rs`](https://github.com/ato-run/ato/blob/main/crates/capsule-core/src/routing/router.rs)
- [`crates/capsule-core/src/engine/engine_impl.rs`](https://github.com/ato-run/ato/blob/main/crates/capsule-core/src/engine/engine_impl.rs)
- [`crates/nacelle/src/cli/mod.rs`](https://github.com/ato-run/ato/blob/main/crates/nacelle/src/cli/mod.rs)

## Design Notes

This split preserves Ato's main design constraint: one user-facing handle, one
execution model, one lower-level engine boundary. The implementation can stay
complex internally, but the mental model should remain simple from the outside.
