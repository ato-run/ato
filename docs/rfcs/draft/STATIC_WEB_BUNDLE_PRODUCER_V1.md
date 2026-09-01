# Static Web Bundle Producer v1

Status: Draft

## Context

PR #1227 introduced deterministic Static Web Bundle production, but the
capability disappeared when snapshot implementation code was isolated from
the current workspace architecture. Static delivery remains an immutable
Materialization concern; it is not Capsule identity and ato-edge is not an
execution authority.

## Current boundary

`ato-materializer-static-web` owns the reusable physical contract and pure
producer:

```text
built static directory
  -> validated output closure
  -> canonical manifest + content-addressed blobs + receipt
```

`tools/snapshot-builder` owns only CLI orchestration. The producer does not
depend on the snapshot provider, old capsulefs, deployment state, R2, D1, or a
Browser Runner implementation.

## Instrumentation lanes

Extraction may instrument the MATERIALIZED COPY only, never `image_root`.
`StaticWebInstrumentation` selects each lane explicitly; the default instruments
nothing and reproduces the built bytes exactly. Instrumented bytes are
content-addressed like any other file, so selecting a lane changes the
manifest digest and therefore the host label — an instrumented bundle is a
different physical artifact, not a mutated one.

| Lane | Protocol | Reserved path |
|---|---|---|
| Operation | `ato.browser@1` | `__ato/browser-runner-bridge-v0.1.2.js` |
| State | `ato.materialize.browser@1` | `__ato/instance-state-bridge-v1.js` |

The two lanes are not interchangeable. An operation Record is evidence of how a
computation was operated; instance state is which data currently remains. A
`localStorage.setItem()` is therefore never emitted as an `ato.browser@1`
operation.

The State lane injects a parser-blocking classic script plus an empty state
document (`<script id="__ato_instance_state_v1" type="application/json">null`).
`null` is the artifact's placeholder: the bundle is immutable and shared, and
the SAME bytes are served on the public Static Web lane where no instance
exists. Only a delivery edge that has resolved an owning instance rewrites that
element's text content, and the bridge is inert — no hydration, no patching, no
network — whenever it is absent, `null`, or malformed. The State lane is
injected last so it precedes every other script: hydration must complete before
any application script observes `localStorage`.

Both lanes fail closed when the built output already occupies their reserved
path.

## Identity

Manifest canonical bytes and every file digest are derived only from the
validated built output and explicit output plan. The manifest digest derives
the immutable host label. Repeated production from identical inputs must be
byte-identical.

This physical artifact is a Materialization Value. It does not create or alter
a `ComputationRef`.

## Security

The producer rejects non-normalized paths, traversal, links, unsupported media
types, source maps, closure-limit violations, unsafe connect origins, and any
runtime secret canary present in a blob or manifest. Frame ancestors are fixed
by the v1 contract.

## Non-goals

- restoring the former snapshot crate topology;
- deployment, route, or mutable delivery configuration;
- source builds or framework-specific behavior.
