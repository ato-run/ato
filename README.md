# Ato

Ato is a computing interface that lets you save, share, and resume files,
applications, execution state, and operation history as one common unit
called a **Capsule**. It isn't just file sharing — the goal is to hand over
a computation's midpoint as-is, so the recipient can pick up and continue
from exactly there. A single file, a file with its application, or a full
working state with history are all expressed through the same unified
Capsule model.

*([日本語 README](README.ja.md))*

---

## Main use cases

- **Faster bug reproduction**: instead of sending repro steps and
  screenshots, share the Workspace, Terminal, and Browser state where the
  error is actually happening, as a Capsule. The recipient starts debugging
  from that exact point, with no environment setup in between.
- **Collaborating with AI agents**: not just a code diff, but the history of
  commands run, files changed, tests, and browser actions can be Replayed
  and inspected. A human can pick up from the working point, or hand it off
  to another AI agent to continue.
- **Sharing application state**: share not just the app itself, but the
  actual state you've built up — "this far, with this model and these
  settings." The recipient can Continue from there and branch off into their
  own future with their own changes.
- **Restoring and handing off dev work**: save a running dev server, the
  Terminal's working directory, and whatever's open in the Browser, all
  together. This works for human-to-human handoff as well as human-to-AI
  handoff.

*What these all have in common: the shared unit is not just "what was used,"
but "how far the computation got."*

---

## Design principle: Everything can be a Capsule

Rather than building a separate sharing model for each kind of target, Ato
projects whatever elements are involved onto one common Capsule interface.

| What the Capsule contains | Operations available |
| --- | --- |
| Data | Pass / Open |
| Data + application | Open |
| Data + application + execution state + history | Replay / Continue |

*Everything can be a Capsule* doesn't mean "treat everything as the same
kind of data." It's a design principle for handling computing targets of
different natures through the same common operations: save, hand over,
resume, and compose.

---

## Core model

### Capsule / Run / Replay / Continue

- **Capsule**: an immutable, addressable value carved out of a computation,
  from which you can continue starting at a given point. Multiple Runs can
  branch off into different futures from a single Capsule.
- **Run**: a mutable, currently-in-progress computation state resumed from a
  Capsule. Advancing and saving a Run produces a new Capsule.
- **Replay**: uses saved Records to replay and apply the interactions that
  led up to a point — an operation for checking or reconstructing "how did
  we get here."
- **Continue**: re-realizes a Capsule's point and starts a new Run — the
  operation for "keep going from here."

```text
C41 ─────▶ C42 ─────▶ C43
            │
           seal
            ▼
       Capsule C42
          /     \
         ▼       ▼
       Run A    Run B
         │       │
        C43a    C43b
```

### Core / Kernel and Adapter

Ato separates a computation's logical meaning from how it's actually
executed in the real world.

| Element | Role | What it handles |
| --- | --- | --- |
| **Core / Kernel** | defines a computation's identity, interaction, and evolution | Computation, Port, Protocol, Evolution, Composition |
| **Adapter** | connects the logical computation to real-world I/O | Process, PTY, Workspace, HTTP, Binding, etc. |

Core handles what a computation *is*, how it changes, and how it composes.
Adapters connect that to the concrete world — an OS process, a terminal, a
filesystem, HTTP — and observe and apply interactions as Records.

```text
               Computation
                    │
                Protocol
                    │
          ┌─────────┼─────────┐
          ▼         ▼         ▼
       Process     PTY     Workspace
       Adapter   Adapter     Adapter
          │         │         │
          └─────────┼─────────┘
                    ▼
                Physical world
```

This separation means the Web, Terminal, AI agents, or future new runtimes
can all be added without changing what a Capsule itself means.

---

## Materialization

Ato keeps a clean separation between "which computation point was saved" and
"how you physically get back to that point." The former is the **Capsule**;
the latter is the **Materialization**.

```text
                    Capsule C42
                         │
          ┌──────────────┼──────────────┐
          ▼              ▼              ▼
       Replay       Filesystem     VM checkpoint
          │              │              │
          └──────────────┼──────────────┘
                         ▼
                       ≈ C42
```

The same Capsule can, in principle, be re-realized (restored) through
different means — Replay, filesystem reconstruction, process checkpoints, VM
snapshots. By not treating the snapshot or container itself as the Capsule's
identity, the physical execution method stays interchangeable.

---

## The computation theory behind it

Ato's model is influenced by existing computation theory and systems
research. The goal isn't to invent these ideas anew, but to recompose them
into a systems model for saving, handing over, and continuing computation.

- **λ-calculus / Continuation**: thinking not in terms of past processing,
  but of "what remains from the current point" (residual computation /
  continuation).
- **π-calculus**: treating computation not as a closed process but as one
  that interacts with other computations and the outside world through
  Ports, and composing multiple Computations together.
- **Kell calculus**: giving a computation an explicit boundary, and
  passivating (suspending and extracting) a bounded process while it runs —
  what Ato's Capture / Seal corresponds to.
- **Reversible process calculi**: treating history as a causal relationship
  rather than a single linear path, as a foundation for safely doing Replay,
  Rewind, and Fork.
- **Distributed snapshots**: a foundation for capturing a Computation made
  up of multiple processes or hosts as one consistent point, including
  internal communication and in-flight messages.

```text
Computation ──▶ capture / seal ──▶ Capsule ──▶ transfer ──▶ another runtime ──▶ materialize ──▶ Run ──▶ Continue
```

---

## Basic lifecycle

```sh
# Create a new lineage and start recording.
ato init demo

# Save the current point.
ato stop demo

# Resume from a saved point.
ato resume demo@main

# Create a new branch from a past point.
ato resume demo@main#42 --branch experiment

# Export one point as a portable Capsule.
ato encap demo@main \
  --materialize ato.replay@1 \
  -o demo.capsule

# Run a portable Capsule.
ato run demo.capsule
```

### `capsule.toml` example

Current authoring makes the processes to start and the Adapters to use
explicit.

```toml
schema = 1

[[process]]
id = "app"
command = ["python", "app.py"]
cwd = "."

[[adapter]]
target = "app"
use = "ato.process@1"

[[adapter]]
target = "workspace"
use = "ato.workspace@1"

[encap]
materializers = ["ato.replay@1"]
```

---

## Project scope

**What Ato handles**

- Computation / Capsule / Run identity
- Capsule lineage and branches
- Interaction via Port / Protocol
- Composition of Computations
- Record / Replay via Adapters
- Portable encoding of a Capsule
- Re-realization via Materialization

**What Ato does not aim to be**

- A general-purpose environment-provisioning system replacing Docker / Nix /
  VMs
- A full reimplementation of package managers / toolchain provisioning
- A replacement for Git or a container registry
- Fully deterministic Replay for every possible process
- A universal sandbox / orchestration system

*Docker, Nix, VMs, existing runtimes, and AI agents are used to prepare the
computing environment; Ato's layer is what makes the computation that runs
on top of it recordable, savable, branchable, transferable, and
continuable.*

---

## Current implementation status

**Implemented**

- Immutable `ComputationObject`s and content-addressed `ComputationRef`s
- Computation evolution via Port / Protocol
- Computation composition
- Capsule lineage / branch / Run / Record
- CLI commands (`init`, `stop`, `resume`, `encap`, `run`)
- Various Adapters (Process, PTY, Workspace, HTTP, Binding)
- Portable `.capsule` bundle v2
- Protocol-generic Replay Materializer

**Experimental / in development**

- Physical restore of filesystem/workspace snapshots
- Heterogeneous Materializations, including process checkpoints and VM
  snapshots
- General-purpose Resume across different hosts
- Distributed Capture across multiple hosts
- Contract-equivalent realization across different Materializations

Ato is currently an experimental project. Not every part of the model is
complete; the current focus is on validating, step by step, whether
heterogeneous Computations can be handled through one unified lifecycle.
