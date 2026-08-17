# ADR-010: Runtime Connectivity and Placement-Aware Clients

Status: Draft

## Context

Issue #382 defines the Ato 0.7.0 runtime roadmap. The issue is a parent roadmap issue, not a single implementation PR. The work must land as small PRs because it crosses ADRs, shared schemas, session persistence, local APIs, network connectivity, Desktop orchestration, and the dev Web Console surface.

The implementation baseline is `ato-run/ato` `origin/dev` HEAD. The MVP is DesktopRuntime first. ManagedRuntime and ExternalRuntime are represented by ADR language and minimal DTO placeholders only.

`ato-store-local` may be used as the dev/MVP Web Console surface, but it is not the production Ato Web Console.

## Decision

This issue does not introduce a separate Placement Control Plane service.

Ato 0.7.0 extends the existing `ato-net` direction into a Runtime Connectivity Layer:

- `ato-net` owns pairing, reachability, tunnel/serve, and authenticated transport.
- Runtime providers own materialize/build/launch/supervise/stop.
- Runtime Control API owns launch/stop/log/status semantics.
- PlacementGraph owns provider selection and placement identity.
- `ato-protocol` only contains stable DTOs shared across process/API boundaries.
- `ato-netd` must not gain launch/session orchestration verbs.

HTTP Runtime Control API routes may reuse `ato-protocol` DTOs where stable, but HTTP routes remain an API layer and must not force route-specific transport concerns into `ato-protocol`.

## Session Identity

Session records add placement-aware fields as optional additive metadata:

- `placement_provider`
- `placement_provider_id`
- `placement_id`
- `placement_fingerprint`
- `placement_facets`
- `user_visible_url`
- `requested_by_client`
- `runtime_owner`

The semantics are intentionally separated:

- `placement_provider`: `desktop`, `managed`, or `external`
- `requested_by_client`: `desktop_fe`, `web_console`, `cli`, or `automation`
- `runtime_owner`: `desktop_be`, `managed_runtime`, or `external_runner`

`placement_id` is an opaque stable id. `placement_fingerprint` is a debug/receipt hash. `placement_facets` carries non-sensitive classes such as provider kind, isolation class, storage class, network class, and runner version.

## PR Breakdown

1. ADR only.
2. `ato-protocol` placement and runtime-control event DTO stubs.
3. Additive session record fields in `ato-session-core`.
4. Local registry Runtime Control read APIs.
5. Local registry Runtime Control write APIs with token, Origin/Host, and CSRF guards.
6. `user_visible_url` and StartServe integration without adding orchestration verbs to `ato-netd`.
7. Runtime-aware `ato-store-local` dev/MVP UI.
8. DesktopRuntimeProvider wrapper that preserves the Desktop to CLI spawn boundary.

## Security Requirements

Local registry write APIs bind to loopback by default. Non-loopback bind requires explicit opt-in and an auth token. Runtime write APIs must reject tokenless POST requests. Browser-originated calls require Origin and Host validation plus CSRF protection before launch/stop endpoints ship.

`open-url` does not trigger server-side browser opening in the MVP. The API returns a `user_visible_url`, and the client renders it as a normal link.
