# Computation

A **Computation** is the work remaining from a current point. It is an evolving
residual computation, not a transcript of everything that happened before it.

## Evolution

```text
C ──α──▶ C'
```

An interaction `α` evolves `C` into successor `C'`. The current computation is
`C'`. A Record may preserve evidence that the transition occurred, but the
Record and the preceding history are not the current Computation.

## Composition

Computations compose to produce another Computation:

```text
composeW(C1, ..., Cn) = C
```

`W` is explicit wiring between compatible Ports. Interactions hidden inside
the composition become internal `Tau` transitions; exported Ports form the
new boundary. Composition is closed: its result is not a separate service
graph or application category.

## Port and PortRef

A **Port** is a typed interaction boundary on a Computation. Its Protocol and
role determine which interactions are valid.

A **PortRef** is a logical, persistent reference to a Port. It is not a socket,
file descriptor, or URL. A Binding maps that logical reference to a
runtime-specific Endpoint when a Computation is realized.

## Contract

A **Contract** is a predicate over a Computation:

```text
C ⊨ K
```

“Web app ready,” “compiler error reproduced,” “game score reached,” and
“terminal interactive” are different Contracts. Ready State is therefore not
a special semantic primitive; it is one possible Contract and realization
concern.

## State as projection

State is an observation selected for a purpose, not the semantic center:

```text
StateK(C)
```

Different observers or Contracts may project different state from the same
Computation. A filesystem tree, memory image, UI view, or protocol state can be
useful without becoming the identity of the Computation.

## Implementation note

The current implementation represents a sealed point as
`ComputationObject { semantics, boundary, residual }` and addresses its
canonical bytes with a `ComputationRef`. See the accepted
[Computation Architecture](../rfcs/accepted/COMPUTATION_ARCHITECTURE.md).
