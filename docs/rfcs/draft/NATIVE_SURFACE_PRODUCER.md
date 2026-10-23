# Native Surface producer and Runner adapter

Status: Draft. Adapter/runtime orchestration, outside Capsule identity.

A native application uses the existing session-surface profiles, gateway
assertions, Runner lease, and workload supervisor. Surface readiness is transport
evidence. It cannot replace or rewrite Contract K or issue a satisfied receipt.

## Linux GUI producer

`SupervisorConfig.pixel_surface` optionally selects a `PixelChildWorkload` for
one long-running service. Existing configurations omit the field and keep their
serialized shape. The supervisor still owns binding delivery, rotation, stop,
process group cleanup, and restore. GUI plus sibling services are refused by
this initial profile rather than wrapping every sibling in a display server.

The common producer starts an independent TigerVNC X server, executes the
existing argv, and stops both when either exits. It never scrapes an existing
window or host desktop. Width/height, unprivileged RFB port, and a private display
are validated. A private Xauthority cookie prevents accidentally attaching to
an existing display. TCP X11 and clipboard in both directions are disabled.
RFB listens on loopback; cross-guest access requires an explicitly isolated
Runner relay and the existing assertion gateway. A public RFB mapping is invalid.
The guest image must contain Bash, TigerVNC standalone, xauth, and xdpyinfo.
Missing dependencies produce a failed workload, never a ready surface.

The producer does not interpret argv as shell text. Binding file paths use the
existing child-only environment composition. Operator managed secrets must not
be injected into this general-purpose workload.

## Runner gateway

The common `netd::native_surface` lifecycle wraps the existing Pixel first-frame
probe and Terminal ready probe. It keeps exact origins, scoped HMAC assertions,
one-time grants, and the existing PTY resize/input protocol. Reconnect attaches
to the same workload; it does not launch another Run. Idle time counts user input
rather than framebuffer polling. Expiry closes listeners and viewers; the owning
Runner must stop the workload through its existing lease lifecycle.

The `ato-native-surface-gateway` sidecar reads one bounded configuration through
Runner-owned stdin. Assertion keys use the existing keyring, never command-line
arguments or output. The same private owner channel remains open; EOF, TERM,
INT, or idle closes the gateway. Readiness emits `contract_verified: false`.
The listener is loopback-only and public access stays behind the existing TLS
session boundary. Unsupported platforms refuse the sidecar.

File upload/download and clipboard are currently denied capabilities. File
exchange needs separate per-Run authorization and path validation; no generic
host filesystem channel is inferred from possession of a surface assertion.

## Integration gates

The existing v1 submission subset remains Web/Terminal. Installing a producer
or passing an RFB test does not advertise a complete hosted GUI submission lane.
The portable application HTTP profile remains unchanged. Image-builder config,
Runner launch/lease wiring, native Contract observations, and the shared PWA
frame must all be connected and verified before Pixel submission is enabled.
The physical display and endpoint never enter Capsule semantic identity.

## Verification

The Linux acceptance script launches the common producer inside a private
network/rootfs sandbox and optionally connects through the actual Rust gateway.
It observes real pixels, keyboard changes, desktop resize, reconnect, assertion
rejection, and teardown. These are surface observations; the report deliberately
contains no Contract success or application acceptance claim.

## Connected Runner assertion scope

Connected Runner gateways accept an optional physical `execution` claim containing
`run_id`, `lease_id` and `runner_id`. This is authorization metadata, outside
Capsule identity. The API derives the existing per-Runner verification key and
uses the Surface audience with key id `ato.runner-surface@1`; a User Runner must
never receive the deployment-global Surface keyring. The existing signed claim
shape is byte-compatible when `execution` is omitted.

A gateway configured for a claimed execution requires exact equality of all
three fields. A legacy gateway rejects scoped assertions, and a scoped gateway
rejects legacy assertions. The readiness probe uses the same execution scope.
The Runner-owned sidecar accepts this scope through bounded stdin configuration,
requires the single Runner key for a scoped execution and never prints key
material. End-to-end lease launch/placement and native Contract verification
remain separate integration gates; this change does not advertise native support.
