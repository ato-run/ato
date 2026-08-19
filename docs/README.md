# Ato documentation

This documentation distinguishes the current semantic model, implemented
architecture, theory, and historical designs. A document's layer matters:
theory describes intended semantics; accepted RFCs and code define current
behavior; archived documents are evidence, not authority.

## 60-second model

```text
Computation
  what can still happen from here

Capsule
  one immutable point in that computation

Run
  an active continuation from that point

Record
  evidence of how a Run evolved

Materialization
  a physical way to get back to the point
```

This is a summary, not a substitute for the [Glossary](glossary-reference.md)
or the worked example in [Computation](concepts/computation.md).

## Pick a reading path

**Using Ato**

1. [README](../README.md) — the concrete example and lifecycle
2. [Local lifecycle](run.md) — verified CLI behavior, manifest details
3. [Capsule bundle](capsule.md) — the `.capsule` transport format

**Contributing to Ato**

1. [Computation](concepts/computation.md), [Capsule and Run](concepts/capsule.md),
   [Materialization](concepts/materialization.md) — the concepts
2. [Core architecture](core-architecture.md) — library responsibilities and
   dependency direction
3. [Accepted RFCs](rfcs/README.md) — normative current architecture
4. [AGENTS.md](../AGENTS.md) — before changing architecture or implementation

**Research / theory**

1. [Computation](concepts/computation.md), [Capsule and Run](concepts/capsule.md),
   [Materialization](concepts/materialization.md) — the concepts
2. [Capsule Process Model](theory/capsule-process-model.md) — **Theory
   Draft**; a design-hypothesis deep dive, not an implementation status
   document

## Full index

### Concepts

- [Computation](concepts/computation.md) — evolution, composition, Ports,
  Contracts, and state projections.
- [Capsule and Run](concepts/capsule.md) — the immutable point and its mutable
  evaluation.
- [Materialization](concepts/materialization.md) — logical identity versus
  physical realization.
- [Glossary](glossary-reference.md) — canonical terminology.

### Architecture

- [Core architecture](core-architecture.md) — library responsibilities and
  dependency direction.
- [Accepted RFCs](rfcs/README.md) — normative current architecture.
- [Local lifecycle](run.md) — verified CLI behavior.
- [Capsule bundle](capsule.md) — transport and identity.
- [Snapshot materialization](snapshot.md) — the limited current snapshot
  implementation.

### Theory

- [Capsule Process Model](theory/capsule-process-model.md) — **Theory Draft**;
  intended semantics, not an implementation status document.

### Implementation and protocol details

- [Protocol Adapter RFC](rfcs/accepted/PROTOCOL_ADAPTER.md)
- [Composition RFC](rfcs/accepted/COMPOSITION.md)
- [Object Closure Bundle RFC](rfcs/accepted/OBJECT_BUNDLE.md)
- [Materialization RFC](rfcs/accepted/MATERIALIZATION.md)
- [Internal engineering material](internal/README.md)

### Historical material

[The archive](archive/README.md) contains superseded manifest-, lock-,
Ready-State-, provider-, and Store-centered models. Historical documents must
not be used as current API or architecture authority.
