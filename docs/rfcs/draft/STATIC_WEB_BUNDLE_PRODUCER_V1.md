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
- Browser Runner bootstrap injection;
- deployment, route, or mutable delivery configuration;
- source builds or framework-specific behavior.
