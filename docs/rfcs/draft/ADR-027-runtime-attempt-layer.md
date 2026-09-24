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

## Migration status (updated with stage 2c)

Which entries execute and verify through `run_attempt`, and which still
build their own observation and receipt:

| Entry | Realizer | Status |
|---|---|---|
| Local Formation (`ato form`) | `FormationRealizer` (build from source) | on the common attempt (2a/2b) |
| Runtime Network ticket | `FormationRealizer` | on the common attempt (2a/2b) |
| CLI Run, LocalProcess `.capsule` (`ato run`, `ato app start`) | `PortableBundleExecutor` (unpack, no build) | on the common attempt (2c) |
| CLI Run, StaticWeb `.capsule` | `PortableBundleExecutor` | on the common attempt (2c) |
| CLI Run, OCI and OCI service group | — | previous CLI path; moves in 2e |
| Hosted Formation job (`run_claimed_job`) | — | previous path, artifact-only `verify()`; moves in 2d |
| Hosted `.capsule` validation (`validator_agent`) | — | previous path; moves in 2e |
| Hosted Run (connected-realization-worker leases) | — | previous path; moves in 2e |

Since 2c, `run_attempt` takes an `AttemptSpec` (frozen K and D, shape,
input identities, restored snapshot) and a `CandidateRealizer` (how the
candidate comes to be running). A Formation adapts its `PlannedCandidate`;
a Run adapts its validated `.capsule`. Neither is converted into the other.
