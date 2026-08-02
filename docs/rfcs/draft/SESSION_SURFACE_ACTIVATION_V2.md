---
title: "Session Surface Activation v2"
status: draft
date: "2026-08-01"
author: "@koh0920"
ssot:
  - "crates/cli/src/application/runner_agent.rs"
  - "crates/cli/src/application/ready_state/restore_lease.rs"
related:
  - "docs/rfcs/draft/SESSION_SURFACE_CONTRACT.md"
---

# Session Surface Activation v2

## 1. Decision

`Session Surface Contract v1` continues to define the immutable surface
descriptor and rotatable access envelope. Activation v2 is an additive runtime
contract that defines when a public surface may become usable. It does not bump
`surface_contract_version` and does not reinterpret a v1 descriptor.

The control-plane invariant is:

```text
runner-local readiness
        +
validated public ready URL
        +
control-plane reachability probe
        +
atomic proxy activation
        =
run ready
```

Runner-local readiness alone is `surface_starting`, never public `ready`.

## 2. Negotiation and wire

A runner that implements this contract advertises the heartbeat capability:

```json
{ "capabilities": ["surface-activation-v2"] }
```

The control plane dispatches an Activation v2 lease only to a runner that
advertised that exact capability. The lease command carries:

```json
{
  "surface_contract_version": "1",
  "surface_activation_version": "2",
  "session_surface": {},
  "healthcheck_url_path": "/health"
}
```

Missing `surface_activation_version` means the legacy activation behavior.
Unknown or malformed explicit versions fail closed. During rollout, Public
Preview is the first lane that requires v2; other lanes remain unchanged.

## 3. State and activation

The minimal persisted state uses existing layers:

- `runs.status=starting`: runtime or surface activation is still in progress.
- `app_proxy_bindings.state=pending`: the stable public host exists but cannot
  route traffic yet.
- `runner_leases.status=ready`, `runs.status=ready`, and
  `app_proxy_bindings.state=active`: activation completed atomically.

No separate runtime/surface status columns are required for this rollout.
`pending` hosts return typed HTTP 503 with `Retry-After` and `Cache-Control:
no-store`; they never return 410 or proxy to the placeholder upstream.

## 4. Ready acknowledgement

For an Activation v2 lease:

1. `POST /status` with `status=ready` is rejected. Ready is accepted only by
   `POST /ready`.
2. Web and Pixel surfaces require `ready_url`.
3. The URL must pass the existing registered-runner/managed-ingress allowlist.
4. The control plane performs a bounded GET of `healthcheck_url_path` through
   the reported public origin. Redirects are not followed and only a successful
   response activates the surface.
5. Probe failure returns a typed retryable response and leaves lease/run/binding
   unchanged (`running`/`starting`/`pending`).
6. Probe success commits lease ready, binding upstream + active, run ready,
   usage start, and run-count event in one transactional D1 batch. Conditional
   writes prevent a concurrent Stop from resurrecting the binding or run.

The runner retries transient transport, 429, and 5xx ready-ack failures with a
bounded backoff until the preview horizon expires or the lease control endpoint
reports Stop/Done. Terminal 4xx responses are not retried. It never falls back
to `/status ready`.

## 5. Proxy failure semantics

For every App Proxy access mode, an upstream dial rejection is a typed 502 and
a time-to-first-byte deadline is a typed 504. Both responses are no-store and
carry the App Proxy marker. Once response headers arrive, ordinary Web response
bodies and WebSockets remain unbounded/streaming as before.

## 6. Rollout and acceptance

1. Deploy API support with the Public Preview v2 switch off.
2. Deploy runners that advertise `surface-activation-v2` and retry `/ready`.
3. Enable the switch in staging; selection fails closed when no v2 runner is
   available.
4. Verify polling timeout, `starting` presentation, and Stop-vs-ready races in
   the browser path. PWA production code changes only if those checks fail.
5. Enable production after staging probe and Stop-race evidence are green.

Acceptance tests cover missing URL, invalid URL, failed probe, successful atomic
activation, pending-host 503, Stop racing ready, proxy 502/504, runner retry and
terminal refusal, and the absence of an iframe before canonical run ready.
