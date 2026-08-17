# Capsule Process Model

> **Status: Theory Draft.** This document describes the intended semantic
> model. Not all semantics, realization strategies, authority rules, or
> distribution mechanisms are implemented.

The model starts from Computation, an evolving residual process:

```text
C ──α──▶ C'
```

Sealing makes a persistent logical point:

```text
seal(C) → Capsule κ
resume(κ) → Run
```

The Capsule is an open continuation because evaluation can continue after the
seal. It is persistent because its logical identity survives any one evaluator
or physical realization. A Run is the evolving evaluation; Records are
evidence of its transitions.

Computations compose through typed Ports:

```text
composeW(C1, ..., Cn) = C
```

Contracts describe observations or obligations over a point:

```text
C ⊨ K
```

The research position is that logical continuation identity can be independent
of physical realization, and that realization equivalence may be indexed by a
Contract. This is a design hypothesis, not a novelty claim or a statement that
arbitrary Replay, process, container, and VM artifacts are interchangeable
today.

For implemented behavior, use the [current status](../../README.md#current-status)
and [accepted RFCs](../rfcs/README.md), not this theory draft.
