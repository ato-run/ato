# Browser Adapter

Status: Draft

## Scope

`ato.browser@1` treats a human's top-level browser input as an ordinary Ato
Protocol interaction. It records physical keyboard, pointer, click, and scroll
input, and applies one previously recorded input at a time to a live browser.
It is application-independent: application state, DOM meaning, storage,
network traffic, and workload-specific determinism are outside this Adapter.

The Adapter uses the existing `AdapterFactory -> AttachedAdapter` lifecycle and
the existing protocol-generic `ato.replay@1` Materializer. Kernel and Replay do
not branch on Browser events. There is no Browser-specific Player, timeline,
artifact, Materializer, Computation, or Capsule.

## Responsibility boundary

- Ato Records own stream-local and causal order.
- `ato.replay@1` owns reconstruction from the descriptor anchor to its target.
- `ato.browser@1` validates, observes, applies, acknowledges, and quiesces one
  interaction frontier.
- `browser-bridge.js` captures and dispatches physical browser events only.
- A browser driver injects the Bridge and is replaceable physical tooling.

Playwright is used only by end-to-end tests. It is not part of the Protocol,
Capsule identity, Adapter semantics, or product runtime.

## Protocol events

The Protocol ID and Adapter ID are both `ato.browser@1`. Payloads use canonical
JCS encoding and reject unknown fields. Version 1 contains:

- keyboard `key_down` and `key_up`, represented by `code` and modifiers;
- pointer `pointer_down`, `pointer_up`, `pointer_cancel`, and `pointer_move`,
  represented by pointer identity/type, normalized viewport coordinates,
  button, and buttons;
- click with normalized viewport coordinates and button;
- scroll with absolute document coordinates.

Events contain no sequence or timestamp. Record order is authoritative and
wall-clock timing reproduction is not guaranteed. Double-click, long-press,
and other timing-sensitive semantics are future protocol work.

## Observe and apply

Trusted human input is emitted as `Direction::Inbound` and
`ObservationEffect::Evolution`. Bridge readiness, handshake, acknowledgements,
and viewport information are runtime evidence and produce no semantic Record.

`AttachedAdapter::apply(record)` verifies Adapter, Protocol, Port, canonical
payload, and privacy policy, sends one apply request, and returns only after the
Bridge acknowledges dispatch. Replay may then advance to the next Record. The
Adapter has no batch or timeline scheduler.

Synthetic replay events are not observed again. Browser network requests are a
separate `ato.http@1` boundary when that Adapter is also configured. Until Ato
can prove cross-Adapter causality, a Replay descriptor containing inbound
Evolution from both Browser and HTTP fails closed; replaying both could apply a
Browser-triggered network mutation twice. This is a generic Adapter replay
policy, not a Browser-specific Replay Materializer.

Bridge acknowledgement proves only that a canonical event was dispatched. It
does not prove the resulting application state. A replay Realization therefore
reports `AppliedUnverified` unless an independent Contract verifies the target;
fixture DOM or HTTP inspection remains test-harness responsibility.

After a portable Replay activates, new trusted input is recorded on a
`continued` branch rooted at the restored Computation. When the foreground
Run exits cleanly, Ato seals that branch, retains its workspace under the
recipient's private cache, and reports the new ComputationRef and workspace.
The recipient can therefore encap the new future instead of losing post-Replay
interaction with an ephemeral realization.

## Ordering and continuous input

Adjacent `pointer_move` and scroll events may be coalesced. The implementation
flushes all pending continuous events, in observation order, before every later
discrete event. Continuous events from different families are not reordered.
Quiesce also flushes the pending frontier.

## Security boundary

Every attach generates a cryptographically random channel credential and
browser-session identifier. In hosted operation `ATO_BROWSER_RUNTIME_DIR`
selects an absolute, host-private Runner directory outside the application
workspace. Local development may fall back to `.capsule/runs`. The loopback
control endpoint, credential, and session are written as owner-readable live
Run discovery state and removed on detach. They are never stored in
`capsule.toml`, a Computation, Record, object bundle, or portable Capsule, and
the application process receives no inherited host environment.

The WebSocket upgrade validates the exact configured top-level origin. The
first Bridge message must also match Protocol version, channel credential,
expected origin, and browser session. A mismatched origin, channel, session, or
protocol is rejected. Version 1 accepts one top-level frame and one live Bridge
per Adapter attach.

## Privacy

By default keyboard capture permits navigation and control codes only: arrows,
Enter, Escape, Tab, Space, Home, End, PageUp, PageDown, Insert, Delete, and
Backspace. Additional non-text codes require explicit Adapter configuration.
Payloads persist `code` and modifiers, never `KeyboardEvent.key` or field
values.

The Adapter and Bridge do not capture text values, password values, clipboard,
cookies, authentication tokens, localStorage, sessionStorage, or IndexedDB.
Text input requires a future consent, redaction, and field-classification
design.

## Quiesce

Quiesce sends a barrier to the Bridge. The Bridge stops accepting new human
input and acknowledges only after all earlier event messages have been sent.
WebSocket order lets the Adapter process those messages first; it then flushes
continuous events through the synchronous Observation sink and waits for any
pending apply acknowledgement before returning. `ato stop` seals only after
this succeeds.

## Configuration and injection

The normal Adapter registry path lowers generic Adapter configuration. A
logical Port contains only stable Protocol identity. Expected origin and input
policy are Adapter configuration; the generated endpoint, credential, browser
session, and process/socket identity are runtime-only.

The Bridge is a generic init script. Chrome tooling injects it through a named
CDP isolated world, keeping bootstrap credentials outside the application
JavaScript realm. Tests use this same physical boundary with a fresh browser
context. Future delivery may use an extension, Wry/WebView injection, or the
ato.run delivery pipeline without changing Protocol or Replay semantics.

The staging acceptance driver under `tests/browser/` uses raw Chrome DevTools
Protocol over a loopback WebSocket and Chrome's `Input` domain. It launches a
real Chrome process with an isolated, disposable profile and does not use
Playwright. This remains operator/test tooling: Chrome/CDP is not linked into
the Adapter, Protocol, Materializer, Capsule identity, or product runtime.

The Bridge lifecycle is `restoring -> active -> quiescing -> stopped`. Trusted
input is captured and suppressed before it reaches application listeners while
the lifecycle is not active. Apply and lifecycle requests have transport-owned
deadlines; shutdown remains actionable while an acknowledgement is pending,
closes the socket, joins the transport thread, and removes discovery state.
The Adapter quiesce deadline is strictly shorter than the Supervisor stop
deadline, so a failed barrier cannot be mistaken for a successful seal.

## Known limitations and deferred work

Version 1 deliberately excludes text input, touch, multi-tab and multi-frame
semantics, local/session storage, IndexedDB, cookies, authentication sessions,
clipboard, browser checkpoints, DOM semantic diffs, selector healing,
AI-assisted repair, wall-clock deterministic replay, OS-window input, Pixel
Adapters, viewport adaptation, native/default-action equivalence for every DOM
event profile, and application-specific semantic/state Adapters. Replay
currently requires a compatible viewport. Scroll capture is limited to the
top-level document frontier. ComfyUI support is not part of this RFC's
implementation change.
