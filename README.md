# Ato

Ato preserves and transfers running computation.

A **Capsule is a persistent open continuation**: a computation point that you
can preserve, pass to someone else, resume, compose, and fork. In practical
terms, it says: **“I got here. Start from here.”**

```text
C0 ──α──▶ C1 ──α──▶ C2
                     │
                   seal
                     ▼
                 Capsule κ
                     │
          ┌──────────┼──────────┐
          ▼          ▼          ▼
        Replay     process       VM
                  checkpoint   snapshot

          different physical realizations
             of one logical point
```

**The checkpoint is replaceable. The continuation is the identity.**

## Why Ato

Software is usually shared as source files, images, configuration, or setup
instructions. The recipient must reconstruct the point the author reached—or
the author must keep hosting it as a service.

Ato instead makes the continuation point addressable. A recipient can resume
or fork from that point without treating the reconstruction method as its
identity. Repository execution, runtime setup, and source reconstruction can
help realize a computation, but they are not Ato's semantic core.

## Core concepts

### Computation

A **Computation** is the work that remains at the current point, not the trace
of work already performed.

```text
C ──α──▶ C'
```

An interaction `α` evolves computation `C` into its successor `C'`.
Computations interact through typed Ports and can be composed into another
Computation.

### Capsule

Sealing a Computation makes one immutable, addressable point:

```text
seal(C) → Capsule
```

A Capsule is not a VM, container, snapshot, replay log, manifest, or lockfile.
Those may describe or physically realize it; none defines its logical identity.

### Run

Resuming a Capsule creates a **Run**, the mutable runtime object whose head
evolves:

```text
resume(Capsule) → Run
```

The Capsule remains immutable. The Run advances.

### Materialization

A **Materialization** is a physical way to realize a Capsule. Replay,
filesystem reconstruction, process checkpoints, container checkpoints, VM
snapshots, source reconstruction, and remote live state are possible
strategies. Different Materializations can represent the same logical Capsule.

## Minimal lifecycle

Authoring currently starts from an explicit `capsule.toml`. The command uses
the host environment as declared; Ato does not infer or install missing
runtimes or toolchains.

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

```sh
# Create C0 and start a durable authored Run.
ato init demo

# Quiesce the Run and advance the immutable branch head.
ato stop demo

# Resume that head, or fork a historical point onto a new branch.
ato resume demo@main
ato resume demo@main#42 --branch experiment

# Export one point and consume it as an ephemeral Run.
ato encap demo@main --materialize ato.replay@1 -o demo.capsule
ato run demo.capsule
```

`ato run` accepts portable `.capsule` files. It does not accept a local
repository or Git URL. Use `ato init` for local authoring.

## Current status

### Implemented in the current tree

- immutable `ComputationObject` values and content-addressed `ComputationRef`s;
- Kernel evolution through typed Ports and registered Protocol semantics;
- closed Computation composition with explicit connections and internal `Tau`;
- local immutable objects, branch heads, active Runs, Records, and lineage;
- the `init`, `resume`, `stop`, `encap`, and portable `run` lifecycle;
- Process, PTY, Workspace, Binding, and HTTP Adapters;
- portable `.capsule` bundle v2 with verified object closure;
- protocol-generic Replay as a restore-capable Materializer.

### Experimental or limited

- the lifecycle and extension APIs are still evolving on `nightly`;
- `ato.snapshot@1` captures and verifies a workspace/filesystem
  Materialization, but has no physical restore implementation;
- Adapter coverage and Contract evaluation are narrower than the full model.

### Model / future work

The semantic model allows heterogeneous Materializations, contract-equivalent
realization, distributed capture, cross-host resume, and persistent PortRef
authority. The current repository does **not** yet implement these as a general
end-to-end system. Process and VM checkpoint interchangeability are examples
of the model, not present product claims.

## Architecture

```text
lib/
  computation/      identity, Ports, pure composition wiring
  kernel/           protocol-aware, payload-opaque evolution
  compose/          operational small-step composition
  objects/          verified CAS, Records, lineage, bundle v2
  ipc/              process-boundary DTOs

extensions/
  adapters/         physical interaction ↔ Protocol
  materializers/    physical realization of a selected point

apps/
  cli/              lifecycle supervisor and product assembly
  desktop/          separate desktop shell
```

Placement, distribution, sandboxing, providers, and VMs are realization
concerns. They do not introduce a second semantic identity.

## Documentation

Follow the [documentation map](docs/README.md), beginning with:

- [Computation](docs/concepts/computation.md)
- [Capsule and Run](docs/concepts/capsule.md)
- [Materialization](docs/concepts/materialization.md)
- [Glossary](docs/glossary-reference.md)
- [Core architecture](docs/core-architecture.md)
- [Accepted RFCs](docs/rfcs/README.md)

The [Capsule Process Model](docs/theory/capsule-process-model.md) is a theory
draft. It explains the intended semantics; it is not a claim that every
realization or distribution mechanism is implemented.

## Contributing and license

Read [AGENTS.md](AGENTS.md) before changing architecture or implementation.
The Rust packages currently declare `Apache-2.0 OR MPL-2.0` in their package
metadata.
