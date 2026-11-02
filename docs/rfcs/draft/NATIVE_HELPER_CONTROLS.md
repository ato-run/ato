# Native helper controls (draft)

Status: Draft.

The helper is physical Runner tooling. Controls never enter K, D, a Capsule
reference, or the exact D-derived session descriptor. The accepted Materialization
and Protocol Adapter RFCs require physical realization and secret binding to
remain separate from logical identity; this change keeps that boundary.

## Implemented in this child

The same trusted helper preserves its existing two-argument ABI and behavior.
A new fixed five-argument ABI accepts only
`width height --controls-v1 <paste 0|1> <framebuffer-resize 0|1>`.
Only the Runner constructs those literals. App argv/env, a lease, Capsule or a
browser request cannot select helper controls. Server SendCutText stays zero,
RFB stays Unix-only, and TCP X11 is disabled. The app shares only its own X11
socket and Xauthority; it never receives the private RFB mount or host display.

A trusted helper image declares `org.ato.native-display.controls-abi=1` in its
image config. The existing OCI archive validator must verify digest, graph,
closure and exact image before the Runner performs a bounded read of that exact
config blob. The label does not authenticate an unverified image. Missing or
unknown ABI markers refuse an explicit control opt-in before start. Legacy
configuration remains permitted with the unchanged helper ABI.

Operator controls default to false. An enabled control additionally requires
Linux OCI, verified warm helper admission, the existing numeric UID/HostBoundary
and cgroup capacity checks. The Runner must not advertise a new control feature
from the operator flag alone. Physical availability is separate from session
permission: the guarded common listener ANDs its physical mask with signed,
execution-scoped User permission and the original expiry/Origin/replay checks.

The actual Run uses the existing owned helper/app container pair, recovery,
StopBudget, private UDS relay, authenticated first-frame readiness, idle policy
and independent original Contract probe. Operational support is reported only
from a successfully ready guarded gateway. No viewport recapture or K rewriting
is allowed after user input or resize.

## Agreed operational wire, separate API receiver

The optional field on the existing Runner ready request is:

```json
{"surface_controls":{"clipboard_paste":true,"framebuffer_resize":false}}
```

Omit the field entirely when both controls are false, on legacy helpers or on
Terminal/Web. These are live physical support facts, not a Contract verdict or
a user authorization. The API retains its existing accepted-receipt and live
Run/lease/Runner ownership gate and may expose the same optional operational
field in runtime access/presentation. The exact D-derived descriptor remains
unchanged. Browser-origin permission minting and presentation are API/PWA work.

The heartbeat feature names are `runtime_feature=native_text_paste_v1` and
`runtime_feature=native_framebuffer_resize_v1`. They remain unadvertised until
the corresponding verified helper and guarded listener are actually integrated.

## Verification and current limits

Require default/legacy serialization and behavior regression, invalid helper
ABI/flag-marker mismatch refusal before container creation, fixed trusted argv,
no app/RFB mount escalation, Surface ready independent from K, original receipt
identity, and stop/crash cleanup. CPU Linux actual acceptance uses an offline
COPY-only derivative of the existing pinned helper with no package installation
or registry pull. It is not a shared Runner install, flag activation, deployment
or staging/browser acceptance. Clipboard stays receive-only Latin-1; remote
copy, extended clipboard/Unicode and any full host desktop remain unsupported.

The preceding scoped clipboard listener exists in PR #1539. The framebuffer
validator/claim/listener is the dependency at 95825c007. API #876 receives the
operational field separately; API/PWA owner permission and controls UI remain
separate work. The pinned noVNC 1.5 client advertises ExtendedClipboard, so its
automatic negative ClientCutText response requires a bounded negotiation filter
follow-up; negative CutText remains forbidden. Raw RFB fixtures do not prove
noVNC/browser acceptance. No shared Runner feature is enabled by this PR.
