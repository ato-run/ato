# ADR-027 — The `runtime` layer and `ato-runtime-attempt`

**Status**: proposed
**Context**: Formation roadmap stage 2a. After ato#1390 the common attempt
entry (admission, start record, execution, observation, receipt) lived in
`apps/formation-worker`, process launch in `apps/connected-realization-worker`
and static serving in `apps/portable-application`. Stage 2 moves the Hosted
job, the Hosted Run and the CLI Run onto one entry; they cannot share code
that lives inside one of them.

## Decision

A new architecture layer, `runtime`, with one crate,
`lib/runtime-attempt` (`ato-runtime-attempt`).

```text
app  →  runtime  →  lib · ipc · computation · objects · adapter · adapter-api
                    · materializer · materializer-api
```

- `runtime` may depend on the layers below it and never on an app. The apps
  it was moved out of depend on it, not the other way round.
- `lib` (`ato-formation`, `ato-sandbox`) could not host it: that layer
  depends on nothing in the workspace. `adapter` could not either: it may not
  depend on `lib`. An app-layer library would have made
  `connected-realization-worker` depend on a crate that depends on it.

### What moved, unchanged

| From | To |
|---|---|
| `connected-realization-worker::runtime_launch::{process_executor, resolved, sandbox, sandbox_exec}` | `launch::*` |
| `portable-application::StaticApplicationServer` (listening, request handling, in-page state bridge) | `static_server` |
| `formation-worker::{attempt, admission, journal, ephemeral, executor, build, static_lane, browser_verify, browser_sandbox}` | same names |
| `formation-worker::{sandbox, sandbox_exec}` (the build sandbox) | `build_sandbox`, `build_sandbox_exec` |
| `formation-worker::job::{PlannedCandidate, plan_candidate, stage_workspace, copy_tree, observe_candidate, digest}` | `plan` |
| `formation-worker::api::{FAILURE_REASON_LIMIT, bounded_reason}` | `text` |

The apps re-export the moved modules under their previous paths, so no
caller changed behaviour. Starting a static application from a
`ValidatedPortableApplication` stays in `portable-application` as
`StaticApplicationServerExt::{start, start_with_state}`; the server itself
takes an already-verified route table.

### What does not move

Leases and fences, the hosted job lifecycle, publication, the Formation
search driver and the Runtime Network client stay in their apps. The crate
does not decide which Derivation to try, and it will not decide whether a
verified candidate is stopped or kept (stage 2b makes that the caller's
`Stop` / `HandOff`).

### `ato-materializer-static-web` is a `materializer`

It had the `app` label with no workspace dependency. The static attempt
serves and observes what it produces, so the `runtime` crate needs it; it
now carries the same `materializer` label as `replay` and `snapshot`.

## Consequences

- One crate executes and verifies a Derivation for every entry that moves
  onto it; the next PRs move callers, not code.
- The layer rule is checked by `tools/arch-check`.

## Migration status (updated with stage 2e-c)

Which entries execute and verify through `run_attempt`, which only verify
through its verification point, and which still build their own observation
and receipt:

| Entry | Realizer | Status |
|---|---|---|
| Local Formation (`ato form`) | `FormationRealizer` (build from source) | on the common attempt (2a/2b) |
| Runtime Network ticket | `FormationRealizer` | on the common attempt (2a/2b) |
| CLI Run, LocalProcess `.capsule` (`ato run`, `ato app start`) | `PortableBundleExecutor` (unpack, no build) | on the common attempt (2c) |
| CLI Run, StaticWeb `.capsule` | `PortableBundleExecutor` | on the common attempt (2c) |
| CLI Run, OCI container and OCI service group | `PortableBundleExecutor` → common `OciCandidate` | on the common attempt (2e-c) |
| Hosted Formation job (`run_claimed_job`) | — | previous path, artifact-only `verify()`; moves in 2d |
| Hosted `.capsule` verification (`validator_agent::verify_hosted`) | none — it observes a Run it does not own | common verification and receipt (2e-a); observation through the existing API `observe_url` relay; not on the common attempt |
| Hosted LocalProcess Run | `LaunchedProcess` | common launch, live handle, confirmed group stop and Drop cleanup (2e-b); Hosted retains lease, authorization, readiness, state and recovery |
| Hosted OCI container / service group | common `launch::oci` (`LaunchedOci`) | common launch, live handle and per-container confirmed stop (2e-c); Hosted retains lease, authorization, TCP egress grants, readiness policy, state and recovery |

Since 2c, `run_attempt` takes an `AttemptSpec` (frozen K and D, shape,
input identities, restored snapshot) and a `CandidateRealizer` (how the
candidate comes to be running). A Formation adapts its `PlannedCandidate`;
a Run adapts its validated `.capsule`. Neither is converted into the other.

### The verification point (2e-a)

`verification::verify_observed_candidate` is the part of the attempt that
decides K and issues the receipt, on its own: an `AttemptSpec` (K, D, input
identities, restored snapshot), the HTTP responses read from the candidate,
the caller's execution evidence and a `ReceiptContext` (target, transport,
Run, dependency profile). It starts, records, stops, cleans up and publishes
nothing. `run_attempt` reaches its verdicts and receipt through it after its
own realization and observation. The Hosted `.capsule` verifier does the
same for a Run it did not start:

```text
run_attempt        admission ▶ start record ▶ realize ▶ observe ─┐
                                                                 ├▶ verify_observed_candidate
Hosted verifier    claim ▶ transport + selected D ▶ observe_url ─┘      K verdicts, receipt
```

For the Hosted verifier this settles only what K means and what the receipt
states. What stays as it was:

- **Observation transport.** Observations still go through the API's
  `observe_url` relay (`?port=&method=&path=`), for the same GET-on-exported
  -port plan `run_attempt` uses. The verifier does not reach the Hosted
  endpoint itself.
- **Execution ownership.** The Hosted Run starts, owns, stops and cleans up
  the candidate, under its lease and fence. The verifier creates no attempt
  record, no realizer and no start; the receipt describes this observation
  of the Run and nothing about its lifecycle.
- **Receipt identity.** Target `ato-run-hosted`, schema /1 (no dependency
  profile), the actual transport digest, the bundle's K, the selected D, and
  the Hosted job's Run, lease, attempt and endpoint as execution evidence.
  No `request_id`.

K, D and the workspace identity come from the validated bundle; the job only
selects a declared D and reports the Instance snapshot the Hosted Run
restored, which K's snapshot observation is decided against.

### Hosted LocalProcess ownership (2e-b)

The Hosted ACTIVE session already held the common `LaunchedProcess` since
2a. 2e-b completes that handle's group cleanup instead of wrapping it in
another candidate or synthesizing an `AttemptSpec`. Its explicit stop and
Drop now use the same bounded group termination, including surviving children
after leader exit. Only confirmed cessation permits Hosted state commit and
writer release. An unconfirmed stop remains quarantined and journaled with
its process identity; recovery checks the group as well as the leader.

The unused parallel `session::start_run/finish_run` production entry points
are removed. The stateful tests now exercise `lease::start/finish`, the same
path the worker serves. The caller explicitly supplies the existing
`ProcessLaunchHost` (worker shim and lease scratch); argv/cwd/env, toolchain,
workspace/state mounts and route selection are unchanged.

Lease/fence/slot, execution authorization renewal, owner stop, bindings,
network grants, readiness and state acquire/restore/commit/release stay in
Hosted. The common handle never commits/releases state. Static Hosted is
unchanged; OCI/service groups remain 2e-c. K is still judged by the 2e-a
verification helper after readiness and ACTIVE, with the existing receipt
schema and actual restored snapshot evidence.

See `docs/ops/formation-2e-b-ownership-2026-09-25.md` for verification and
review status; this is not deployment or completion of 2e-c.

### OCI ownership (2e-c)

`launch::oci` is where an OCI candidate comes to be running and how it
stops, for both surfaces:

- the container a v1 launch spec asks for and its readiness wait, and the
  v2 service-group launcher (moved unchanged from the Hosted Runner);
- `start_service_group`: services in authored order, each ready before the
  next; on failure explicitly attempts stop in reverse order and returns the
  original error plus each stop outcome. Confirmed stops permit reclamation;
  unconfirmed stops retain container/network identities and scratch for
  recovery. Services not yet started never start;
- `LaunchedOci` / `OciStop`: one container or one group, stopped with an
  outcome confirmed per container;
- `OciCandidate`: the `RunningCandidate` for the common attempt, which
  removes its runtime scratch only after every container is confirmed
  stopped.

The Hosted Runner holds `ActiveWorkload::Oci(LaunchedOci)` and stops it
through `LaunchedOci::stop` before its existing state gate. TCP egress stays
the Runner's to authorize: `service_group.rs` creates one egress network and
broker per grant and hands them to the group, which keeps them exactly as
long as its services. Lease/fence/slot, execution authorization, readiness
policy, the ready-report execution evidence, state commit/release and
quarantine/recovery are unchanged.

The CLI realizes an OCI container or group in `PortableBundleExecutor` and
runs it through `run_attempt`, so every portable Run is on the common
attempt and the CLI no longer builds an observation or receipt of its own.
Behavior that changed deliberately: a local Instance's OCI receipt names its
Run from the start instead of being rewritten, a stop reports success only
when every container is confirmed stopped and then removes the unpacked
workspace and OCI scratch, and a non-Linux host is refused at admission.
An unconfirmed CLI stop fences filesystem/browser state save, successful ack,
Run release and the next Run of that Instance. A pre-launch pending marker
also fences release after worker death; worker death and Drop are not Docker
stop evidence. Launch identities are written before Docker can create a
container. Activation, receipt persistence, observation and service-exit
errors must explicitly stop and retain uncertain results. Confirmed cessation
with scratch-removal failure is a distinct lifecycle error. K receipts remain
historical verification evidence, unchanged by these lifecycle outcomes.
Hosted partial-start uncertainty feeds the existing quarantine/recovery path;
no new distributed recovery protocol or automatic CLI recovery is introduced.

Not changed: image identity/offline archive rules, the adapter's isolation
(internal network, readonly workspace, limits), new multi-service
capability, and the old D→projection→intent interpretations (2f). See
`docs/ops/formation-2e-c-oci-ownership-2026-09-25.md`; this is not
deployment.
