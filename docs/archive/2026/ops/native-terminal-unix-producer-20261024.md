# Private Terminal producer — 2026-10-24

Base: ato #1511, branch `feat/native-terminal-unix-producer`.

The existing guest PTY broker can serve an explicitly selected, private Unix
socket inside a non-VM sandbox. This reuses the same bounded Terminal protocol
and supervisor workload; no per-app server, global key or public endpoint is
introduced. PtyChildWorkload owns bounded process-group cleanup on Drop.

Linux x86_64 validation on the existing CPU host:

- `cargo test -p guest-agent --all-targets`: library 83, binary 17 and real
  binary smoke 2 passed. New transport tests exercise real PTY input, actual
  terminal size, reconnect, readiness-probe initial-prompt preservation, mode
  0600, private-directory requirements, no overwrite, active disconnect,
  workload Drop cleanup and preservation of a replacement file.
- Actual binary smoke closes owner stdin with an attached viewer and confirms
  Agent exit, socket removal and termination of its owned workload without a
  Stop RPC. Linux all-target Clippy with `-D warnings` passed.
- Test TMPDIR is a fresh short directory under the task workspace `.tmp` to
  remain within Unix socket length limits. No system service was changed.
- macOS existing library 80, binary 17 and binary smoke 1 passed. The new Unix
  producer is Linux only.

This slice is a private producer/transport integration test. Full common-gateway
WebSocket, hosted Runner lease, PWA shared frame, persistence and application K
acceptance remain separate gates. No staging or production deployment, paid
resource, API key, capability advertisement or feature flag was changed.
