# Computation

Take a concrete example: Alice runs a build and hits `error[E0277]` at a
point we call `C42`. Her shell is still sitting there, interactive, waiting
for the next command.

`C42` is not "everything Alice typed to get here" — `git clone`, `npm
install`, `npm run build` are done and gone. What matters about `C42` is what
can still happen from it: Bob can type a command, edit the source, rerun the
build. That "what can still happen" is what Ato calls a **Computation**.

A **Computation** is the work remaining from a current point, not a
transcript of everything that happened before it. In process-theory terms
this is an evolving *residual computation*.

## Evolution

```text
C ──α──▶ C'
```

An interaction `α` — Bob typing `npm run build`, a process writing to
stdout — evolves `C` into its successor `C'`. `C42 ──α──▶ C43` is Bob's fix
landing and the build succeeding. A [Record](capsule.md#record) may preserve
evidence that the transition occurred, but the Record and the preceding
history are not the current Computation — only `C43` is.

## Composition

Alice's terminal is one Computation, but it is built from smaller ones — the
shell process, its PTY, the workspace filesystem — wired together. Composing
Computations produces another Computation:

```text
composeW(C1, ..., Cn) = C
```

`W` is explicit wiring between compatible Ports. Interactions hidden inside
the composition become internal `Tau` transitions; exported Ports form the
new boundary. Composition is closed: its result is not a separate service
graph or application category.

## Port and Endpoint (theory vs. implementation)

A **Port** is a typed interaction boundary on a Computation — for example,
the terminal's stdin/stdout Port, or the workspace's filesystem Port. Its
Protocol and role determine which interactions are valid. In the current
implementation this is `PortId`/`PortDef` on a Computation's `Boundary`.

Composition needs to say *which* Port on *which* child it is wiring up; the
core library's `Endpoint` type (`{ node, port }`) is that internal selector —
it identifies a child Port inside a Composition graph. It is a graph-wiring
detail, not a runtime resource.

Do not confuse it with the physical thing a Port gets bound to at
[realization time](materialization.md#binding-and-runtime-endpoint) — a
socket, PTY, or file descriptor. This document calls that a **runtime
endpoint** (lowercase) specifically to avoid colliding with the `Endpoint`
composition-graph term above; the theory further imagines a persistent,
realization-independent reference to a Port surviving across resumes and
hosts, sometimes called a `PortRef`. **`PortRef` is not a type in the current
codebase** — treat it as a model/future concept, not an implemented one.

## Contract

A **Contract** is a predicate over a Computation:

```text
C ⊨ K
```

"Build succeeded," "compiler error reproduced," "game score reached," and
"terminal interactive" are different Contracts — `C42 ⊨ "compiler error
reproduced"` holds, `C43 ⊨ "build succeeded"` holds. Ready State is therefore
not a special semantic primitive; it is one possible Contract and
realization concern.

## State as projection

State is an observation selected for a purpose, not the semantic center:

```text
StateK(C)
```

Different observers or Contracts may project different state from the same
Computation — the compiler's error text, the file diff between `C42` and
`C43`, the terminal's visible buffer. A filesystem tree, memory image, UI
view, or protocol state can be useful without becoming the identity of the
Computation.

## Implementation note

The current implementation represents a sealed point as
`ComputationObject { semantics, boundary, residual }` and addresses its
canonical bytes with a `ComputationRef`. See the accepted
[Computation Architecture](../rfcs/accepted/COMPUTATION_ARCHITECTURE.md).
