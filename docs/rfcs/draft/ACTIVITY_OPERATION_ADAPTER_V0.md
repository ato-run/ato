# Activity Operation Adapter v0

Status: Draft

## Decision

Actor, Controller, and Activity are ato.run product-runtime projections. They
are not new Semantic Core nouns. An ato.run Activity realizes its top-level
graph as composed Runs, while every accepted application operation continues
an existing target Run through its registered Protocol and Adapter.

`OperationDescriptorV1` is the actor-scoped product projection of an operation
offered by a surface. A Browser executor first publishes the surface-scoped
subset (`SurfaceOperationDescriptorV1`); the Activity control plane adds
`activity_id`, `actor_id`, `actor_run_id`, and `target_run_id` only after it has
authorized the Controller binding. WebMCP is one physical producer of Browser
operations. It is not a Core protocol enum and its draft consumer API does not
escape the Browser compatibility bridge.

The existing `ato.browser@1` ingress remains authoritative for Browser
application operations:

```text
Controller request
-> Activity Room authorization and Actor single-flight
-> connected realization worker
-> ato.browser@1 live operation / physical ACK
-> RunEvolutionAuthority commit
-> Runner-issued run_sequence
-> Activity operation receipt
```

Neither the Activity Room nor an MCP client assigns the target Run sequence.
`actor_participant_id` remains optional compatibility provenance; `actor_id`
and `actor_run_id` are the new product authority.

## Surface and operation projection

The actor-scoped descriptor fields are:

- `id`
- `activity_id`
- `actor_id`
- `actor_run_id`
- `target_run_id`
- `surface_id`
- `surface_epoch`
- `protocol_id`
- `operation_name`
- `safe_description`
- `input_schema`
- `source`
- `origin`
- `read_only`
- `discovered_at`

The Browser Adapter always offers the fixed compatibility operations
`browser_pointer`, `browser_keyboard`, `browser_click`, and
`browser_scroll_to` with source `browser` and protocol `ato.browser@1`.
Page-offered tools have source `webmcp` and are exposed through
`list_operations`; they never become dynamically registered Activity MCP
tools.

`surface_epoch` changes when a prior descriptor id may no longer identify the
same live handler: top-level document replacement, Browser context
replacement, or a material tool-registry replacement. Poll timestamps alone
do not change the epoch or surface revision. Native `listTools`/`getTools`
polling compares normalized definitions rather than newly allocated invoke
closures, so consecutive identical snapshots remain stable. Invocation checks
both `descriptor_id` and `surface_epoch`; a same-named operation from a newer
epoch is never selected automatically.

## Activity Room wire extension

These messages extend `ato.activity.room@1` without changing existing
participant input or media messages.

Runner to Room:

```text
surface.observe {
  surface_id, target_run_id, surface_epoch,
  origin, producer_api, untrusted_content, observed_at
}

surface.operations.replace {
  surface_id, target_run_id, surface_epoch,
  operations: [{
    id, protocol_id, operation_name, safe_description,
    input_schema, source, origin, read_only, discovered_at
  }]
}

run.operation.receipt {
  operation_id, actor_id?, actor_run_id?, controller_session_id?,
  controller_epoch?, target_run_id, surface_id?, surface_epoch?,
  client_sequence, result, run_sequence?, record_ref?, applied_at?
}

run.operation.abort.receipt {
  operation_id, actor_id, actor_run_id, controller_session_id,
  controller_epoch, target_run_id, surface_id, surface_epoch,
  status, best_effort_result, requested_at
}
```

Room to Runner:

```text
run.operation.invoke {
  operation_id, descriptor_id,
  actor_id, actor_run_id, controller_session_id, controller_epoch,
  target_run_id, surface_id, surface_epoch,
  protocol_id, operation_name, arguments, client_sequence,
  actor_participant_id?
}

run.operation.abort {
  operation_id, descriptor_id,
  actor_id, actor_run_id, controller_session_id, controller_epoch,
  target_run_id, surface_id, surface_epoch
}
```

`operation_id` identifies the invocation and its retry/evidence. `descriptor_id`
identifies the current offered operation. The Runner deduplicates accepted
invocations by `operation_id`; the Room rejects stale descriptors before
dispatch, and the Runner checks again against its current surface projection.

The existing Activity executor lease stays a separate command:

```json
{
  "kind": "activity_browser_executor_v0",
  "activity_id": "...",
  "activity_run_id": "..."
}
```

`activity_run_id` is the Shared hosted Browser Activity Run and must equal the
lease Run id. The worker exchanges its Runner credential at the existing
executor-session endpoint for a short-lived Room URL and executor credential.
Room credentials are not persisted in `runner_leases.command_json`.

## WebMCP compatibility boundary

The Browser Host injects two distinct scripts:

1. A credential-free consumer compatibility script in the page main world.
   It probes `document.modelContext`, the deprecated compatibility alias, and
   the deterministic fixture producer, in that order.
2. The existing credential-bearing `ato.browser@1` bridge in an isolated
   world. It cannot directly depend on main-world expando visibility and uses
   a narrow DOM-event request/ack bridge.

Only the main-world script knows draft WebMCP producer method names. It holds
one `AbortController` per in-flight page operation. Page-provided operation
output is discarded at that boundary; a Controller must observe the resulting
surface state. No Ato credential is installed in the main-world consumer.

The Browser Adapter treats names, descriptions, schemas, origins, observations,
and outputs as untrusted. It:

- admits only bounded identifier-shaped operation names;
- replaces page descriptions with an Ato-generated `safe_description`;
- reduces schemas to bounded structural validation keywords and safe enum
  tokens, removing annotations and instruction-shaped strings;
- reduces origins to an HTTP(S) origin without credentials, paths, or query;
- omits raw output from operation receipts; and
- rejects an oversized raw snapshot before projection.

The Activity API repeats schema/origin sanitization as defense in depth and
regenerates `safe_description` instead of trusting the Runner value.

## Ordering, single-flight, and abort

The Activity control plane enforces one mutating operation in flight per Actor.
Read-only operations and mutations by another Actor are not fenced by that
Actor-level gate. Browser physical handlers from different Actors may overlap.
The Adapter's single ACK demultiplexer assigns a monotonic settlement ticket,
then the Browser ingress commits in ticket order from the current Run head.
This Browser-only path relies on `ato.browser@1` external input being
head-independent and derivable at every valid Browser frontier; generic Kernel
semantics retain apply-before-commit.

The physical dispatch carries an opaque realization generation made from the
main-world document token and WebMCP registry generation. It is not part of the
semantic Browser event or Record. The main-world compatibility boundary checks
it immediately before every fixed Browser or WebMCP operation, so navigation
and registry replacement invalidate all prior descriptors even before the
worker's next projection poll. A missing ACK or disconnect after ordered
dispatch makes the physical outcome indeterminate and terminally fences that
Adapter incarnation; it never abandons one ticket and issues a later ticket.

## Lease-lifetime retry evidence

Before physical Controller dispatch, the worker atomically persists and
file/directory-syncs a `started` intent under the lease root. The filename is a
digest rather than a caller-provided operation id. The entry contains only the
canonical request digest and bounded provenance: it excludes credentials,
arguments, page output, and raw WebMCP metadata. After settlement it is
atomically replaced with the sanitized terminal receipt. The journal root and
files are owner-only, and normal lease cleanup removes the journal.

An identical Room redispatch joins the live owner or exactly replays the
terminal receipt, including its original result and Runner sequence. A
different digest fails closed. A `started` intent found after worker restart is
reported as `operation_indeterminate` and is never physically replayed. This
provides no-double-apply behavior through Room restart and the Runner lease
lifetime. It is deliberately not an exactly-once claim: without a producer
transaction or producer idempotency there is no way to distinguish a process
crash immediately before a page effect from one immediately after that effect.

Head persistence failure after physical ACK is non-terminal. The same
operation repairs the pending head/Record without reapplying the page effect;
later settlement tickets remain fenced behind it. The controller page keeps a
bounded in-memory receipt outbox, reconnects to the Room, and retries ambiguous
loopback responses with the same operation id so a lost HTTP response cannot
overwrite an already-journaled acceptance with a synthetic failure.

Emergency stop records `abort_requested` first. If the matching WebMCP
operation is active, the worker attempts to signal its main-world
`AbortController`. Abort receipts distinguish `abort_signal_delivered`,
`settle_only_abort_unavailable`, and `not_in_flight_or_queued`. A cooperative
abort which fails the physical apply returns `operation_aborted` without
inventing a Run sequence. An operation already acknowledged by the Adapter
settles with its Runner-issued sequence as `applied_after_abort_requested`.
Future mutations remain the control plane's responsibility to freeze when the
Actor is paused, including when a producer cannot abort.

Take over is different: it fences the prior Controller's new mutations and
normally lets its current mutation settle before rebinding the same Actor Run.

## Scope and limitations

v0 supports the connected hosted Browser realization and the deterministic
WebMCP fixture. It does not promise universal WebMCP draft compatibility, DOM
or accessibility operation discovery, or cross-target hard cancellation.
Those additions must preserve the same Protocol/Adapter boundary and
Runner-issued ordering.

The separate pure-stdio `ato-activity-mcp` is a Controller client for the
Activity API. It exposes a fixed tool vocabulary and reads a scoped connection
file; it does not become a WebMCP registry or extend `ato-desktop-mcp`.

## References

- `docs/rfcs/accepted/COMPUTATION_ARCHITECTURE.md`
- `docs/rfcs/accepted/COMPOSITION.md`
- `docs/rfcs/accepted/PROTOCOL_ADAPTER.md`
- `docs/rfcs/accepted/CAPSULE_CLI_LIFECYCLE.md`
- `docs/rfcs/draft/HOSTED_BROWSER_COMPUTATION_V1.md`
- `extensions/adapters/browser/src/operation.rs`
- `extensions/adapters/browser/bridge/webmcp-consumer-bridge.js`
- `extensions/adapters/browser/bridge/browser-bridge.js`
- `apps/connected-realization-worker/src/activity_controller.rs`
- `apps/connected-realization-worker/src/activity_controller.html`
