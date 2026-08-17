---
title: "Terminal Surface Profile v1"
status: draft
date: "2026-08-09"
author: "@koh0920"
ssot:
  - "lib/ipc/src/session_surface.rs"
  - "lib/ipc/src/terminal_surface.rs"
related:
  - "docs/rfcs/draft/SESSION_SURFACE_CONTRACT.md"
  - "docs/rfcs/draft/PIXEL_STREAM_PROFILE_V1.md"
---

# Terminal Surface Profile v1

## Scope

`ato.terminal-surface.v1` exposes the PTY of one capsule-declared interactive
workload. It never opens a runner-host shell and does not infer a shell command.
The v1 surface requires a signed-in principal, permits one viewer, and supports
PTY input/output, resize, reconnect to the same live session, and clean exit.

Clipboard access, file transfer, recording, terminal transcript persistence,
programmatic link opening, anonymous access, and multi-service selection are
outside v1. Existing capsule filesystem, network, device, secret, and quota
policies remain authoritative.

## Descriptor and access

The only valid transport is `terminal_websocket`. The browser connects with the
`ato.terminal.v1` WebSocket subprotocol. `connect_url` is an absolute token-free
`wss://` URL; `auth_exchange_url` is an absolute `https://` URL on the same host.

The fixed v1 capabilities are input and resize enabled; clipboard, file
transfer, and recording disabled; encoding `utf-8`.

## Wire protocol

- Client binary frames carry raw PTY input bytes.
- Server binary frames carry raw PTY output bytes.
- Client text frames are `{type:"resize",cols,rows}` or `{type:"ack",bytes}`.
- Server text frames are `ready`, `exit`, or a typed `error`.
- Input/output frames are at most 64 KiB; control frames are at most 4 KiB.
- Terminal size is 2–500 columns and 2–200 rows.
- The server stops reading PTY output at 512 KiB unacknowledged output and
  resumes only after browser render acknowledgements reduce the window.

## Guest and lifecycle boundary

Guest control remains on vsock port 1025. Terminal streaming uses port 1026 and
attaches only to the PTY master owned by guest-agent. Session stop, expiry,
binding revoke, workload restart, or generation replacement closes the PTY and
gateway. A stale generation cannot attach to a replacement workload.

## Security invariants

The authenticated gateway applies the existing surface assertion contract:
exact Origin allowlist; session, surface, principal, expiry, and one-time JTI
binding; a host-only HttpOnly cookie; and no credential in a URL or guest. It
relays only to the selected guest VM's terminal vsock endpoint. Terminal bytes,
keystrokes, and transcripts are excluded from logs, analytics, and persistence.

## Readiness

Ready means the public authenticated path reached the guest terminal broker,
attached to the current workload generation, and received a `ready` control
frame while the workload was alive. Listener bind or PTY creation alone is not
readiness.
