# Capsule Process Model

> **Status: Theory Draft.** This document describes the intended semantic
> model. Not all semantics, realization strategies, authority rules, or
> distribution mechanisms are implemented. It is a **design hypothesis, not
> a novelty claim** — see [What Ato is hypothesizing](#16-what-ato-is-hypothesizing)
> for the explicit boundary between prior art and open questions.

This is an optional deep dive. If you just want to use or contribute to Ato,
the [concept docs](../README.md#pick-a-reading-path) are enough; come back
here for the research framing.

## 1. Motivation

Software today is usually shared as source, images, or setup instructions;
the recipient reconstructs a point the author already reached. Ato's
motivating question is whether the *point itself* — not the recipe for
reaching it — can be the unit that is named, shared, and resumed,
independent of which machine or process produced or restores it.

## 2. Residual computation

The model starts from Computation, an evolving residual process — what
remains to happen, not a transcript of what already happened:

```text
C ──α──▶ C'
```

An interaction `α` evolves `C` into successor `C'`. This framing is standard
in process calculi (CCS, the π-calculus): a process's identity at a point in
time is what it can still do, not its execution history.

## 3. Why continuation

Sealing makes a persistent logical point:

```text
seal(C) → Capsule κ
resume(κ) → Run
```

The Capsule is a *continuation* in the same sense as in programming-language
theory: a reified "the rest of the computation from here," capturable and
resumable independent of the original call stack.

## 4. Why open

The Capsule is an **open** continuation because evaluation can continue
after the seal — sealing does not terminate the computation, it makes one
point in it addressable. A closed artifact (a finished trace, a terminated
process's exit state) cannot be resumed; an open one can.

## 5. Ports / interaction

Computations interact through typed Ports and compose through explicit
wiring:

```text
composeW(C1, ..., Cn) = C
```

Ports are the model's interface boundary — the same role interface types
play in π-calculus channels or CSP events: they say *what* can be observed
or driven at a boundary, independent of *how* it is physically carried.

## 6. Capture / seal

`seal(C) → κ` captures the current point without stopping `C`'s evaluator.
The captured value is addressed by its canonical bytes
(`ComputationObject` → `ComputationRef` in the implementation), not by which
evaluator produced it.

## 7. Resume

`resume(κ) → Run` re-establishes an active evaluation from a sealed point.
Resuming does not require the same evaluator, host, or physical
representation that produced `κ` — only a compatible Materialization for
that Computation's boundary and Protocols.

## 8. Materialization-independent realization

The core hypothesis: `κ`'s logical identity is independent of the physical
means used to reach it. Replay, a filesystem reconstruction, or (as future
work) a process/VM checkpoint could all realize the same `κ`. Today only
Replay is restore-capable; the others are model/future work, not
interchangeable in the current implementation.

## 9. Contract-indexed equivalence

Contracts describe observations or obligations over a point:

```text
C ⊨ K
```

The research position is that realization equivalence may be indexed by a
Contract rather than by byte-for-byte artifact equality — two
Materializations "count as the same point" if they satisfy the same
Contract (e.g. `"terminal interactive at the same error"`), even if their
underlying bytes differ. This is a hypothesis about *how* equivalence could
be defined, not a claim that Ato checks it today.

## 10. Fork / lineage

`fork(κ) → RunA, RunB`: two Runs can resume the same Capsule and diverge
without mutating their shared origin. Lineage records which point a Run or
branch descended from; it is evidence, not part of a Capsule's identity.

## 11. Relation to π-calculus

The Computation-as-residual-process framing, typed interaction Ports, and
composition-by-wiring are directly inspired by the π-calculus and other
process calculi, where a process's meaning is given by its possible
interactions rather than its history. Ato does not claim a formal
bisimulation proof for its current implementation; the relation is one of
inspiration and vocabulary, not a verified encoding.

## 12. Relation to Kell / passivation

Passivation calculi (e.g. the Kell calculus) study capturing a *located*,
possibly composite process as a first-class, resumable value — closer in
spirit to sealing a Capsule than plain process calculi, which usually don't
model capture of running state as a value. Ato's `seal`/`resume` pair is
conceptually closer to passivation than to a checkpoint/restore primitive
bolted onto an otherwise history-oriented system.

## 13. Reversible computation / causal history

Reversible process calculi track enough causal history to undo a step.
Ato's Records serve an analogous evidentiary role — they let a Materializer
reconstruct a point by replaying causally related history — but Ato does not
claim general reversibility (undoing an arbitrary side effect); Replay
reconstructs forward from a verified anchor, it does not invert.

## 14. Distributed snapshot

Classical distributed-snapshot algorithms (e.g. Chandy–Lamport) capture a
consistent global state across independently-progressing processes.
Cross-host resume and distributed capture are named in this document as
model/future work; Ato does not yet implement a distributed-snapshot
protocol, and the current `.capsule` bundle is scoped to a single sealed
Computation's closure, not a multi-host consistent cut.

## 15. What is already known

Residual-process semantics, typed interaction, capture/resume, and
distributed snapshots are each independently studied areas with decades of
prior work (process calculi, continuations, passivation, distributed
systems). Ato does not claim to invent any of these individually.

## 16. What Ato is hypothesizing

The specific combination and product framing are the open question:
that a **persistent, physically-realization-independent continuation**, with
**Contract-indexed equivalence** between heterogeneous realization
strategies, is a workable unit for sharing and resuming computation
day-to-day — not just in a formal model, but as something a developer can
seal, send, and resume in practice. That combination, and whether Contract
indexing is the right equivalence notion, is the hypothesis this repository
is testing.

## 17. Implementation status

For implemented behavior, use [Current implementation status](../../README.md#current-implementation-status)
and the [accepted RFCs](../rfcs/README.md), not this theory draft. In short:
Computation identity, composition, and Replay-based Materialization are
implemented; process/VM checkpoints, cross-host resume, contract-indexed
equivalence, and persistent Port references are model/future work.
