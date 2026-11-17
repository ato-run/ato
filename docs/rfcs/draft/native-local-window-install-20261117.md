# Durable native GUI installation

Status: Draft

The SolidCraft sample currently verifies a headless CLI exchange. That is not a
GUI installation or evidence of CAD usability. This change gives both native
Derivations a local desktop window and uses the existing local Instance
import/start/stop lifecycle, exposed together by `ato app install <file|HubRef>` for durable installation. `ato run` stays ephemeral.

## Adapter declaration and evidence

`ato.native.presentation = local-window` selects a macOS realization adapter.
Its one logical HTTP Port is a private adapter observation endpoint, not an
application web UI. No guest port is declared: the Adapter binds a fresh
loopback ephemeral port for each Run. GET `/ready` returns the frozen measurement
`{"window_visible":true}` only after CoreGraphics reports an on-screen,
normal-layer window owned by this Run's process. The unchanged shared HTTP
Contract verifier evaluates that response; the adapter never sets satisfaction.
The new GUI Contract is different from the old CLI Contract. D1 and D2 share
this new Contract and retain distinct DerivationRefs. No display capture or
accessibility permission is needed for window metadata. CAD operations are a
separate human-visible acceptance check, not implied by window readiness.

A local-window route has one filesystem state slot delivered by path. Its HOME
and working directory are that slot; artifacts remain read-only. Runtime network
is denied. The local-window adapter allows only the five Metal/IOSurface IOKit
user-client classes from macOS's GPU profile and registration of the app's own
`com.apple.axserver` local Mach service (accessibility in its PID namespace). The host display is shared, not contained, and receipts say so.
Artifacts, managed toolchains and source caches are materialization outside
identity. Verified GUI trees live at stable cache paths outside Run scratch;
each start rehashes the tree and rechecks the signature before execution. Each start produces a fresh Run and receipt. Explicit unsigned/source
consents are passed to the worker as typed launch options. Installation never
copies untrusted code into `/Applications` or changes a user's existing app.

## Artifact boundary

Remote DMGs are digest/length verified before a read-only, non-browsing mount.
Only top-level real `.app` directories are copied, with bounded file count and
2 GiB total contents. Links/devices inside these bundles fail closed; the DMG's
usual `/Applications` shortcut is not copied or followed. Signature verification
covers the copied bundle. All mount exits detach; detach failures retain and
report the mount location. Hashing streams large files. D2 compiles the pinned
`solidcraft` GUI package with the existing managed Rust/source policy. A local
build requires the already approved explicit unsigned override.

## Acceptance

Stage only: publish a prerelease CLI and GUI bundle, install the public staging
CLI into a fresh environment, select each D explicitly, install/start GUI, create
and edit a solid, save a design, stop/start and reopen it. Verify equal K,
different D, real fully satisfied receipts; second D2 start must be a cache hit.
Record versions, timings, artifact/state paths, receipts and GUI screenshots.
No production deployment or stable release mutation is authorized.
