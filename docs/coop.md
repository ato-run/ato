# COOP and Ato

**Status:** Integration concept / non-normative

COOP (**Co-Operation Protocol**) is a collaboration protocol and interface model
for humans and computational actors to participate in the same work context.
It is intended to make an AI agent a visible, interruptible participant in work,
not merely something invoked through a chat or CLI turn.

This document explains how Ato can realize COOP. It does **not** add COOP nouns
to Ato's Semantic Core and is not a normative COOP specification.

## User model

COOP should support two equally important modes of work.

### Work together on the same surface

A human and an agent can observe and operate the same software surface at the
same time.

```text
Human controller ───┐
                    ▼
               same Surface
                    ▲
Agent controller ───┘
```

Examples:

- a human fills a form while an agent checks another field;
- a human reproduces a bug while an agent inspects the same browser state;
- an agent proposes or applies an operation while the human can observe,
  interrupt, or take over.

The important property is shared operation, not remote-control theatre. Both
participants remain first-class actors and the execution authority determines
which operations were actually applied.

### Work in parallel on separate surfaces

Participants can also work toward the same objective without sharing one screen.

```text
                    COOP work context
                    /               \
                   /                 \
Human ── Surface A                    Surface B ── Agent
        writing                       researching
```

A human may keep writing while a research agent uses another browser surface,
or a coding agent may work in a terminal while a human reviews a web result.
The user should be able to see what the other participant is doing and enter its
surface when useful.

The product-level mental model is therefore:

> **Work together, or work in parallel. Move between the two when needed.**

## COOP is above the Ato Semantic Core

Ato's Semantic Core remains:

```text
Computation
Port / Protocol
Evolution
Composition
Contract
```

COOP describes collective work around computations. It does not redefine what a
Computation, Capsule, Run, Record, or Materialization is.

A useful layering is:

```text
COOP
  participants / awareness / coordination / work closure
        │
        ▼
Ato coordination + runtime
  Activity / Actor / Controller / Run / Runner
        │
        ▼
Ato Semantic Core
  Computation / Port / Evolution / Composition / Contract
```

A COOP Workspace is therefore a collaboration scope, not a new Ato semantic
primitive and not the same thing as the `ato.workspace` Adapter protocol.

## Mapping COOP onto Ato

The mapping should remain explicit rather than making the two models identical.

| COOP concept | Ato realization |
| --- | --- |
| Participant / Actor | Ato `Actor` participating in an `Activity` |
| Participant connection | `Controller Session`; connection identity is not Actor identity |
| Shared or parallel activity | one `Activity` composed from one or more `Run`s |
| Operation | authorized Controller operation targeting a Run/Surface |
| Apply authority | the owning `Runner` |
| Effect / receipt | Runner-authoritative applied/failed/unknown result plus observations |
| Persistent computation point | a sealed Ato `Capsule` when continuation is required |
| Evidence of computation evolution | Ato `Record` |
| Software-specific target/action semantics | Protocol + Adapter/Profile, not COOP Core or Ato Core |

Two distinctions are important:

```text
COOP Operation != Ato Record
COOP Workspace != Ato Computation
```

A COOP Operation expresses collaborative intent/action. An Ato Record is
evidence that a Computation evolved. A Runner receipt and resulting Record may
provide evidence for a COOP effect, but they are not the same object.

## Authority and effects

COOP must preserve Ato's existing execution-authority boundary.

```text
Controller
    │ proposes / sends operation
    ▼
Coordinator
    │ identity / grants / topology
    ▼
Runner
    │ validate
    │ apply through Adapter
    │ establish authoritative order
    ▼
Computation evolves
    │
    ▼
receipt / observation / Record
```

Receiving an operation is not the same as applying it. Likewise, successful
application is not automatically the same as observing the requested user-level
effect.

A COOP-facing implementation should be able to distinguish at least:

```text
proposed
→ authorized
→ dispatched
→ applied | failed | unknown
→ effect observed
→ optionally verified
```

The Coordinator may decide **who**, **where**, and **may**. The Runner remains
the authority for **do**, **order**, and **result**.

No universal global event sequence is required. Runner-local authoritative order
plus explicit causal relationships is sufficient unless a particular protocol
requires a stronger ordering domain.

## Awareness is not the audit log

COOP needs realtime awareness such as:

```text
presence
focus
intent
current work
```

These signals can be ephemeral, lossy, coalesced, and renderer-specific. They
should not force pointer frames, partial transcripts, or every UI observation
into durable Ato Records.

Durable facts such as applied operations, commitments, deliveries, evaluations,
or acceptance decisions may require persistence, but their persistence policy is
separate from realtime awareness.

## Renderer model

Chat is one renderer for coordination, not the center of the protocol.

A COOP renderer may use:

- cursors or selections for focus;
- halos or previews for intent/proposals;
- inline questions and choices for coordination;
- attributed animations for applied operations;
- receipts, diffs, tests, or result cards for effects and deliveries;
- text or voice when object-grounded interaction is insufficient.

COOP should describe the meaning of an interaction without requiring one visual
presentation.

## Ato product realization

The first Ato realization should keep one UI source and vary the execution
substrate.

```text
                         COOP UI
                         ato-pwa
                            │
               ┌────────────┴────────────┐
               │                         │
          hosted/web                 local/desktop
               │                         │
           ato-api               Local Coordinator
               │                         │
        Hosted Runner              Local Runtime
               └──────────┬──────────────┘
                          ▼
                    Protocol Adapters
```

`ato-pwa` is the first COOP renderer. `ato-api` provides hosted identity,
authority, participation, and topology. Ato Runners and Adapters establish
execution and effects. `ato-desktop` can later expose the same COOP UI to local
files, terminals, local browser profiles, native applications, and local agents.

Desktop is therefore a capability expansion, not a second COOP product.

## v0 experience

The first COOP experience should be deliberately narrow: Browser-first, one
human and one agent, with the same UI working in a normal browser and later in
the Desktop shell.

The minimum experience is:

```text
1. Human joins an Activity.
2. Agent joins as another Actor.
3. Human and Agent can operate the same Browser Run.
4. Each can observe the other's presence/focus/activity.
5. Human and Agent can also work on separate Browser Runs/Surfaces in parallel.
6. The human can enter or switch to the Agent's current surface.
7. Applied operations are acknowledged by Runner authority.
8. The resulting effect is observable by the participants.
```

This is enough to validate the central interaction hypothesis:

> **Can humans and agents work naturally as participants in the same computing
> environment without the human becoming the router between agent sessions?**

Task discovery, delivery/acceptance workflows, robotics, federation, and a full
cross-domain work model can be layered on after this interaction model is proven.

## Non-goals

This Ato integration does not make COOP:

- a replacement for MCP or agent tool protocols;
- a generic multi-agent task orchestrator;
- a chat-message protocol;
- a new Ato computation primitive;
- a requirement that every participant see the same screen;
- a requirement that every realtime frame become semantic durable state;
- a Desktop-only protocol;
- a protocol that moves execution ordering from Runner to Coordinator.

## Specification ownership

COOP should remain usable independently of Ato. A dedicated COOP specification
can own protocol schemas, event semantics, profiles, and conformance fixtures.
This repository should document only Ato's mapping to that protocol and the
constraints required to preserve Ato's Computation / Runner / Adapter model.

Until such a specification is versioned, this page is explanatory architecture
documentation rather than implementation authority. Accepted Ato RFCs and code
remain authoritative for Ato behavior.
