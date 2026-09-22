# ADR-019 — Formation verifies by running the candidate, briefly

**Status**: proposed
**Context**: Phase 1 of local Formation (`ato form <dir> --runtime local`)

## The question

The Formation model is `I ── D on R ──▶ C, C ⊨ K`: a Formation is not done
until the candidate the Derivation produced is observed satisfying the
Contract. The hosted worker cannot observe everything — it forms an artifact
and never starts the author's process — so its verifier has a third verdict,
`Deferred`, for conditions a named run-time gate will decide later.

Deferred is admission evidence, not Formation success. A local Formation that
sealed a Capsule on a Deferred observation would mint an identity nobody
checked — the same failure the verifier was built to prevent, one step later.

## The decision

Local Formation runs the candidate itself, ephemerally, and requires every
Contract observation to be decided `Satisfied` before it reports `Formed`.

- Static lane: the artifact's own manifest decides — already complete.
- Process lane: the candidate is realized temporarily through the Runtime's
  own process executor, each Contract HTTP observation is performed against
  the endpoint its logical port was realized on, and the realization is
  destroyed before the attempt ends.

The boundary this draws:

```text
Formation:  temporary lifecycle, for verification
Runtime:    durable lifecycle, for use
```

The lifecycles differ; the execution machinery does not. Process execution,
filesystem isolation, network policy, port allocation and teardown are the
Runtime's (`launch_process_with`: bwrap namespaces, the Landlock shim, a
cleared environment, an allocated host port, `LaunchedProcess::stop`). A host
that cannot contain a process does not run the candidate at all — the attempt
is Filtered at admission.

## The Initial Condition is frozen

A local directory is snapshotted once into a codeload-shaped archive (a
`source/` wrapper, no `.git`) and passed through the same proof-state chain an
uploaded source uses (`DownloadedArchive → TreeVerifiedArchive →
materialize`). Detection, planning and building read only the materialized
tree, so the measured `I` is the built `I` even if the directory changes
mid-Formation. Symlinks are archived and refused by the source rules, as for
any other source.

## Verification state is disposable

The candidate runs on a copy of the build output that the Runtime mounts
read-only at `/app`, with a tmpfs `/tmp`. Whatever it writes on first run —
a SQLite file, a cache, generated config — goes with the realization. The
kept artifact is packed from the untouched build output. Capturing state as
part of a Capsule would be a different, explicit Contract.

## Ports

The Derivation is executed exactly: the Runtime never reads argv or
environment values for port numbers, and never rewrites them.

A process realization has no NAT, so the port a workload binds is a host port.
The Runtime's endpoint ABI tells the workload which one:
`ATO_ENDPOINT_<NAME>_PORT` (`app.http` → `ATO_ENDPOINT_APP_HTTP_PORT`), and
Landlock admits a TCP bind on that port only. The guest port is used as the
host port when it is free; otherwise the Runtime allocates one.

- A Derivation that reads the endpoint variable runs either way. Generated
  and Preset routes should be written this way.
- An authored Derivation that binds its guest port literally runs when that
  port is free, and fails visibly when it is not — the attempt names the port
  and the variable that carried the replacement. NAT or port forwarding is
  not part of Phase 1.

The verifier maps logical port id → realized endpoint and never assumes
`127.0.0.1:<guest_port>`.

## Working directory

`cwd_relative` is honored by the Runtime itself (`sandbox::guest_cwd`): the
resolved `effective_cwd` is re-expressed under `/app`, so `apps/web` starts at
`/app/apps/web` for a Run and for a Formation candidate alike. Absolute,
`..` and symlink-escaping cwds are refused by `ResolvedRuntimeLaunchContext`.

## Output

Candidate stdout/stderr are drained continuously and kept in two rotating
segments bounded to 8 MiB in total (`ProcessAdapter::with_output_file`), so
an untrusted candidate can neither block on a pipe nor fill the disk. Failure
messages carry a 4 KiB tail.

## Network

The request's network policy governs the build. The running candidate gets
the Runtime's process policy — no egress, TCP bind on its allocated ports —
under either value, and the attempt's evidence states both separately.

The worker still owns nothing a tenant executes — no ComputeInstance, no Run,
no lease, no stable URL. The ephemeral candidate is part of the attempt, like
the build that preceded it: fenced, bounded, and gone before the result is
written.

## Consequences

- `Formed` means complete verification. `Deferred` remains possible inside a
  hosted job's evidence, but never inside a local `FormationResult::Formed`.
- A Contract observation nothing can perform fails the attempt, locally as it
  already did on the hosted path.
- Runtime selection stays exact: `--runtime local` pins the one Runtime, and
  a failed attempt produces evidence, never a fallback.
- Phase 1 process verification needs Linux with unprivileged user namespaces
  and bwrap; elsewhere process candidates are Filtered, not run unconfined.
- Crate boundary: `formation-worker` depends on
  `ato-connected-realization-worker` for `runtime_launch::{process_executor,
  sandbox, sandbox_exec, resolved}`. That module is a library surface today,
  but its crate is the connected worker application (leases, network broker,
  Docker, record pipeline). The process-execution part is a donor boundary to
  extract into a runtime-execution library before a second consumer grows
  around it; Phase 1 does not extract it.
