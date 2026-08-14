# Ato

Ato advances addressable computations.

A repository, a composed service graph, and a resumed workload all become a
canonical `ComputationObject`. Sealing that value produces a `ComputationRef`;
that reference is the Capsule handle. The kernel resolves the current
reference, dispatches its concrete semantics, persists the successor, and
updates a mutable `Run { head }` cursor.

```text
repository syntax ──adapter──> ComputationObject ──seal──> ComputationRef
                                                │
                                                ▼
                              Kernel + registered Semantics
                                                │
                              Provider realization + Objects CAS
                                                ▼
                                      successor ComputationRef
```

History is optional evidence and is never part of the current computation's
identity. Snapshots are provider materializations. A `.capsule` file is a
portable, verified object-closure bundle whose identity is its root
`ComputationRef`, not its envelope bytes.

## Commands

```bash
ato run .
ato run github.com/owner/repository
ato lock .
ato encap . --output app.capsule
ato decap start app.capsule
ato ps
ato logs run
ato stop run
```

`capsule.toml`, when present, is repository-adapter input for the concrete
`ato.workspace@1` semantics. It is compiled away and is not canonical.

## Repository layout

```text
lib/
  computation/        canonical semantic values and identity
  kernel/             minimal transition dispatcher
  objects/            CAS, verified loading, closure bundles, signatures, GC boundary
  ipc/                process wire DTOs
extensions/
  semantics/compose/  capsule.compose@1 small-step semantics
  semantics/workspace/ ato.workspace@1 semantics
  adapters/repository/ source inference and authoring compilation
  providers/nacelle/  sandboxed process realization
  providers/snapshot/ physical snapshot materialization and guest agent
apps/
  cli/                ato product assembly
  desktop/            native launcher and MCP process bridge
services/
  netd/               network policy/transport service
  ato-tsnetd/         tailnet sidecar
tools/
  snapshot-builder/   snapshot materialization tool
  arch-check/         cargo-metadata dependency validator
```

## Validation

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo run -p arch-check
cargo check --manifest-path apps/desktop/Cargo.toml --all-targets
cargo test --manifest-path apps/desktop/Cargo.toml
```

See [Computation Architecture](docs/rfcs/accepted/COMPUTATION_ARCHITECTURE.md)
and [Migration Matrix](docs/MIGRATION_MATRIX.md).
