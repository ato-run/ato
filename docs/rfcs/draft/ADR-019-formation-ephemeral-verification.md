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
- Process lane: the workspace is launched as a local process, each Contract
  HTTP observation is performed against its loopback port, and the process
  group is destroyed before the attempt ends.

The boundary this draws:

```text
Formation:  temporary execution for verification
Runtime:    durable execution for use
```

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
