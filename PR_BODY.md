# Rebuild Ato around addressable computations

## Goal

This migration rebuilds Ato around one principle: **Ato advances addressable computations**. `ComputationObject` is the canonical semantic value, `ComputationRef` is its immutable address and Capsule identity, and a `Run` is only a mutable cursor to the current ref. History remains optional evidence and does not participate in computation identity.

## Semantic model

- `ato-computation` owns semantic identifiers, boundaries, canonical encoding, BLAKE3 identity, and verification.
- `ato-kernel` resolves a ref, dispatches its registered semantics, persists a successor, and advances `Run.head`.
- `ato-objects` owns verified CAS persistence, closure traversal, signatures, garbage collection, and `.capsule` bundles.
- Concrete semantics own evolution; adapters compile authoring inputs away; providers own physical realization.
- Ports are typed, boundary-relative interactions. Internal composition synchronizes as `Tau`.
- Goals and contracts remain application concerns; snapshots are provider materializations.

## What was removed

The migration removes the old State/I/O model, LockDraft engine and WASM compatibility layer, the `capsule` monolith, application execution plans/contracts, old protocol/artifact formats, generic snapshot filesystem package, and all compatibility facade packages. The forbidden legacy package names no longer occur in Cargo metadata or the lockfile.

## Capability preservation

| Capability | New owner |
| --- | --- |
| Local and Git repository ingestion, detection, ambiguity evidence | `ato-adapter-repository` |
| Workspace logical behavior and version-bearing runtime constraints | `ato-semantics-workspace` |
| Composition and boundary-relative synchronization | `ato-semantics-compose` |
| Process launch, dependency installation, sandbox, filesystem, network, env and secret binding | `ato-provider-nacelle` |
| Snapshot capture/restore and host compatibility | `ato-provider-snapshot` |
| CAS, verified loading, closure, GC, signing and `.capsule` transport | `ato-objects` |
| Process wire DTOs for CLI, Desktop, guest and netd | `ato-ipc` |
| `run`, `lock`, `encap`, `decap`, `ps`, `logs`, `stop` product workflows | `apps/cli` |
| Desktop launcher and process-boundary integration | `apps/desktop` |

Secrets are represented in computation identity only by safe binding identifiers; plaintext values stay in provider-owned resolution. Resolution evidence produced by `ato lock` is a derived repository cache, not a semantic primitive.

## Canonical scenario

The canonical integration test branches Alice and Bob from the same computation. The name-provider and greeter test semantics derive transitions from their own residuals, the Kernel persists every successor, Compose turns their connected name exchange into `Tau`, and the exported `{name, greeting}` boundary stays invariant. All seven computation refs are distinct. Existing invalid-transition coverage remains, including stale sources, polarity/value mismatches, invalid endpoints/exports, same-node links, and successor-boundary drift.

## Repository layout

```text
lib/{computation,kernel,objects,ipc}
extensions/semantics/{compose,workspace}
extensions/adapters/repository
extensions/providers/{nacelle,snapshot}
apps/{cli,desktop}
services/{netd,ato-tsnetd}
tools/{snapshot-builder,arch-check}
```

No primary workspace package remains under `crates/`.

## Dependency architecture

```text
ato-computation
  ├─ ato-objects ─ ato-kernel
  ├─ concrete semantics
  ├─ adapters
  └─ providers

ato-ipc ─ providers/services/apps
apps assemble semantics + adapters + providers
```

The Kernel has no dependency on Compose, Workspace, Repository, Nacelle, Snapshot, CLI, or Desktop. `tools/arch-check` reads the actual `cargo metadata` graph and enforces layer rules and forbidden-package absence.

## Breaking changes

Compatibility with old State/I/O schemas, LockDraft JSON/WASM APIs, manifest schemas, execution-plan types, wire types, package names, and legacy artifact bytes is intentionally not retained. `capsule.toml` is repository-adapter syntax and compiles away; `.capsule` now transports a root `ComputationRef` plus its verified object closure.

## Validation

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --no-fail-fast`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo metadata --format-version 1 --locked`
- `cargo run -p arch-check --locked`
- `cargo check --manifest-path apps/desktop/Cargo.toml --all-targets`
- `cargo test --manifest-path apps/desktop/Cargo.toml`
- `cargo test --manifest-path apps/desktop/xtask/Cargo.toml`
- `go test ./...` in `services/ato-tsnetd`
- `dist plan --output-format=json`
- local repository run, lock, `.capsule` export/import, sandboxed rematerialization, and Git fixture execution

## Migration notes

Downstream code must consume `ato-computation`, `ato-objects`, `ato-kernel`, or the appropriate extension rather than importing former capsule packages. Producers of old artifact bundles must re-export them through the new object-bundle path. Repository authoring should target the concrete `ato.workspace@1` adapter input instead of relying on legacy manifest compatibility.
