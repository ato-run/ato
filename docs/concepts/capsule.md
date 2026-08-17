# Capsule and Run

Computation, Capsule, and Run describe different parts of one lifecycle.

## Computation

A Computation is an evolving residual process. It can take a transition and
become a successor Computation.

## Capsule

```text
seal(C) → Capsule
```

A **Capsule is a persistent open continuation**: an immutable, addressable
Computation point that can be preserved, passed, resumed, composed, and
forked. “Open” means that work remains; “persistent” means the point can be
named independently of one active process.

In the current implementation, a Capsule is identified by a
`ComputationRef`. A `.capsule` file is a transport bundle rooted at that ref;
it is not the Capsule's identity.

## Run

```text
resume(Capsule) → Run
fork(Capsule) → RunA, RunB
```

A **Run** is an active evaluation. Its head changes as interactions evolve the
Computation. Two Runs may start from the same Capsule and develop different
futures without modifying their shared origin.

```text
Capsule   immutable
Run       evolving
Record    evidence of evolution
```

Branch names and lineage help locate or relate computation points, but do not
change the identity of those points.

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
