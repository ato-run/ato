# Ato documentation

This documentation distinguishes the current semantic model, implemented
architecture, theory, and historical designs. A document's layer matters:
theory describes intended semantics; accepted RFCs and code define current
behavior; archived documents are evidence, not authority.

## Reading path

```text
README
  ↓
Concepts
  ↓
Architecture
  ↓
Theory
  ↓
Implementation and protocol details
```

### 1. Concepts

- [Computation](concepts/computation.md) — evolution, composition, Ports,
  Contracts, and state projections.
- [Capsule and Run](concepts/capsule.md) — the immutable point and its mutable
  evaluation.
- [Materialization](concepts/materialization.md) — logical identity versus
  physical realization.
- [Glossary](glossary-reference.md) — canonical terminology.

### 2. Architecture

- [Core architecture](core-architecture.md) — library responsibilities and
  dependency direction.
- [Accepted RFCs](rfcs/README.md) — normative current architecture.
- [Local lifecycle](run.md) — verified CLI behavior.
- [Capsule bundle](capsule.md) — transport and identity.
- [Snapshot materialization](snapshot.md) — the limited current snapshot
  implementation.

### 3. Theory

- [Capsule Process Model](theory/capsule-process-model.md) — **Theory Draft**;
  intended semantics, not an implementation status document.

### 4. Implementation and protocol details

- [Protocol Adapter RFC](rfcs/accepted/PROTOCOL_ADAPTER.md)
- [Composition RFC](rfcs/accepted/COMPOSITION.md)
- [Object Closure Bundle RFC](rfcs/accepted/OBJECT_BUNDLE.md)
- [Materialization RFC](rfcs/accepted/MATERIALIZATION.md)
- [Internal engineering material](internal/README.md)

### 5. Historical material

[The archive](archive/README.md) contains superseded manifest-, lock-,
Ready-State-, provider-, and Store-centered models. Historical documents must
not be used as current API or architecture authority.
