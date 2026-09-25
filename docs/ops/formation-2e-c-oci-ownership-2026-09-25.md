# Formation 2e-c — OCI container and service-group ownership

## Baseline and scope

Started from ato main `da4d179059d2c9027058282aaeeeb75341c33a13` (after
#1399/#1401/#1403/#1404 and API #691 were merged). Branch
`refactor/oci-execution-ownership`. All commits carry `[skip ci]`. No deploy,
remote migration, flag change, workflow edit or manual CI rerun.

Moved: the existing OCI container and OCI service-group execution of the
Hosted Runner and the CLI onto `ato_runtime_attempt::launch::oci`, and CLI
OCI Runs onto `run_attempt`. Left with the callers: lease/fence/slot,
execution authorization, TCP egress grants (Hosted creates the networks and
brokers), readiness policy, state acquire/restore/commit/release,
quarantine/recovery. Not in this change: new multi-service capability, the
old D→projection→intent reduction (2f), image identity or adapter isolation.

## Before and after

| Boundary | Before | After |
|---|---|---|
| Hosted container spec + readiness | `lease.rs` | `launch::oci::{container_adapter, wait_until_container_ready}`, moved unchanged |
| Hosted v2 group launch | `service_group.rs` (spec, order, readiness, egress) | `launch::oci::{launch_service_group, start_service_group}`; Hosted keeps egress network/broker creation per grant |
| Hosted ACTIVE handle | `ActiveWorkload::{Oci(OciHandle), OciServiceGroup(..)}` | `ActiveWorkload::Oci(LaunchedOci)` |
| Hosted stop | per-variant `stop_gracefully` | `LaunchedOci::stop` (same adapter calls), then the unchanged state gate |
| Egress grant naming an absent service | refused after every service started | refused before any container starts |
| CLI OCI start | `PortableLocalRuntime::start` (own path) | `PortableBundleExecutor` → `OciCandidate` through `run_attempt` |
| CLI OCI verification | own observation loop, `verify_runtime`, receipt assembly | common HTTP observation and `verify_observed_candidate` |
| CLI local Instance OCI receipt | `run_id`/`attempt_id` written into the receipt after verification | named by the attempt from the start |
| CLI OCI stop | handles dropped; lifecycle recorded `succeeded` unconditionally; workspace and OCI scratch left | confirmed per container; scratch removed only after a confirmed stop; otherwise `failed` and scratch kept |
| CLI OCI on a non-Linux host | adapter error during spawn | refused at admission (`oci_needs_native_linux`), before the start record |

## Verification

Linux host: `ubuntu-sugamo` (x86_64, Docker 29.1.3), isolated
`~/formation-2e-c/{base,branch}/{src,target}` and `scratch/`; the base tree
is `346aa8d1` (main + the baseline test), the branch tree is the branch
head with the same tests. Only resources labelled `runner-id=itest-2e-c`
(Hosted tests) or container ids taken from receipts (CLI) were inspected;
the host's other containers were not touched. Images used were already
present with the pinned digests.

Hosted, real Docker, through the production `lease::start` → `lease::finish`
(`--ignored`, serial):

| Case | Base | Branch |
|---|---|---|
| single container (whoami) | pass | pass |
| two-service group (whoami + nginx proxy, state + secret) | pass | pass |
| group with one TCP egress grant | pass | pass |

The normalized documents (execution evidence, served Surface status, running
containers/networks, per-container environment names and network count, stop
outcomes, state commit and writer release, resources left) are
byte-identical between base and branch for all three. Only the granted
service got the egress network and `ATO_BINDING_UPSTREAM`; nothing labelled
remained afterwards. One early branch run took 61s instead of ~4s; three
reruns took 4.3–4.8s (base 3.8–3.9s) and the outlier did not recur.

CLI, real Docker (`.tmp`-style acceptance script: `ato run --no-open`
receipt, then `app import` / `app start` / request / `app stop`):

| Fixture | Base | Branch |
|---|---|---|
| `datasette-cpu.capsule`, Python process D | verified | verified (path unchanged since 2c) |
| `datasette-cpu.capsule`, OCI D | verified, `Runtime: OCI container` | same |
| packed `oci-service-group-proof` | verified, `Runtime: OCI service group` | same |

Receipt differences: the OCI receipts now carry `attempt_id` and
`request_id` like every common-attempt receipt. The only other difference is
`body_sha256` of observations whose K pins only status: those bodies vary
per run (whoami echoes its container hostname; the unchanged Python D varies
the same way). Every K-pinned body digest is identical. Containers were gone
after `ato run` exited and after `app stop` on both. After `app stop` the Run
directory's `workspace/` and `oci/` remain on base and are removed on branch
(intended). The lifecycle record says `succeeded` on both.

Linux suites: runtime-attempt lib 54 (base) / 55 (branch) pass. Hosted
library with the worker shim built: base 139/139 in one run; branch 137/139
in one run, with `browser_e2e_…` and `volume::a_replacement_is_all_or_nothing`
failing. Repeated separately, the Browser E2E fails on **both** trees
(base 1/3, branch 1/3 passing; a WebMCP registry-generation assertion) and
every volume failure is `Locked` / `state_volume_locked`: host-level
contention on a shared host, with the volume suite passing 3/3 in
isolation on both. Neither touches OCI.

Local (macOS): fmt, `clippy --workspace --all-targets --all-features -D
warnings`, arch-check (43 packages); runtime-attempt 55, portable-application
79, formation-worker 183, adapter-oci 21, ato-cli (all suites, portable 15)
pass. The Hosted library passed 138/139 in a parallel run; its Browser E2E
passed alone.

## Not verified

- An unconfirmed OCI stop on the CLI path (scratch kept, `failed` recorded)
  was not exercised against Docker; it follows the adapter's existing
  `StopOutcome::Unconfirmed`.
- Offline OCI archives (`embedded_oci_image_loaded`) were not rerun.
- No staging/Hosted deployment; Hosted OCI was exercised through the lease
  functions with a fake control plane, not a live Runner.
