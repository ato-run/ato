# Guest-agent ↔ host sanitizer protocol (interface only)

> Interface definition for the M0 spike + the future `ato run` Sanitize phase. The agent is a
> tiny in-guest PID1 helper that talks to the host over **vsock**. Implementation lands after
> the spike proves restore works; this fixes the contract so both sides can be built against it.

## Transport

- vsock, guest CID = 3 (see `versions.env` / Firecracker `/vsock`), host listens on a unix
  socket (`${SPIKE_WORK}/vsock.sock`). One newline-delimited JSON request → one JSON response.
- The host drives; the guest-agent executes in-guest steps and ACKs each.

## Sanitizer steps (each maps to plan §8.2; `layer` = who runs it)

| step | layer | request | success criteria |
|---|---|---|---|
| `regenerate_ids` | guest | `{"op":"regenerate_ids"}` | new session id + machine-id + hostname; old values gone |
| `reseed_entropy` | guest | `{"op":"reseed_entropy","seed_bytes":"<base64>"}` | `/dev/urandom` reseeded from host-provided entropy |
| `refresh_clock` | guest | `{"op":"refresh_clock","unix_ms":<n>}` | guest clock set; timers notified |
| `reset_sockets` | guest | `{"op":"reset_sockets"}` | stale listeners closed; app re-binds |
| `reconnect_net` | host+guest | `{"op":"reconnect_net","iface":"eth0"}` | host re-creates TAP+NAT; guest re-ups iface, renews ARP/DHCP |
| `cleanup_request_state` | guest | `{"op":"cleanup_request_state"}` | tmp dirs + request-local caches cleared |
| `ack_ready` | guest | `{"op":"ack_ready"}` | agent confirms app reachable on the health path |

Response envelope: `{"op":"<op>","ok":true|false,"detail":"<short>"}`. The host applies steps in
the order above, **before** any binding/expose. Any `ok:false` fails the restore closed.

## Host-only steps (no guest-agent)

`port_remap`, `capability_endpoint_remap`, `overlay_mount`, `context_mount` — done by the
host runtime (FirecrackerBackend / QemuBackend) via device + port-forward + proxy rewrite.

## Mapping to ato types

These map 1:1 onto `snapshot::SanitizerStep{ step, layer }` / `SanitizerLayer{GuestAgent,Host,HostAndGuest,App}`
already defined in `crates/snapshot/src/manifest.rs`. The `ReadyStateManifest.sanitizer_contract`
carries the ordered step list; this protocol is how the host executes it at restore.
