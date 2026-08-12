---
title: "Connector Driver stdio JSON-RPC Binding v1"
status: draft
date: 2026-08-12
author: "@egamikohsuke"
related:
  - "CONNECTOR_DRIVER_V1.md"
---

# Connector Driver stdio JSON-RPC Binding v1

## 1. Framing

The binding carries JSON-RPC 2.0 over child-process stdin/stdout. Each message
is framed as a four-byte unsigned big-endian length followed by exactly that
many UTF-8 JSON bytes. The v1 maximum control frame is 8 MiB. Zero-length,
oversized, invalid UTF-8, invalid JSON, duplicate request IDs, unmatched
responses, and EOF inside a frame are protocol errors. Stdout is reserved for
framed protocol data and stderr is diagnostic-only.

Responses correlate by request ID and may arrive out of request order.
Notifications from one Driver are ordered by their complete frame order.
`initialize` is the first request and negotiates the binding and Driver
contract versions before other operations.

## 2. Object exchange

Large payloads use capability-scoped object handles instead of exposing the
Supervisor object-store path. The binding defines `object.put_begin`,
`object.put_chunk`, `object.put_commit`, `object.open`, `object.read`, and
`object.close`.

Handles are bound to the logical Session, Driver instance, Supervisor
generation, and incarnation nonce. A handle from another Driver or stale
incarnation is rejected. Object content is streamed and verified against its
declared digest; the backing filesystem layout is not an ABI.

