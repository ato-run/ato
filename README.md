# Ato

Ato preserves software as an evolving computation rather than treating a
repository, lockfile, runtime, or snapshot as the product identity.

The authoring lifecycle is:

```text
init → record interaction → stop/seal → resume/branch → encap → run
```

## Author a Capsule

Create an explicit `capsule.toml` in a project directory:

```toml
schema = 1

[[process]]
id = "app"
command = ["python", "app.py"]
cwd = "."

[[adapter]]
target = "app"
use = "ato.process@1"

[[adapter]]
target = "workspace"
use = "ato.workspace@1"

[encap]
materializers = ["ato.replay@1"]
```

Ato does not infer or install Python, packages, runtimes, or toolchains. The
declared command starts using the host environment and fails normally when a
required executable is unavailable.

Start and seal an authored Run:

```sh
ato init demo
ato stop demo
```

Continue its current branch:

```sh
ato resume demo@main
ato stop demo
```

Create a new future from a historical Record without rewriting `main`:

```sh
ato resume demo@main#42 --branch experiment
ato stop demo
```

Make one logical point portable, then consume it ephemerally:

```sh
ato encap demo@main \
  --materialize ato.replay@1 \
  -o demo.capsule

ato run demo.capsule
```

`ato run` accepts only portable `.capsule` files. Use `ato init` for local
authoring; Git repositories and URLs are not runnable inputs.

## State and identity

- `ComputationObject { semantics, boundary, residual }` is immutable.
- Its canonical JCS bytes produce a BLAKE3 `ComputationRef`.
- `.capsule/objects` stores immutable computation and content objects.
- `.capsule/refs/heads/*` are atomic mutable pointers into that DAG.
- `.capsule/records` stores protocol-neutral evidence and Record cursors.
- `.capsule/runs` stores active physical Run metadata, never a sealed head.
- A portable `.capsule` bundle has version 2. Its identity is the root
  `ComputationRef`, not its bytes or Materialization set.

## Architecture

```text
lib/
  computation/      identity, Ports, pure composition wiring
  compose/          operational small-step composition
  kernel/           protocol-aware, payload-opaque evolution
  objects/          verified CAS, local repository, records, bundle v2
  ipc/              process-boundary DTOs

extensions/
  adapters/         public API plus process, PTY, workspace, binding, HTTP
  materializers/    public API plus replay and verify-only snapshot

apps/
  cli/              lifecycle supervisor and product assembly
```

Adapters connect physical interactions to Protocols. Materializers encode or
restore one selected computation point. The Kernel depends on neither concrete
Adapters nor Materializers.
