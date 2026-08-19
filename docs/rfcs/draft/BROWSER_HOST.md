# Browser Host

Status: Draft

## Context

`ato.browser@1` owns a logical Browser Protocol boundary, but it must not own a
particular browser executable, Chrome DevTools Protocol (CDP), VNC/RFB server,
or delivery surface.  Those are physical Runner concerns.  The original
Browser Adapter acceptance driver supplied that physical concern manually with
raw CDP.  A staging Run needs the same concern as a reusable host component,
without putting Chrome control data into a Computation, Record, or Capsule.

## Decision

The Ato runtime provides an internal **Browser Host** command.  It is runtime
orchestration, not a new semantic object or a new Protocol.

```text
Browser Adapter attach
  -> host-private Browser runtime discovery
  -> Browser Host
  -> Chrome CDP isolated world
  -> generic browser-bridge.js
  -> application page
```

The Browser Host:

- reads exactly one live Browser Adapter discovery document from an absolute,
  host-private runtime directory;
- verifies that its target URL has the discovery document's exact origin;
- launches a disposable Chrome profile on loopback CDP;
- injects the unmodified generic Bridge in a named Chrome isolated world;
- passes the bootstrap only in that isolated world;
- waits for the discovery document to disappear, then disposes Chrome and its
  profile; and
- reports lifecycle facts without logging the channel credential, browser
  session, control URL, or injected source.

It does **not** decode Browser Records, schedule replay, decide a Capsule
identity, interpret application state, or know application names.  It is
therefore replaceable by a Chrome extension, a Wry host, or a remote-display
host without changing `ato.browser@1` or `ato.replay@1`.

The normal host command is internal (`ato __browser-host`).  Its inputs are
runtime-only: the host-private directory, browser executable, and target URL.
They are not authoring fields and must not appear in `capsule.toml`, bundles,
PWA URLs, application storage, or application process environments.

## Delivery boundary

For a hosted, human-controlled Chrome, the Browser Host is expected to run
behind the existing authenticated Pixel Stream/RFB surface.  That surface is a
presentation transport: it forwards human pointer/keyboard input to the
host-owned Chrome, which then emits trusted browser events to the Bridge.  The
PWA, API, Semantic Core, and Replay Materializer do not receive CDP authority
or Browser Adapter credentials.

The first implementation supports a local Chrome process and is intentionally
independent of RFB provisioning.  Connecting that process to the selected
Pixel Stream session is an explicit Runner delivery integration, not a
Browser Adapter behavior.  Until that integration is installed in staging,
the Browser Host may be used for host-local acceptance only; this must be
reported as `Public staging delivery integration: PARTIAL`, not as a product
delivery pass.

## Security requirements

- Browser runtime discovery must be in an absolute, owner-only directory.
- CDP listens only on loopback and has a disposable profile.
- Bootstrap values are read from host-private discovery and are sent only to
  Chrome's named isolated world.
- The target page's main world must not expose `__ATO_BROWSER_BOOTSTRAP__` or
  any other Browser control value.
- The host must remove its profile on normal discovery cleanup and on every
  host error it can clean up locally.

## Operational contract

Browser Host starts only after a Browser Adapter has published discovery.  It
does not make a Run active and does not activate an Adapter; those continue to
be ordinary Supervisor/Realization lifecycle operations.  If the Bridge fails
to connect, the Adapter's existing deadline and fail-closed behavior remain
authoritative.  If the Host exits, the Bridge disconnect is observed by the
Adapter and Replay cannot claim successful restoration.

## Non-goals

This RFC does not add a Browser-specific Materializer, application state
capture, VNC implementation, PWA API changes, text capture, browser
checkpointing, multi-tab/frame semantics, or ComfyUI semantics.
