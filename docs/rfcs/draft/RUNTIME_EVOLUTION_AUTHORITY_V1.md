# Runtime Evolution Authority v1

## Decision

A hosted Run owns one logical `ComputationRef` head independently of its VM,
Browser profile, Record frontier, and observations. `RunEvolutionAuthority` is
the reusable runtime boundary: it owns a `Kernel`, `Run { head }`, a monotonic
operation sequence, and one serialized acceptance gate.

The authority accepts only inputs that the target's registered `Semantics` and
protocol registry can derive. It deliberately knows neither Browser, Activity,
Product, nor Record operation meanings. `ComposeSemantics` remains the way a
registered composition derives a child transition and seals its parent
successor.

## External operation ordering

For an external input alpha, the authority uses two phases:

1. `Kernel::derive_transition(C0, alpha)` seals `C1` without publishing it.
2. The caller's Actuator applies alpha.
3. Only on success does `commit_derived_transition` publish the transition and
   change `Run.head` to `C1`.

An Actuator failure leaves the visible head at `C0`; the sealed successor is
unreachable ordinary object-store garbage. `Kernel::step` retains its existing
in-process convenience behavior, but hosted external paths must use the
authority rather than call `step` before physical application.

After physical success, the Runner persists `(operation_id, run_seq,
head_before, head_after)` with a lease-scoped API CAS. If that persistence
fails, the authority does not pretend to undo the physical operation. It holds
the same transition as pending and fail-closes later accepts until that exact
CAS can be retried.

## Record boundary

The accepted semantic input and a Record are related but not identical. The
authority gives the caller a serialized point to submit a Record candidate,
after the operation is accepted. A Record write failure is reported alongside
the accepted transition and never rewinds the Computation head. Record stream,
local sequence, time, actor, and transport metadata are not inputs to
Computation identity.

## Existing CLI evolution

The CLI's `RepositoryObservationSink` currently owns a direct evolution path:
`live_head` / `ActiveRun.head` are advanced by `evolve_observation()`. For
`ato.authoring@1`, that implementation updates the authoring semantic frontier
from canonical semantic fields (adapter, protocol, port, direction, and payload
reference) and intentionally excludes Record sequence/timestamp provenance.
It also recurses through composition. The Supervisor disables this observation
path when `ato.replay@2` records operations.

Kernel does not currently register an `ato.authoring@1` `Semantics`
implementation. Copying `evolve_observation()` into Connected Worker would
therefore introduce a second identity rule and risks changing existing Capsule
references. This RFC makes no such change. Hosted authority is initialized at
the assigned lease root but cannot process an operation until its Semantics are
explicitly registered. A later migration may extract shared successor
derivation only with an equivalence test against existing authoring references.

## Capture boundary

`freeze()` serializes behind any in-flight acceptance and returns the resulting
`(head, run_seq)` frontier; `unfreeze()` resumes acceptance. Capture will later
freeze, drain, read that logical head, seal the Record frontier, and materialize
VM/Browser state. It must never compute a new `ComputationRef` from Record
history, VM bytes, Browser state, or observations.

## Control-plane projection

`parent_root` remains the immutable Capsule root used to start a Run.
`current_computation_ref`, `current_run_seq`, and
`current_head_updated_at` are separate live projections. The assigned Runner
alone advances them through a CAS; the API validates assignment, prior head,
sequence, and an immediate exact retry, but does not execute Semantics.

## Non-goals

This change adds no Browser Semantics, Browser Materialization, capture
advertisement, Save flow, Invite flow, CAS object transport change, or staging
deployment. In particular, a 2048 keyboard operation does not yet claim to
advance the hosted head.
