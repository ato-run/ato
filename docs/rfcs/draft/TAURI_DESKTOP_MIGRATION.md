# Tauri Desktop Migration

Status: Draft

## 1. Overview

Port the Tauri desktop shell from the pre-rewrite `crates/*` layout onto the
current `lib/` / `extensions/` / `apps/` / `tools/` layout. The old PR
`feat/tauri-migration-v2` (#1224) is a **donor**, not a branch to rebase or
cherry-pick. Only the parts that still fit the current Computation Architecture
are reimplemented; the installed-apps, store, update, rollback, repair, and
remove model is intentionally dropped.

```text
Bundled Desktop UI
        │ typed invoke
        ▼
apps/desktop-tauri
        │
        ├─ ato-ipc DTO
        └─ ato-host-control
                 │ child process
                 ▼
               ato CLI
                 │
                 ▼
Computation / Run / Adapter / Materializer
```

## 2. Scope

### 2.1 In scope

- Tauri shell that delegates every Capsule operation to the `ato` CLI.
- A host-agnostic process-supervision library for starting, watching, and
  terminating the CLI process tree.
- Typed process-boundary DTOs for inspecting the active Run.
- One local loopback Web Surface per active Run.
- macOS smoke verification of navigation, invoke, and bundle binary resolution.

### 2.2 Out of scope

- Store install, GitHub install, Installed Apps library.
- Update, rollback, repair, remove.
- Warm retained sessions and the old `runner serve` control surface.
- Marketplace, `atoview://` remote proxy.
- A production `ato-pwa` Desktop UI (a minimal local frontend ships first).
- Windows/Linux installer release integration.
- Removal of the existing GPUI + Wry `apps/desktop`.

## 3. Responsibilities

```text
ato CLI
  Computation generation, resume, stop, materialization, and execution.

ato-host-control
  Start, watch, cancel, and terminate the ato CLI process tree.

apps/desktop-tauri
  Native window, file dialog, typed invoke, and display.

ato-ipc
  The DTOs used across the process boundaries above.
```

Tauri never manages `Computation` or `Run` directly, and never reads or writes
`.capsule/` directly. It invokes the CLI and renders typed results.

## 4. MVP operations

```text
Init
Resume
Stop
Encap
Run portable .capsule
Inspect active Run
Open one Web Surface
```

## 5. Non-goals (not provided in the MVP)

```text
Store install
GitHub install
Installed Apps
Update
Rollback
Repair
Remove
Warm retained sessions
Marketplace
```

## 6. Security model

| Window  | Content                       | Native invoke | Top-level navigation          |
| ------- | ----------------------------- | ------------- | ----------------------------- |
| `main`  | bundled assets                | yes           | local asset origin only       |
| `home`  | `https://app.ato.run`         | no            | exact trusted HTTPS origin    |
| `app-*` | CLI-returned loopback origin  | no            | exact loopback origin only    |

The `main` capability is defense in depth, not the sole authorization check.
Every `#[tauri::command]` — including `desktop_info` — verifies the caller
label is `main`. Remote and guest windows cannot invoke native commands
regardless of capability file.

`app-*` labels are collision-resistant: `app-` plus a truncated BLAKE3 digest
over the canonical project path and the surface URL. Reusing an existing
`app-*` window additionally verifies that the window's current URL origin
still equals the expected surface origin.

## 7. CLI boundary

The public CLI keeps exactly five operations (`init`, `resume`, `stop`, `encap`,
`run`). The desktop shell adds no public commands. It uses a hidden machine
command for active-Run inspection:

```text
ato __desktop inspect <project>
```

The command prints a single JSON object to stdout; diagnostics go to stderr.
It returns a Web surface only for an explicit `127.0.0.1:<port>` listen with a
non-zero port. Dynamic ports (`port 0`), `0.0.0.0`, other loopback ranges
(`::1`, `127.0.0.2`), hostnames, remote addresses, and non-HTTP surfaces are
all rejected, keeping the CLI and the shell's URL validation (`127.0.0.1` /
`localhost` hosts only) on one policy.

## 7.1 Process ownership

The shell is a supervisor of the CLI process tree, not a detached launcher.
Short-lived operations (`init`, `resume`, `stop`, `encap`, inspect) run to
completion. The portable `run` is long-lived (it blocks inside realization):
it is spawned through `ato-host-control`'s `ProcessSupervisor` and stays
supervisor-owned for its whole lifetime. App exit and an explicit cancel both
terminate the owned process group (`kill -pid` on Unix, `taskkill /T /F` on
Windows), so no CLI child or grandchild outlives the desktop.

## 7.2 Bundle staging

The release `.app` carries the `ato` CLI as a sidecar next to the shell
executable (`bundle.externalBin`). `build.rs` stages the root workspace's
release `ato` binary (or an `ATO_DESKTOP_ATO_STAGE` override) into
`bin/ato-<target-triple>`; tauri-build then places it next to the shell binary
inside the bundle. A release build without a staged CLI fails hard — a release
bundle without a real `ato` sidecar must never be produced. Debug builds fall
back to a placeholder that delegates to `ato` on PATH so plain
`cargo build` / `cargo test` keep working.

## 8. Port / Rewrite / Drop table

The old PR's `crates/desktop-tauri`, `crates/runner`, and `crates/protocol`
files classify as follows. `crates/cli`, `crates/capsule`, `crates/nacelle`,
`crates/snapshot*`, `crates/desktop`, and the docs changes from #1224 are out
of scope for this port.

| Old file (in #1224)                              | Action  | New location                                             |
| ------------------------------------------------ | ------- | -------------------------------------------------------- |
| `crates/desktop-tauri/Cargo.toml`                | Rewrite | `apps/desktop-tauri/Cargo.toml`                          |
| `crates/desktop-tauri/Cargo.lock`                | Drop    | regenerated                                              |
| `crates/desktop-tauri/.gitignore`                | Rewrite | `apps/desktop-tauri/.gitignore`                          |
| `crates/desktop-tauri/build.rs`                  | Rewrite | `apps/desktop-tauri/build.rs`                            |
| `crates/desktop-tauri/tauri.conf.json`           | Rewrite | `apps/desktop-tauri/tauri.conf.json`                     |
| `crates/desktop-tauri/capabilities/default.json` | Rewrite | `apps/desktop-tauri/capabilities/default.json`           |
| `crates/desktop-tauri/icons/icon.png`            | Drop    | regenerate                                               |
| `crates/desktop-tauri/src/main.rs`               | Rewrite | `apps/desktop-tauri/src/main.rs`                         |
| `crates/desktop-tauri/src/lib.rs`                | Rewrite | `apps/desktop-tauri/src/lib.rs`                          |
| `crates/desktop-tauri/src/host.rs`               | Rewrite | `apps/desktop-tauri/src/host.rs` (trust model kept)      |
| `crates/desktop-tauri/src/proxy.rs`              | Drop    | `atoview://` remote proxy is out of scope                |
| `crates/runner/Cargo.toml`                       | Rewrite | `lib/host-control/Cargo.toml`                            |
| `crates/runner/src/backend.rs`                   | Port    | `lib/host-control/src/backend.rs`                        |
| `crates/runner/src/os.rs`                        | Port    | `lib/host-control/src/native.rs`                         |
| `crates/runner/src/supervisor.rs`                | Port    | `lib/host-control/src/supervisor.rs`                     |
| `crates/runner/src/lib.rs`                       | Rewrite | `lib/host-control/src/lib.rs`                            |
| `crates/runner/src/client.rs`                    | Drop    | old CLI dependency; a current-CLI client is written fresh |
| `crates/runner/src/control.rs`                   | Drop    | old `runner serve`; needed machine API moves to CLI hidden command |
| `crates/runner/src/events.rs`                    | Rewrite | event names redefined inside Tauri                        |
| `crates/runner/src/session.rs`                   | Drop    | replaced by `ActiveRun` inspection                        |
| `crates/runner/src/surface_timing.rs`            | Drop    | old surface-timing model                                  |
| `crates/protocol/Cargo.toml`                     | Drop    | `ato-ipc` already exists                                 |
| `crates/protocol/src/lib.rs`                     | Drop    | `ato-ipc` already exists                                 |
| `crates/protocol/src/intent.rs`                  | Rewrite | `lib/ipc/src/desktop_control.rs`                         |
| `crates/protocol/src/desktop_library.rs`         | Drop    | Installed Apps model is removed                          |
| `crates/protocol/src/nacelle_ipc.rs`             | Drop    | out of scope                                             |
| `crates/protocol/src/oci_session.rs`             | Drop    | out of scope                                             |
| `crates/protocol/src/runtime_control.rs`         | Drop    | out of scope                                             |
| `crates/protocol/src/runtime_control_events.rs`  | Drop    | out of scope                                             |
| `crates/protocol/src/secret_bridge.rs`           | Drop    | out of scope                                             |
| `crates/protocol/src/binding_control.rs`         | Drop    | exists in `ato-ipc`                                      |
| `crates/protocol/src/binding_lease.rs`           | Drop    | exists in `ato-ipc`                                      |
| `crates/protocol/src/session_surface.rs`         | Drop    | exists in `ato-ipc`                                      |
| `crates/protocol/src/config.rs`                  | Drop    | out of scope                                             |
| `crates/protocol/src/consent.rs`                 | Drop    | out of scope                                             |
| `crates/protocol/src/error.rs`                   | Drop    | exists in `ato-ipc`                                      |
| `crates/protocol/src/handle.rs`                  | Drop    | out of scope                                             |
| `crates/protocol/src/placement.rs`               | Drop    | out of scope                                             |
| `crates/protocol/src/net.rs`                     | Drop    | exists in `ato-ipc`                                      |
| `crates/protocol/src/net/control.rs`             | Drop    | exists in `ato-ipc`                                      |
| `crates/protocol/src/net/ingress_http.rs`        | Drop    | out of scope                                             |
| `crates/protocol/src/net/receipt.rs`             | Drop    | out of scope                                             |
| `crates/protocol/src/net/resolver.rs`            | Drop    | out of scope                                             |
| `crates/protocol/src/net/stable_origin.rs`       | Drop    | out of scope                                             |
| `crates/protocol/src/ccp/mod.rs`                 | Drop    | out of scope                                             |
| `crates/protocol/src/ccp/schema.rs`              | Drop    | out of scope                                             |
| `crates/protocol/src/ccp/tolerance.rs`           | Drop    | out of scope                                             |
| `crates/protocol/src/ccp/version.rs`             | Drop    | out of scope                                             |
| `crates/protocol/tests/session_surface_contract.rs` | Drop | exists in `ato-ipc` tests                                |

The `runner` package name is not revived: it collides with the current `Run`
and remote-runner vocabulary. The package is `ato-host-control`. Neither
`protocol` nor `runner` package names are reintroduced; `arch-check` forbids
the legacy names.

## 9. Constraints and known limitations

- Surface discovery is bounded by the CLI-owned inspect boundary; `ActiveRun`
  has no presentation endpoint, so the CLI derives explicit loopback listen
  addresses from Runtime state.
- Release builds must resolve the bundled sibling `ato` binary; a missing
  binary is an explicit failure, not a silent `$PATH` fallback.
- Window creation failure never stops the underlying Run.

## 参照

- `docs/rfcs/accepted/COMPUTATION_ARCHITECTURE.md` — Computation identity and Run cursor.
- `docs/rfcs/accepted/CAPSULE_CLI_LIFECYCLE.md` — the five public CLI operations.
- `docs/rfcs/accepted/LOCAL_CAPSULE_REPOSITORY.md` — `ActiveRun` lease and `.capsule/` layout.
- `lib/ipc/src/computation.rs` — existing `ComputationCommand`.
- `lib/ipc/src/session_surface.rs` — existing Web/Terminal surface contract.
