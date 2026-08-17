# Glossary

These terms are normative for current public documentation.

- **Computation** — the evolving residual computation at a current point. It
  is not state, history, a trace, or a runtime process.
- **Evolution** — a transition `C ──α──▶ C'` from one Computation to its
  successor.
- **Composition** — explicit Port wiring that combines Computations and
  produces another Computation: `composeW(C1, ..., Cn) = C`.
- **Capsule** — an immutable, addressable, sealed Computation point: a
  persistent open continuation. The current implementation identifies it by a
  `ComputationRef`.
- **Run** — a mutable evaluation whose head advances between immutable
  Computation points. A Run is not a Capsule.
- **Port** — a typed interaction boundary owned by a Computation.
- **PortRef** — a logical, persistent reference to a Port. It is not a socket,
  file descriptor, URL, or other runtime Endpoint.
- **Endpoint** — a physical interaction endpoint chosen during realization,
  such as a socket or PTY attachment.
- **Binding** — the realization-time mapping from a logical requirement or
  PortRef to an Endpoint or provider resource. Secret values are runtime
  inputs, not logical identity.
- **Contract** — a predicate or obligation over a Computation, written
  `C ⊨ K`. Readiness is one possible Contract, not a universal primitive.
- **Record** — durable evidence that an Evolution was observed. A Record is
  not the Computation and does not define its current state.
- **Trace** — an ordered or causally related view of past Records. History is
  not the current Computation.
- **Materialization** — a physical representation or reconstruction strategy
  for a Capsule. It is not Capsule identity.
- **Evaluator** — a physical mechanism that advances a realized Computation.
  It belongs to runtime realization, not the Semantic Core.
- **Lineage** — evidence relating a branch or fork to an origin Computation and
  optional parent Record. It is not part of Computation identity.
- **Fork** — creation of a new Run or branch future from an existing Capsule
  without mutating that Capsule.
- **Resume** — realization and continued evaluation of a Capsule as a Run.
- **Replay** — a Materialization strategy that reconstructs a target by
  applying recorded interactions from a verified anchor.
- **State** — a purpose-specific projection `StateK(C)` of a Computation. State
  is not the semantic primitive.
- **ComputationObject** — the current canonical representation
  `{ semantics, boundary, residual }` of a sealed Computation.
- **ComputationRef** — the immutable address derived from canonical
  ComputationObject bytes; the current Capsule handle.
- **`.capsule`** — a portable object-closure bundle rooted at a
  ComputationRef. Bundle bytes and included Materializations are not Capsule
  identity.

## Legacy terminology

The following framings are historical and must not be used as current
definitions: Capsule as application package, manifest, VM snapshot, Ready-State
artifact, lockfile, or Store revision; lockfile-centered reproducibility as
Ato's semantic core; and “source → detect → resolve → run” as the universal
Capsule lifecycle.
