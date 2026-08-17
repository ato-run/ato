# Capsule and Run

Continuing the [compiler-error example](computation.md): Alice hits
`error[E0277]` at point `C42`. Computation, Capsule, and Run describe
different parts of what happens to `C42` next.

## Computation

A Computation is an evolving residual process. It can take a transition and
become a successor Computation. `C42` is Alice's Computation at the moment of
the error; see [Computation](computation.md) for the full explanation.

## Capsule

```text
seal(C42) → Capsule C42
```

Alice runs `ato encap` on `C42`, sealing it into a Capsule. A **Capsule is a
persistent open continuation**: an immutable, addressable Computation point
that can be preserved, passed, resumed, composed, and forked. "Open" means
work still remains — Bob can keep debugging; "persistent" means the point can
be named and shared independently of Alice's still-running process.

In the current implementation, a Capsule is identified by a
`ComputationRef`. The `.capsule` file Alice sends Bob is a transport bundle
rooted at that ref; the file's bytes are not the Capsule's identity — see
[Capsule bundle](../capsule.md).

## Run

```text
resume(Capsule C42) → Run
fork(Capsule C42) → RunA, RunB
```

When Bob runs `ato run build-error.capsule`, resuming `Capsule C42` creates a
**Run** — an active evaluation. Its head changes as Bob's interactions evolve
the Computation, moving from `C42` toward `C43`. If Bob and a teammate both
resume the same Capsule, two Runs develop different futures without
modifying their shared origin `C42`.

```text
Capsule   immutable   (C42, forever)
Run       evolving    (Bob's session, moving toward C43)
Record    evidence    (what Bob's Run actually did)
```

Branch names and lineage help locate or relate computation points, but do
not change the identity of those points.

## Record

A **Record** is durable evidence that an interaction was observed — for
example, that Bob's edited source was compiled and the build succeeded.
Records are how Alice's `git clone`/`npm install`/`npm run build` history got
recorded in the first place, and how [Replay](materialization.md) reconstructs
`C42` on a fresh machine. A Record explains how a Run reached a point; it is
not the point itself, and it does not define the current Capsule.

## What a Capsule is not

```text
Capsule != Snapshot
Capsule != VM
Capsule != Container
Capsule != Replay Log
Capsule != capsule.toml
```

Each item may participate in authoring, transport, observation, or physical
realization. None substitutes for the logical continuation.
