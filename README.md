# Ato

**Share where a computation got to. Continue from there.**

Ato lets you share a point in a computation so someone else can resume or
fork from it — without sending the repository, the setup steps, or a
description of what went wrong.

## A concrete example

Alice is debugging a build.

```text
Alice's terminal

$ git clone …
$ npm install
$ npm run build
error[E0277]: the trait bound `Foo: Send` is not satisfied
  --> src/lib.rs:42:5
$                                        ← still sitting at the shell
```

Normally, to get help, Alice would have to send Bob:

- the repository and the exact commit,
- the install/build commands,
- a description of the error,
- and hope Bob's machine reproduces it the same way.

With Ato, she shares the point itself:

```sh
ato encap demo@main --materialize ato.replay@1 -o build-error.capsule
```

```text
Alice ──(build-error.capsule)──▶ Bob
```

Bob opens it and lands in the *same* place — same files, same failing build,
same interactive shell — and keeps going:

```sh
ato run build-error.capsule
```

```text
Bob's terminal (after `ato run build-error.capsule`)

$ npm run build
error[E0277]: the trait bound `Foo: Send` is not satisfied
  --> src/lib.rs:42:5
$ vim src/lib.rs        ← Bob edits and keeps debugging from here
```

That shareable point — "I got here, start from here" — is what Ato calls a
**Capsule**. In the theory, we call it a **persistent open continuation**: a
computation point you can preserve, pass to someone else, resume, compose,
and fork.

## Why not just a VM snapshot or a Docker image?

Those *can* be used to physically reproduce a point, but they aren't what
Ato treats as identity. Ato separates two questions:

```text
Capsule            answers: where should I continue from?
Materialization     answers: how do I get back there, on this machine?
```

A Capsule is a logical point. A Materialization — Replay, a filesystem
reconstruction, a future process/VM checkpoint — is one physical way to
reach it. Swapping the physical method doesn't change which point you're
at, the same way swapping a file's storage medium doesn't change which
file it is.

## The lifecycle, in one picture

```text
work / interaction

C41 ─────▶ C42 ─────▶ C43
            │
          seal
            ▼
      Capsule C42
            │
         resume
            ▼
           Run
            │
        continue
            ▼
      further work
```

`C41`, `C42`, `C43` are successive points in Alice's computation — before the
error, at the error, and after Bob's fix. `seal` freezes `C42` as an
immutable, addressable Capsule; `resume` turns it back into an active,
evolving Run. See [how a Capsule gets restored](docs/concepts/materialization.md)
for the second half of this picture — Replay today, other strategies as
future work.

## Core concepts

Everything below uses the same example: `C41` (before the build), `C42` (the
build error, terminal still interactive), `C43` (after Bob's fix).

### Computation

At `C42`, **Computation** means what can still happen from `C42` onward — not
the list of commands that produced it. Alice's `git clone`/`npm install`
history is not part of `C42`; what matters is that the shell is still there,
interactive, waiting for the next command.

```text
C ──α──▶ C'
```

An interaction `α` (Bob typing a command, a process producing output) evolves
Computation `C` into its successor `C'`. In process-theory terms this is a
*residual computation*: what remains, not what already ran. Computations
interact through typed Ports and can be composed into another Computation.

### Capsule

```text
seal(C42) → Capsule C42
```

A Capsule is a point you can save and share, not a copy of a machine. It is
not a VM, container, snapshot, replay log, manifest, or lockfile — those may
describe or physically realize it, but none of them defines its identity.

### Run

```text
resume(Capsule C42) → Run
```

When Bob opens `build-error.capsule`, resuming it creates a **Run** — the
active, mutable evaluation whose head advances as Bob works, moving toward
`C43`. The Capsule `C42` itself never changes; the Run is what evolves from
it.

### Record

A **Record** is evidence of an observed transition — e.g. that the build ran
and produced that error. Records explain how Alice's Run reached `C42`; they
are history, not `C42` itself, and they are how Replay reconstructs a point.

### Materialization

A **Materialization** is a physical way to realize a Capsule. Replay can
reconstruct `C42` today by replaying recorded interactions. A future
checkpoint implementation could reach the same logical point by a different
physical path — same Capsule, different Materialization.

## What Ato is not

- not a VM snapshot format
- not a container registry
- not deterministic record/replay for every process
- not a Git replacement
- not a universal build system

These can participate in *realizing* or *producing* a Capsule, but none of
them is the Capsule's identity.

## Try the lifecycle

```sh
# Create C0 and start a durable authored Run.
ato init demo

# ...do some work...

# Quiesce the Run and advance the immutable branch head.
ato stop demo

# Resume that head, or fork a historical point onto a new branch.
ato resume demo@main
ato resume demo@main#42 --branch experiment

# Export one point and consume it as a portable, ephemeral Run.
ato encap demo@main --materialize ato.replay@1 -o demo.capsule
ato run demo.capsule
```

`ato run` accepts portable `.capsule` files. It does not accept a local
repository or Git URL — use `ato init` for local authoring. The manifest
format (`capsule.toml`), Adapters, and schema versioning are covered in
[Local lifecycle](docs/run.md), not repeated here.

## What works today

**Implemented in the current tree:**

- immutable `ComputationObject` values and content-addressed `ComputationRef`s;
- Kernel evolution through typed Ports and registered Protocol semantics;
- closed Computation composition with explicit connections and internal `Tau`;
- local immutable objects, branch heads, active Runs, Records, and lineage;
- the `init`, `resume`, `stop`, `encap`, and portable `run` lifecycle;
- Process, PTY, Workspace, Binding, and HTTP Adapters;
- portable `.capsule` bundle v2 with verified object closure;
- protocol-generic Replay as a restore-capable Materializer.

**Experimental or limited:**

- the lifecycle and extension APIs are still evolving on `nightly`;
- `ato.snapshot@1` captures and verifies a workspace/filesystem
  Materialization, but has no physical restore implementation (verify-only);
- Adapter coverage and Contract evaluation are narrower than the full model.

**Model / future work — not implemented as a general system today:**

heterogeneous Materializations (process checkpoints, VM snapshots),
contract-equivalent realization, distributed capture, cross-host resume, and
a persistent, realization-independent Port reference. These are examples of
where the model is heading, not present product claims.

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

Start with the [documentation map](docs/README.md) — it routes by what
you're trying to do (use Ato, contribute, or study the theory) instead of
one fixed reading order. Quick links:

- [Computation](docs/concepts/computation.md)
- [Capsule and Run](docs/concepts/capsule.md)
- [Materialization](docs/concepts/materialization.md)
- [Glossary](docs/glossary-reference.md)
- [Core architecture](docs/core-architecture.md)
- [Accepted RFCs](docs/rfcs/README.md)

## Theory / research

The [Capsule Process Model](docs/theory/capsule-process-model.md) is an
optional deep dive: a **design-hypothesis draft**, not a claim of novelty and
not a statement that every realization or distribution mechanism described
is implemented.

## Contributing and license

Read [AGENTS.md](AGENTS.md) before changing architecture or implementation.
The Rust packages currently declare `Apache-2.0 OR MPL-2.0` in their package
metadata.
