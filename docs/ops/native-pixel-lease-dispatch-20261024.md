# Default-off owned Pixel lease dispatch — 2026-10-24

This child of #1530 connects the existing connected Runner lease actor to the
owned GUI producer. No shared Runner installation, flag change, staging
execution, paid GPU, operator API key or production operation was performed.

## Admission and publication boundary

`ATO_RUNNER_NATIVE_PIXEL=true` (or `--native-pixel`) is explicit operator
configuration, false by default. It additionally requires a trusted
`ATO_NATIVE_PIXEL_HELPER_ARCHIVE`, its exact
`ATO_NATIVE_PIXEL_HELPER_IMAGE`, and an existing
`ATO_NATIVE_PIXEL_CGROUP_PARENT`. No helper path, image, credential or cgroup
comes from the Capsule or lease. The complete manifest/config/layer graph and
platform are verified once and frozen; the already-loaded exact image config
and platform must be present for advertisement and each new attempt. Missing
warm materialization makes Pixel unavailable, without preventing existing label
recovery. The archive is shared immutably in memory instead of copied per Run.

Only the existing `runtime_feature=native_surface_probe_v1`, intersected with
Linux OCI and the typed `pixel_stream` / `ato.pixel-stream.v1` /
`rfb_websocket` profile, is advertised after physical admission. Terminal's
independent opt-in remains unchanged. The selected D, launch digest, exact
descriptor dimensions, host-owned None ports, session id, trusted origin and
existing scoped Runner assertion key are checked before materialization.
Promised TCP ports, absent binding facts and mismatched profiles are refused.

The existing Native actor owns both physical containers, relay, scoped gateway,
frozen probe results, lease journal and recovery. Surface transport readiness
does not authorize a viewer or satisfy K. The existing private HTTP proof and
Rust verifier observe this Run's original K; API receipt acceptance and its
same-Run verification gate remain necessary before viewer authorization.
Ready evidence contains both observed OCI principals. Existing Rust receipt
service evidence remains `{name, container_id, image}`; the API counterpart must
match it against the stored Ready principals rather than demand new Core fields.

The actual RFB test exposed a URL adapter bug: converting the explicit Native
`/__ato/surface` endpoint to legacy `/surface` correctly failed the outer mux
with HTTP 403. The helper now preserves exactly that explicit reserved path.
Legacy paths retain the prior conversion; Origin, proof and assertion checks
are unchanged.

## Conservative local physical resource scope

The initial backend accepts the default local root-owned Docker Unix socket
only, with matching ambient recovery context and no Docker endpoint overrides.
It verifies the local systemd/PID namespace boundary and actual cgroup-v2 root.
Both app and helper use the explicit existing parent, isolated Docker client
configuration, numeric nonroot ids, read-only roots and hard memory/CPU/PID
limits. Actual created PIDs must enter that parent beneath distinct owned
container ids before probe publication.

Admission observes every effective parent/root cpuset and CPU quota, cgroup
memory headroom, host MemAvailable and matching Docker host capacity. It sums
the application limits, helper limits (default 256MiB/500m CPU/32 PIDs) and 64MiB
of Runner frame/transport headroom before either workload is created. The
dockerd service's potentially sibling cgroup is not used as a workload limit.
Unknown ancestry/capacity or overcommit is refused. The actual cgroup-v2 root
may omit resource-controller files as specified by the kernel; missing files
in a non-root ancestor remain an error.

These are instantaneous admission measurements, not reservations against other
host users or future resource guarantees. One owned slot, per-container limits,
stop confirmation and failure cleanup remain required. Rootless, remote,
containerized/ambiguous host boundaries and unsupported cgroup configurations
remain unavailable.

## Actual CPU fixture and measurements

The static X11 fixture is built with
`tools/native-display-helper/fixture/Dockerfile.static`, fetching packages only
inside the owned build container. Its extracted executable SHA-256 is
`ed69c6cae541241b45bf1bb4c2abc5187b90af97a0cc45261d905efd48752c5c`.
The real lease uses ordinary pinned
`ubuntu:24.04@sha256:52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf`
with `/app/native-app`; no application agent, shell wrapper, VNC package,
registry service or new archive lease field is needed. The helper is the
verified image from #1530:
`ato.local/native-helper@sha256:7076a8aadff1e92a09ba843928380aecfb00395cde9bce697e926174af04a9bd`.

Final Linux serial Native union: 28/28 pass, including 13 actual Docker cases,
14 unit cases and one internal crash-child harness. It includes:

- Real Pixel `execute_lease`, two owned workloads, exact scoped mux/RFB frame,
  independently verified original K, frozen repeated GET, stop and zero residual
  owned containers/networks/listeners: **3,837ms**, `fully_satisfied=true`.
- Independently authored incorrect K, same real frame, Rust rejection, no viewer
  grant and both workloads reclaimed: **3,504ms**, `fully_satisfied=false`.
- Existing real Terminal lease dispatch: **3,457ms**, K satisfied and clean stop.
- Existing GUI owner EOF, helper exit, idle/lifetime, startup timeout, SIGKILL
  recovery/cookie exclusion, malformed offline inputs and partial helper failure.
- Actual trusted local-parent/warm-cache preflight and aggregate overcommit
  rejection before workload creation; typed capability/strict D wire tests.

Selected Pixel D is
`sha256:8e088b257721613e35c666e88729affd7d3bb9b975704c98c6c0cbd14692d2f2`.
The independent producer regression retained the prior original K and measured
3,038ms warm readiness separately from 7,345ms verified archive-input preparation.
These are CPU test-host measurements, not staging or cold-start results.

Linux default library regressions: Worker 164 pass / 17 default ignored, OCI
38 pass / 1 ignored, netd 83 pass, Native gateway 5 pass. Four-crate all-target
Clippy passes. The first Worker run omitted `ATO_TEST_WORKER_BIN` and reproduced
the three existing process-session harness failures (libtest cannot serve as
the sandbox shim). Building the same-source actual Worker and setting that
existing variable resolved all three; no skips or implementation workaround
were added. Actual Native tests above explicitly ran their ignored cases.

An earlier first lease attempt hit the existing five-second OCI command bound
before readiness. Cleanup left zero resources and no successful receipt. Its
cold materialization/scheduling condition is not established as supported; the
bound was not relaxed and no fixture prewarm is claimed as cold acceptance.
Rollout requires explicit bounded pre-materialization, warm admission and a
separate cold setup strategy if cold starts are required.

## Rollout prerequisites

Before operator opt-in: deploy the exact matching API selected-Pixel admission,
typed client/Runner intersection, Ready ancillary helper principal validation,
and receipt-vs-stored-Ready evidence comparison. Keep the existing same-Run
accepted verification receipt gate for viewer access. Install a tested Runner
artifact, pre-materialize the verified helper, configure one local Linux slot
and its known cgroup parent, then perform separate staging app-frame acceptance.
None of those rollout actions is implied by this code-only Draft PR.
