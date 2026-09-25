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

## Migration status (updated with stage 2e-b)

Which entries execute and verify through `run_attempt`, which only verify
through its verification point, and which still build their own observation
and receipt:

| Entry | Realizer | Status |
|---|---|---|
| Local Formation (`ato form`) | `FormationRealizer` (build from source) | on the common attempt (2a/2b) |
| Runtime Network ticket | `FormationRealizer` | on the common attempt (2a/2b) |
| CLI Run, LocalProcess `.capsule` (`ato run`, `ato app start`) | `PortableBundleExecutor` (unpack, no build) | on the common attempt (2c) |
| CLI Run, StaticWeb `.capsule` | `PortableBundleExecutor` | on the common attempt (2c) |
| CLI Run, OCI and OCI service group | — | previous CLI path; moves in 2e |
| Hosted Formation job (`run_claimed_job`) | — | previous path, artifact-only `verify()`; moves in 2d |
| Hosted `.capsule` verification (`validator_agent::verify_hosted`) | none — it observes a Run it does not own | common verification and receipt (2e-a); observation through the existing API `observe_url` relay; not on the common attempt |
| Hosted LocalProcess Run | `LaunchedProcess` | common launch, live handle, confirmed group stop and Drop cleanup (2e-b); Hosted retains lease, authorization, readiness, state and recovery |
| Hosted OCI/service group | — | previous path; moves in 2e-c |

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
