# Computation Architecture

Status: Accepted

## Thesis

Ato does not execute manifests, build plans, or initialization workflows. Ato
advances addressable computations.

The only canonical semantic value is:

```text
ComputationObject { semantics, boundary, residual }
```

Its canonical JCS encoding is hashed with BLAKE3 to derive a
`ComputationRef`. Existing Computation Object v1 golden vectors and hash domain
remain unchanged. A Capsule is exactly a sealed/addressable computation:

```text
Capsule = ComputationRef
Run     = mutable cursor { head: ComputationRef }
```

There is no second `Capsule` semantic root.

## Evolution and observation

```text
C --action--> C'
C models observer K
composeW(C1, ..., Cn) -> C
```

The kernel resolves `Run.head`, dispatches by `SemanticsId`, asks that
semantics for one successor, canonically persists it, and updates the cursor.
The kernel contains no repository, compose, browser, language-runtime,
snapshot, or Nacelle rules.

`C0 -> C1 -> C2` is history. The current computation is `C2`; past
transitions do not participate in its identity. A no-op transition sink is a
complete kernel configuration. There is no privileged `C_init` concept.

Goal and readiness are authoring/evaluator stopping concerns. A Contract is an
observation requirement on a realized/resumed computation. Neither is a core
primitive.

Kernel transition handling is deliberately two-phase. `derive_transition`
computes, seals, and validates a successor without publishing history.
`commit_transition` sends only the selected top-level transition to the
optional sink. `step` is derive + commit + `Run.head` update. Compose therefore
derives boundary-hidden child transitions and commits one parent transition
atomically; failed synchronization cannot leak partial child evidence.

## Boundaries and composition

A Port is a typed interaction crossing the selected computation boundary. The
same child action can become `Tau` when its endpoints are internal to a
composite. Names such as install, build, launch, shell, or navigation do not by
themselves create Ports.

The Kernel knows which Protocol governs an interaction, but never knows the
Protocol's payload model. `Action` carries `ProtocolPayload`, an opaque byte
carrier. The Port's `ProtocolId` dispatches to registered `ProtocolSemantics`,
which owns role compatibility, input/output payload validation, and future
behavioral or capability rules. Thus payloads are opaque in the Kernel and
typed by the Protocol; this is not an unvalidated universal bytes model.

Enabled transitions are `TransitionOffer { choice, action }`. A
semantics-owned opaque `ChoiceId` distinguishes two transitions even when both
visible actions are `Tau` or carry equal output payloads. Semantic choice is
never inferred from collection iteration order. External input may omit a
choice when its payload selects the transition.

`capsule.compose@1` is an ordinary semantics extension. Its child semantics
produce transition evidence; compose validates endpoints/value agreement and
reduces internal synchronization to `Tau`. The canonical Alice/Bob test
branches from the same computation and persists all seven distinct refs while
keeping `{name, greeting}` invariant.

## Classification

1. Future behavior belongs in semantics-specific residual identity.
2. Boundary-crossing interaction is a Port action.
3. Physically different but observationally equivalent realization belongs to a Provider.
4. Past events are optional evidence/trace.
5. Authoring notation is Adapter input and compiles away.

Runtime versions, environment values, source and filesystem topology are in
the residual when they change future behavior. Plaintext secret values never
are: residuals contain safe binding identities, while providers own values and
injection. Network allowlists are provider security contracts. Snapshots are
provider materializations.

There is no universal Value enum, workflow/InitSpec DSL, generic Build or
Launch graph, universal State, Connector, Record, or Capture Manager.
There is no `Kernel<V>`, `Semantics<V>`, `Action<V>`, or universal payload
enum. Concrete Protocol implementations own payload typing.
