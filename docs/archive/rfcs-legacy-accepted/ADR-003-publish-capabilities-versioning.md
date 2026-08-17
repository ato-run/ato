---
title: "ADR-003: Version the Publish Capabilities Contract"
status: proposed
date: 2026-03-31
author: "@egamikohsuke"
related: []
---

# ADR 000003: Version the Publish Capabilities Contract

## Context

`GET /v1/publish/capabilities` now affects product behavior directly:

- managed Store advertises `presigned` as the default upload strategy
- ato-cli consumes this response for runtime strategy selection
- rollback/debug behavior still depends on explicit direct override

Because capability discovery now changes publish transport choice, future payload changes must be versioned so CLI and Store can evolve independently without silent behavior drift.

## Decision

In the next follow-up change, add an explicit version field to the publish capabilities payload.

Candidate shape:

```json
{
  "capabilities_version": 1,
  "registry_kind": "managed_store",
  "default_upload_strategy": "presigned"
}
```

## Consequences

- CLI can reject or ignore unsupported capability versions safely
- Store can evolve discovery fields without relying on ad hoc field presence
- rollout of future strategy flags becomes observable and reviewable

## Follow-up

1. add `capabilities_version` to Store discovery payload
2. teach ato-cli to require a supported version before honoring new fields
3. document compatibility and fallback rules in CLI + Store specs
