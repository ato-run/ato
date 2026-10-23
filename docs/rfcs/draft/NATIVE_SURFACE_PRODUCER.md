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

## Explicit guest image selection

The existing `ato-firecracker-guest-image` tool accepts `--terminal-surface` or
`--pixel-surface-config <json>` as explicit, mutually exclusive physical-profile
choices. The Pixel JSON is only `{width,height,rfb_port,display}` and is bounded
at 8 KiB. The image plan records the choice as materialization evidence while
keeping the same ComputationRef, immutable workspace, and declared argv. Web
remains the default and omits the new optional plan fields.

Generation writes the existing guest supervisor configuration and checks that
GUI dependencies exist in the digest-pinned base image. Pixel generates the
existing guest TCP relay from the guest-private address to loopback RFB. Relay
port conflicts fail closed. This is not a public mapping or automatic admission:
the connected Runner still must negotiate the descriptor, validate its declared
Port, start its authenticated gateway, and pass the app's independent K before
publishing a hosted session. No GUI Contract is inferred from image generation.

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

## Private Unix Terminal producer

A non-VM sandbox may opt into `ATO_GUEST_AGENT_TERMINAL_UNIX_SOCKET` while
using stdio supervisor control and the existing single PTY workload profile.
The Agent requires an absolute normalized path in a canonical directory owned
by its effective uid with no group/other access. The socket is 0600. Existing
files/sockets are never overwritten; teardown removes only its own socket inode.
A bounded `CONNECT 1026`/`OK 1026` preface connects the same existing Terminal
frame broker used by Firecracker; the common Runner gateway is unchanged.
This is an explicit private transport, not host shell access or a public endpoint.

Stopping the handle disconnects an attached viewer, joins its bounded server
thread and removes the socket. Dropping the PTY workload stops its process group
with bounded TERM/KILL grace; an unconfirmed stop retains its generation for
recovery. Supervisor stdio EOF therefore does not leave this owned PTY running.
The existing VM/vsock default is unchanged when the opt-in is absent.

Both transports send Ready before reading output but wait for the first bounded
client frame before draining the PTY. A readiness-only connection can close
without stealing the initial prompt/screen. The existing PWA sends a viewport
Resize after Ready. This transport-level Ready does not satisfy application K.

## Portable v3 native declaration (format admission only)

`BoundDerivation.native_surface` is an optional Adapter declaration. It is
omitted from old canonical bytes. Native configuration changes D, never K;
physical listener addresses, host/Provider identities, Run/lease identifiers,
authorization keys and expected answers are absent from the declaration.
Kernel and computation-root wire v2 are unchanged.

The first portable native shape is one digest-pinned OCI serving step with
an explicit non-root numeric UID:GID, one logical native presentation Port,
and a distinct private HTTP probe Port on that same step. The Application
uses its existing `ato.application/2` logical Surface and names the native
Port. Terminal uses the existing `ato.pty@1` Protocol; pixel uses the existing
payload-opaque `ato.tcp@1` Port. RFB semantics belong to the Adapter and
`ato.pixel-stream.v1` session profile; no new Core Protocol is registered.

Example declaration (byte arrays are public, authored interaction input):

```json
{
  "surface": {"kind":"terminal","port":"app.terminal","cols":80,"rows":24},
  "probe_port":"app.probe",
  "operations":[{
    "kind":"terminal_exchange",
    "path":"/result/calculate",
    "input":[50,43,50,10],
    "output_delimiter":[69,78,68],
    "max_output_bytes":4096,
    "timeout_ms":1000
  }]
}
```

`NativeSurfaceAdapter::validate_catalog` is the shared pure bounds validator;
`validate(ports, steps)` additionally checks the logical graph. A catalog has
one to eight actions in authored order, distinct normalized result paths,
at most 30 seconds per action, 120 seconds total, and 64 MiB aggregate output.
An unknown field or action kind is rejected.

Terminal catalog input is one to 4096 printable ASCII/CR/LF bytes; interactive
viewer input retains the existing PTY byte protocol. Output is at most 64 KiB. The nonempty
completion delimiter is at most 256 bytes, fits within the output cap, and
cannot occur in a conservative representation of the input's line-discipline
echo. The validator removes CR/LF while comparing and also rejects ECHOCTL
`^M` variants; newline-only delimiters are refused. This closes ICRNL/ONLCR
and related newline-translation echo completion without assuming termios
settings. The delimiter frames completion; it is not an
expected answer. Measured output includes the delimiter and preserves prior
pending PTY output. Applications and K must account for echo and other actual
output rather than treating an echoed input or a prompt as app success.
An Adapter must also refuse a delimiter already present in pending output
before it sends this operation's input.

Pixel presentation is 64 to 4096 pixels per dimension. `pixel_key_frame`
contains one to 32 keysyms, a result path, timeout, and `max_frame_bytes`
exactly equal to width × height × 4, bounded by the 64 MiB aggregate capture
limit. Each keysym means one down/up pair in
authored order. The captured result is one full B,G,R,X frame using RFB's
32-bpp, depth-24, little-endian true-colour format, channel maxima 255 and
red/green/blue shifts 16/8/0. It is not a PNG, RGBA buffer, readiness frame,
or a boolean outcome. The HTTP result body is the 72 ASCII bytes
`sha256:<SHA-256 hex of the actual captured BGRX frame>\n`; it never contains
the full frame. `pixel_probe_result` is the shared pure encoder. This fits
the existing Hosted observation body bounds without changing any legacy
transport limit. K's `body_digest` is the digest of this measured HTTP body,
not a frame digest given to the Adapter. A different actual frame therefore
changes the observed response and fails K unless its original expectation
matches that response.

The physical Adapter must perform each action once, capture actual bounded
output, and freeze the result before any observation is published. Repeated
GET retrieves the frozen bytes and never repeats the input. It must not
receive the expected K digest or synthesize a `PASS` result. Every catalog
path requires exactly one original HTTP K observation on the private probe
Port, with GET, status 200 and an explicit SHA-256 body digest. Extra actions,
missing observations, native-Surface observations and digest-free health
checks are refused by format admission. The existing Rust HTTP verifier and
receipt decide satisfaction; Surface readiness remains independent evidence.

The validator report adds kind/profile only for native Surfaces and carries
the exact per-D native declaration. Its native route discriminator is
`native_oci_container`, so an older Coordinator refuses it rather than
launching it as ordinary Web OCI. Existing Web report fields remain omitted
as before. The Runner lease DTO can carry the existing typed
`SessionSurfaceDescriptor` as optional `session_surface` metadata outside
the launch-spec digest.

This slice admits the native bundle format, not native execution. Existing
local/Formation execution lowering and attempt admission refuse a native D.
The Connected Runner rejects native `session_surface` before launch and
continues to advertise Web only. Native probe implementation, producer
attachment, capability matching, private endpoint resolution, authenticated
publication, lease-owned cleanup and staging app acceptance require subsequent
implementation and validation. No existing deployment flag is enabled here.

### Runner-owned application observation probe

The common sidecar may explicitly receive `application_probe` containing a
private loopback `listen_addr` and the selected D's shared `NativeSurfaceAdapter`.
It validates configuration/authority before touching the workload, performs
all declared bounded actions once, then publishes only frozen actual results.
The execution scope is mandatory for application observations. Expected hashes,
K verdicts, Provider identity and host paths never enter the authored catalog.

Terminal line-profile input is printable ASCII plus CR/LF. Catalog validation
rejects completion delimiters reproducible by normal PTY echo transforms.
Queued startup output is retained, and a completion already present before the
input is refused. Capture ends at the authored delimiter; overflow, exit,
trailing bytes in that frame and timeout fail without publishing any result.
Full TUI escape/control-sequence probes need a future explicit profile; viewer
input itself retains the existing unrestricted bounded Terminal transport.

Pixel operations negotiate the shared fixed BGRX32 format, send each key once,
and require one complete nonoverlapping frame. The HTTP result is exactly the
shared `pixel_probe_result` encoding: `sha256:<actual frame hex digest>\n`.
K compares the digest of this measured 72-byte response. No expected frame hash
is supplied to the Adapter and existing HTTP response limits remain unchanged.

The result listener accepts only bodyless GETs of declared exact paths, never
re-executes an action, and is distinct from the public Surface. The existing
Run/lease/Runner/Port/path-scoped observation ingress must be its only Hosted
route. All captures must finish before publication; a later failed action
prevents even earlier results being served. Startup cancellation and owner
EOF/signal/idle close owned gateway/probe listeners and in-flight clients.

Frozen evidence does not attest that a workload remains alive. Runner wiring
must still monitor its owned workload and preserve the existing stop/recovery
and quarantine fences. This implementation does not advertise Native capability
or bypass the portable runtime admission guard added in PR #1514.

### Digested native launch declaration (execution remains unavailable)

A v1 OCI Runtime Launch Spec may contain `realization.native_surface` as an
opaque JSON object, bounded to 256 KiB by IPC. IPC does not depend on Formation
or interpret Adapter semantics. Omission preserves existing canonical bytes.
The runtime layer strictly decodes `{derivation_ref, adapter}` using the existing
`NativeSurfaceAdapter` catalog, rejecting unknown fields at every catalog level.
The validator report's explicit lowering projects its exact selected D ref and
unchanged catalog into this field; it never derives or replaces K.

The Connected Runner first verifies the complete spec digest and Run/Instance
identity, then requires the native declaration's D ref to match the command's
`portable_derivation_ref`. The separate session descriptor must match its kind,
profile and dimensions. Pixel uses the existing viewport; Terminal uses explicit
integer `cols` and `rows` entries in the existing scalar capabilities map, with
no inferred defaults. A legacy native descriptor without a digested declaration
cannot authorize execution. This comparison binds a validated control-plane
projection; it does not independently recompute a D ref from absent D bytes.

Even a correctly bound declaration returns `native_surface_unavailable` until
Runner-owned native lifecycle integration exists. The ordinary common OCI
conversion also refuses any native declaration, so another caller cannot ignore
it and silently execute the Web OCI path. No native capability is advertised.
API dispatch, native endpoint allocation and lease-owned sidecar attachment are
separate follow-ups; this wire extension enables none of them by itself.
