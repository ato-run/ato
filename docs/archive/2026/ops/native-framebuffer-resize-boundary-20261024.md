# Native framebuffer resize boundary

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

Date: 2026-10-24. Parent: text-paste boundary #1539,
`f8f8b8a96df663c1953ad413d8b3eed3441891f2` (frozen base 876cb714).
Code/test/RFC commit: `95825c007e6a640c23909e2d612992211c0ba9ad`.

This adds physical resize admission to the same bounded RFB listener. It does
not enable an API minting path, Runner capability, producer, noVNC option or
product control. The existing initial D viewport, frozen probe, K and Run are
unchanged. A framebuffer ACK is not application Contract evidence.

## Contract

`start_interactive_pixel_gateway(config, authorizer, PixelGatewayControls)`
reuses one existing gateway implementation. Both `text_paste` and
`framebuffer_resize` are false by default. Each operation requires both the
trusted physical producer mask and the per-connection signed permission.
`start_text_paste_pixel_gateway` remains the text-only wrapper: even a true
resize claim cannot upgrade it. `start_pixel_gateway` retains the existing
byte-transparent legacy behavior and producer restrictions.

The optional signed `framebuffer_resize` field is default false / omitted when
false, preserving old serialized claims. True requires a User principal and
exact Run/lease/Runner execution scope. Existing signature, Origin,
session/surface, replay/jti and TTL checks remain in force. The permission expires
on an already-open connection, including during RFB fragment assembly.

The bounded filter forwards a complete SetDesktopSize only after validating:
24 bytes, one screen, zero padding, zero screen origin, matching header/screen
geometry, zero flags, positive dimensions <=4096x2160. It preserves the opaque
isolated server screen id. Unknown/malformed, multi-screen, oversized, missing or
expired permission fails before any offending message prefix is forwarded.
There is no clamp, rewrite, host DISPLAY selection or new semantic operation.

One atomic resize limiter belongs to the gateway, and permits at most one
accepted resize per second across concurrent connections and reconnects.
Invalid geometry or missing permission cannot consume the allowance. Existing
RFB and lower WebSocket 5-second absolute assembly deadlines remain active;
interleaved Ping does not extend them. Complete accepted resize updates input
activity. No lock is held across an async write.

## Local and CPU Linux verification

All artifacts were built in task-owned sources/targets, with jobs2/debug0 /
incremental0, `--offline --locked` and fresh workspace `.tmp` roots. The Linux
shared target was used exclusively after explicit handoff, all own Rust/Cargo
input mtimes were refreshed and all content hashes were checked before/after.
No installed binary or service was changed.

- macOS IPC/netd all targets: 263 passed, one existing ignored case. The final
  additional cross-language authorizer test passed separately; final all-target
  Clippy (`-D warnings`) passed. Formatting and diff whitespace checks passed.
- Linux IPC/netd/gateway all targets: 272 passed / three ignored (one existing,
  two explicit real-container tests), followed by the final cross-language
  authorizer test: one passed. Final same-source Resize and clipboard actuals
  below were then executed explicitly, outside the default ignored suites.
- Final Linux all-target Clippy passed for IPC/netd/gateway/Worker, plus fmt.
  All 572 source input hashes still matched after compilation and execution.
- The first final Mac invocation hit the same socket-path constraint documented
  in #1539: its derived Unix control-socket path reached 104 bytes, and the three
  daemon egress tests could not start. A fresh shorter `.tmp/r.*` root made the
  complete suite pass without changing code or skipping tests.
- Unit/socket tests cover every resize split, exact bounds, padding/geometry /
  screen flags, expiry on the last fragment, fixed deadline, reconnect and real
  thread races on the limiter, physical-mask/signature intersection, default
  deny, text-only resize denial, disabled text paste on a resize-only listener,
  rejected upstream zero bytes, legacy relay and existing clipboard boundaries.

The shared API/Rust fixture is
`services/netd/tests/fixtures/native-interaction-assertion-v1.json`:
fixture SHA-256 `abd294836d81f9297624a8b20dc6fae9f44477e3fe8e642721ad06279454bac3`;
exact token SHA-256
`a57eb0809b251ebd3fd571301d1020fa20f42f83bac3c2cf9458ad7afc6f49f4`.
The Rust authorizer authenticates the literal API token, not a Rust re-encoding:
User/execution, both permissions retained to exp200 at now100, expired at200,
and signed Guest/unscoped variants refused. All fixture keys are public test
values. No operator or user credentials are used.

## Actual isolated TigerVNC

The ignored CPU test is
`pixel_resize_linux::owned_unix_tigervnc_resize_has_real_ack_frame_reconnect_and_denied_zero_bytes`.
It uses the already-present pinned image
`ato-native-display-test@sha256:7076a8aadff1e92a09ba843928380aecfb00395cde9bce697e926174af04a9bd`.
There is no image pull, build, package install, host display or host clipboard.
The fixture is numeric nonroot, readonly, network=none, cap-drop=ALL /
no-new-privileges, 256MiB/0.5 CPU/32 PIDs, with Unix-only RFB. Only a fresh own
0700 socket directory is mounted, without parent chmod/chown. The short client
socket path uses an owned directory fd; it cannot rebind another host display.
Only the ephemeral test container has its entrypoint overridden for this
transport acceptance; the production helper and Runner are unchanged.

On final source, actual resize passed in **1.90s** including fixture lifecycle:
initial 640x480 real frame; exactly one SetDesktopSize; request-specific
ExtendedDesktopSize success (reason1/status0); canonical full 1024x768 BGRX frame,
**3,145,728 bytes**; another scoped gateway connection retained those dimensions
and full frame. Missing resize permission forwarded **zero** offending bytes.
The listener, private relay and owned container were closed/deleted.

The fixture initially sent resize before the initial frame, then repeatedly
requested non-incremental frames while negotiating ExtendedDesktopSize.
TigerVNC returned initial reason0 metadata rather than a request-specific ACK.
The final fixture first consumes a real initial frame and uses the incremental
request sequence already used by the existing noVNC acceptance helper. ACK
reason1/status0 remains mandatory; production and expected K were not changed.
This is protocol setup correction, not an observed production resize failure.

The existing actual Latin-1 clipboard acceptance also passed on the same final
source: **180ms** measured delivery, **0.73s** entire fixture; denied/partial
prefix zero, server clipboard disabled and container removed. This confirms the
text-only wrapper retains its restrictions.

## Existing Native regression intersection

The same-source Native matrix ran 28 cases: **27 passed / one failed**. The
existing real Terminal lease case hit an OCI start command deadline, returned
`AlreadyExited { exit_code: 0 }`, and recorded K false at 14.585s. No deadline was
extended and the failure log is retained. A single diagnostic run of that exact
case with the identical test/Worker binaries and a fresh task root then passed
in **2.64s**, with actual lease **2.377s**, K true and owned stop. The cause of the
initial timeout was not established; it is not labeled a pre-existing bug or a
fully green matrix, and this adds no automated retry.

The other 27 cases include real Pixel lease **3.790s/K true**, independent wrong
K **4.026s/K false**, real GUI producer **2.755s warm readiness** after **6.915s
verified image-input setup**, owner EOF, startup/idle/lifetime, helper exit,
SIGKILL recovery/cookie exclusion and partial helper failure cleanup. The
original K and declared 50ms Pixel settle remain unchanged. The root-only
fresh-process GUI+Files test was not rerun in this slice; its prior combined
acceptance remains separate evidence. Managed Broker/model/file tests were not
repeated as part of this transport change.

Final actual test binary SHA-256 records (captured before execution):

- Resize: `313af7fab64ca046116b49cc6a6eafae456b0e43a88225a7934a77772a982ee1`.
- Clipboard: `e3b57c31aa196aab6f90cd97d46341609183c9566652d6220da896c74ca4fcb1`.

## Source, review and remaining rollout

Final source export is
`/home/ekohsuke/ato-run/.tmp/framebuffer-resize-20261024/source`.
572 Rust/Cargo/config input manifest SHA-256:
`1745586bc3a7d46c59060c9471217a39e853ab014632826e1574302fa8b25613`.
The frozen source is #1539 plus this child; large historical doc media was
excluded from transport, but Rust/Cargo inputs and code fixtures were retained.
The static API vector JSON is additionally bound by the file hash above.

An independent read-only review found no new P1/P2. Initial seven production
file manifest: `c68633b583dd709b6c6bff8fbd740cb6e0fbc2aff8dec672551f51a7ae04f89d`.
The final seven-file manifest is
`187a5400c8fdb01888f6de450d8fcad2a059e9a0e01c2a4dbf050694f58b056c`;
its only change is the appended cross-language test in pixel_authorization.rs,
with no further production-body change.

Task-local logs, source/input manifests and test-binary hashes are under
`.tmp/fresize/.tmp/` and the matching Linux task directory. Source and artifacts
are scratch acceptance exports, not shared installed Runner binaries.
The shared Linux Cargo target was explicitly released after final Clippy and
hash verification. Large own partial source-transfer archives were removed;
no other source, parent mode, cache, service or installed artifact was changed.

Trusted helper/Runtime lifecycle connection, capability advertisement, API
permission minting and PWA resize controls remain coordinated follow-ups.
Pinned noVNC 1.5 advertises ExtendedClipboard unconditionally, which can elicit
automatic negative CutText rejected by this bounded listener. A separate small
prerequisite must strip that single encoding from complete SetEncodings while
retaining all negative CutText denial; it is deliberately not mixed into this
resize proof. Public resizeSession auto-resize also needs UI pacing compatible
with the gateway-wide 1Hz limit. No browser/staging compatibility is claimed.
No staging, feature flag, service installation, paid GPU or production key was
used, and this transport proof does not itself claim a successful Formation.
