# Formation 2e-b — Hosted LocalProcess ownership

## Baselines and scope

At start, fetched ato main `aaaaaa15d0cfafdf397977bcabb8eb189fb5c2c2`
and API main `88cb8aae89524239163e7d431033ea43e6dd8e90`.
ato #1400 and API #689 are merged. Migrations 0296 Hosted uncertainty and
0297 Runtime Network budget remain unchanged; no remote migration was run.
No pre-existing 2e-b branch or modified worktree was found.

Existing #1399 is open. Its latest main integration is
`ed1515bb050a798159588b4412e33517b9365151`, verified locally with 52 runtime
and 11 Hosted verifier cases. Normal merge was refused by branch protection;
no admin bypass or auto-merge was requested. 2e-b stacks on that branch.
All new commits carry `[skip ci]`. No deploy, workflow edit, manual CI rerun,
remote migration or environment flag changes.

## Before and after

| Boundary | Before | After |
|---|---|---|
| Launch/argv/cwd/env/toolchain | common `launch::process_executor` | same; caller explicitly supplies existing `ProcessLaunchHost` |
| Workspace/state mount | resolved context, sandbox guest mounts | same |
| ACTIVE ownership | Hosted `ActiveRun::Process(LaunchedProcess)` | same, no additional wrapper/attempt/journal |
| Readiness / K | readiness before ACTIVE; validator observes through API relay | same, common verification helper, unchanged receipt schema/K/D/Run IDs |
| Explicit stop | early leader exit could leave descendants; stop failure | group TERM, bounded group wait, group KILL, confirmed disappearance |
| Drop | adapter fallback could leave TERM-resistant descendants | common handle uses bounded group cleanup; no state side effects |
| State finalization | main lease path gates commit; parallel unused session path could abort on unconfirmed stop | duplicate session start/finish removed; tests and production use lease gate |
| Recovery | leader-only probe; failed readiness could omit process identity | group disappearance also required; failed readiness passes identity to existing journal |

A successful stop still precedes pack/commit and writer/lease release.
Unconfirmed cessation is never turned into a successful commit/release, even
if Drop subsequently succeeds. The caller owns quarantine/recovery. No
recovery signalling of reused PIDs or new distributed protocol is introduced.

## Verification environment

Local worktree: `apps/ato/.tmp/formation-2e-b`.
Local target: that worktree's `.tmp/check/target`.
Linux aarch64 host: existing `oci-linux-test`, Rust 1.96.0, bwrap.
Isolated root: `~/ato-run/.tmp/formation-integrated-20260925/`.
Separate `2e-base/{src,target,scratch}` and `2e-branch/{src,target,scratch}`;
no base/branch target sharing. No Coordinator, D1/R2 or token required for
these fixtures. Use `ulimit -n 65536` for the network broker stress test.
No shared toolchain, credential, previous task output or process is removed.

Baseline: runtime 52/52; Hosted validator 11/11 when run serially. One
parallel baseline run had a transient LocalProcess connection refusal.
The first branch runtime-launch suite hit EMFILE at the default fd limit;
a subsequent broader run passed 137 cases and failed only the Browser E2E
because its Chrome path was unset. These failures are retained in the logs.

Local checks: targeted process tests 11/11, recovery 9/9, targeted clippy
with warnings denied, fmt, arch-check (43 packages).

Linux final targeted results:
- Hosted `runtime_launch`: **83/83**, including real SQLite change → stop →
  commit → next Run restore, failed spawn, writer quarantine/release, fences,
  owner stop and authorization renewal/expiry, existing network grants.
- Common runtime: **54/54**, including TERM-resistant process-group Drop and
  stop, unaffected second Run, common verification and journal uncertainty.
- Hosted validator with `ATO_HOSTED_PROCESS_SHIM` pointing to the built worker:
  **11/11**, real sandbox process → readiness → relay → helper → receipt.
  HTTP status/body K failures remain failures despite readiness success.
- `process_launch_v1`: **1/1**. Workload uses its secret binding to create a
  value-free proof in the state mount; execution evidence excludes the value;
  explicit stop removes all marker processes.
- Linux targeted clippy (five changed/dependent packages): pass.

The full Hosted library also ran: **138 passed, 1 failed**. The remaining
Browser E2E starts installed ARM Chrome, which aborts with SIGABRT before
CDP readiness; the same single test fails identically on baseline
`ed1515bb` with the same Chrome and fd limit (`browser-baseline.log`). No test was ignored or marked
successful. A separate stateful rerun found a fixed-port collision; its
fixture now allocates an ephemeral port, and all 83 targeted tests pass.
Logs remain in the isolated Linux root above (`hosted-final.log`,
`runtime.log`, `validator.log`, `process-launch.log`, `clippy.log`).

## Limits

The state control plane and observe/ack API are fixtures; actual contained
processes, HTTP responses and SQLite state bytes are used. This is not a
staging/production acceptance or a deployed change. Static Hosted, OCI and
service groups are not migrated. Source object transport is 3c-b, retained
object replay is 3d; neither is claimed here.
