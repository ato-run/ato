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

A process realization has no NAT: the Runtime allocates a host port, exports
it as `ATO_ENDPOINT_<NAME>_PORT`, and Landlock admits a TCP bind on that port
only. A launch argv or env value that states the Derivation's guest port
verbatim is lowered to the allocated host port. The verifier maps logical
port id → realized endpoint and never assumes `127.0.0.1:<guest_port>`.

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
- The Runtime's contained launch starts workloads at the workspace root and
  ignores `cwd_relative`; a candidate whose Derivation names another cwd is
  not realized (see "Detected mismatch" in the PR).
