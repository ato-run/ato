# Browser Protocol Adapter Extension v0

Status: Draft — Experimental extension contract

## Scope and stability

`ato.browser@1` is an experimental Protocol Adapter extension. It is not one
of the accepted built-in v1 Adapters and carries no compatibility promise
beyond the release slice explicitly validated by this RFC. Registration through
`ato-adapter-api` does not promote it to built-in status.

This contract covers the Public Browser Activity vertical slice:

```text
Public Activity URL
→ authorized Actor participation
→ Runner-authorized Browser operation
→ physical Browser Adapter apply / ACK
→ one committed successor Computation
→ one applied-operation Record
→ Activity receipt with Actor and Runner ordering evidence
```

It does not define a general-purpose browser automation platform, Browser state
Materialization, or a new Semantic Core primitive.

## Protocol role

The Protocol identifier is `ato.browser@1`.

- `server` is the application endpoint that receives Browser interaction.
- `controller` is the Browser computation endpoint that applies interaction.
- `browser`, `host`, `participant`, `Activity`, and `Actor` are not Protocol
  role aliases.

Actor, Controller session, Activity, Surface, and Runner sequence are
product/runtime provenance. They do not enter the canonical Browser event or
the current Computation identity.

## Authoritative ingress

Only a Runner-authorized `run.operation.invoke` may become authoritative input
for a hosted Public Activity. Before dispatch, the Runner validates the
Activity, Actor, Actor Run, target Run, Controller session and epoch, Surface
and Surface epoch, operation descriptor, credential scope, and client
sequence.

The experimental v0 operation vocabulary is:

- `keyboard`
- `pointer`
- `click`
- `scroll`
- `operation` for a bounded, normalized page-offered operation

An operation becomes an Evolution candidate only after validation. The
physical Browser Adapter applies the canonical event to the current Browser
realization and returns a physical settlement ticket. The Runner commits from
the current head in settlement-ticket order. Coordinator, Activity Room,
WebSocket, HTTP, or client arrival order is never execution order.

A successful physical apply followed by a persistence retry keeps the same
operation id, event, transition context, and settlement position. It must not
physically reapply the event.

Exactly one Record is submitted for an operation that reached the committed
`applied` state. A rejected, stale, unauthorized, timed-out-before-apply, or
otherwise unapplied operation produces typed failure evidence but no Browser
operation Record.

## Actor provenance

Every hosted invocation and terminal receipt carries:

- `operation_id`
- `activity_id`
- `actor_id`
- `actor_run_id`
- `controller_session_id`
- `controller_epoch`
- `target_run_id`
- `surface_id`
- `surface_epoch`
- `client_sequence`

Every applied receipt additionally carries the Runner-issued `run_sequence`
and the persisted Record reference. The Record or its immutable enclosing
evidence must be inspectably related to the same `operation_id`, `actor_id`,
`actor_run_id`, target Run, and Runner sequence without placing that provenance
inside the Browser semantic payload.

Reconnect creates or resumes an explicitly authorized Controller binding. It
must not infer Actor identity from a socket, browser tab, participant arrival
order, or a previously connected Actor. A disconnected or fenced Actor cannot
submit new operations.

## Ordering and duplicate handling

The Runner is the sole ordering authority for the target Run.

1. The Activity control plane authenticates and scopes an invocation.
2. The Browser Adapter's single ACK demultiplexer assigns a monotonically
   increasing physical settlement ticket.
3. The Runner serializes commits by that ticket from the current head.
4. The Runner assigns `run_sequence` only to the committed operation.

Concurrent operations from different Actors may be physically in flight, but
their committed order is deterministic from the Runner settlement order.

`operation_id` is an idempotency key. Reuse with byte-for-byte equivalent
authorized input joins or replays the existing terminal result. Reuse with a
different Actor, scope, descriptor, epoch, sequence, or arguments fails
closed. A duplicate ACK for an already-settled request is tolerated as
duplicate transport evidence and cannot create a second settlement, head, or
Record. A late ACK cannot reopen a timed-out or fenced transport incarnation.

## Presentation and output

The following are evidence or projection by default and never authoritative
Browser operation ingress:

- screenshot
- rendered frame
- DOM or accessibility observation
- console output
- media
- diagnostics
- page-provided operation output

They do not advance the Computation head, allocate `run_sequence`, choose
operation order, or create a Browser operation Record. A later Protocol may
define an explicit typed interaction derived from an observation; that requires
its own validation and is not implied by presentation capture.

## Apply, verify, and replay capability

The extension may:

- observe trusted local events only in explicit local `ObserveAndApply` mode;
- apply a validated canonical Browser event to a live matching realization;
- verify a Record only to the extent supported by that live realization; and
- replay a persisted Browser operation only when the exact required Adapter
  operation and compatible physical Surface are available.

Hosted Public Activities use `ApplyOnly`. They do not observe actuator echoes
as a second ingress and do not create a second Record.

Actor credentials, cookies, Browser profiles, page output, screenshots, DOM,
and media are not replay payload. The extension makes no exactly-once claim
across an indeterminate physical crash; a durable started intent is terminally
reported as `operation_indeterminate` and is never automatically reapplied.

## Security boundary

The Controller command boundary uses a short-lived credential scoped to:

- Activity
- target Run
- Actor and Actor Run
- Controller session and epoch
- permitted operation class
- expiry
- monotonically increasing client sequence or one-time nonce

The isolated-world Browser bridge uses a separate short-lived channel
credential scoped to its target Run, Browser session, Surface incarnation, and
expiry. It is shared only by operations already authorized at the Controller
boundary; it does not infer Actor identity. The dispatched operation keeps its
Actor provenance outside the page-owned semantic payload.

Both boundaries fail closed for cross-Run, cross-Actor, expired, stale-epoch,
replayed, future/invalid-sequence, or malformed credentials/messages. Secret
values are never formatted into errors or diagnostics.

The page main world receives no Ato credential. Names, schemas, descriptions,
origins, DOM, output, and media from the page are untrusted. The isolated-world
bridge validates exact origin and physical Surface incarnation immediately
before apply.

## Secret and credential persistence

The following values must not enter CAS, Records, logs, Activity archives,
operation journals, snapshots, or presentation receipts:

- `.env` contents
- API keys
- OAuth tokens
- cookies and session secrets
- private keys
- credential-store contents
- resolved Binding values
- Controller and Browser bridge credentials

Records and receipts may contain only logical Binding identity, safe provider
reference identity, credential expiry/status evidence, and redacted failure
codes. Rotation invalidates the prior resolved value before a replacement is
reported usable.

## Unsupported behavior

Experimental v0 does not promise:

- universal WebMCP draft compatibility;
- DOM, accessibility, screenshot, console, or media as Evolution input;
- text/password entry through unrestricted keyboard codes;
- cookie, localStorage, profile, or DOM persistence as Capsule identity;
- automatic replay after an indeterminate physical outcome;
- hard cancellation across arbitrary page targets;
- a stable browser-native pointer actuation guarantee until #1311 is resolved;
- execution ordering from Activity Room or transport arrival order; or
- cross-platform conformance beyond environments with explicit receipts.

Unsupported behavior is rejected with a typed code. It must not silently
degrade to an unscoped synthetic operation.

## Detected implementation mismatches

The contract above is normative for this experimental release slice. Current
code is not evidence that these behaviors are already satisfied.

1. Browser bridge authentication currently proves protocol, random channel
   credential, Browser session, and origin, but does not yet carry explicit
   target Run, Surface epoch, or expiry claims.
2. Activity Controller inputs carry Actor and epoch provenance, but the applied
   receipt does not yet expose a persisted Record reference suitable for the
   release acceptance audit.
3. Duplicate operation dispatch is covered, but duplicate/late ACK behavior
   needs an explicit tolerant terminal-state test rather than treating every
   unknown ACK as a generic transport failure.
4. Browser-native versus DOM-synthetic pointer actuation remains unresolved in
   #1311 and must not be represented as a stable compatibility claim.
5. Cross-Run, cross-Actor, expiry, sequence, disconnect/reconnect, secret
   persistence, process-orphan, and presentation-non-evolution behavior still
   require vertical-slice negative tests.

## Release conformance

The extension is release-ready only when the Browser Activity release gate has
machine-readable receipts proving:

- host plus two participant Actors;
- deterministic Runner order and one final head;
- one Record per applied operation and no Record for rejected operations;
- matching Actor provenance and Record references;
- fail-closed stale/cross-scope/replayed input;
- no secret persistence;
- zero Run-owned orphan processes; and
- Join-to-Interactive p50/p95 and phase timing breakdown.

Manual browser confirmation is supporting evidence, never a PASS by itself.

## References

- `docs/rfcs/accepted/COMPUTATION_ARCHITECTURE.md`
- `docs/rfcs/accepted/PROTOCOL_ADAPTER.md`
- `docs/rfcs/draft/ACTIVITY_OPERATION_ADAPTER_V0.md`
- `docs/rfcs/draft/HOSTED_BROWSER_COMPUTATION_V1.md`
- `extensions/adapters/browser/`
- `extensions/semantics/browser/`
- `apps/connected-realization-worker/`
