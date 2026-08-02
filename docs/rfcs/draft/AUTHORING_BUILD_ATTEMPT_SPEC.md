# Authoring Build Attempt Contract

Status: Draft  
Owner: `snapshot-builder`, `snapshot::authoring_evidence`  
Companion: `ato-api/docs/rfcs/draft/authoring-build-plan-and-live-console.md`

## Decision

Authoring detection and execution are separate operations:

1. `setup` downloads and verifies the pinned source, detects or normalizes a
   `capsule.toml`, reports Program Intent, and exits without building, launching,
   or holding a preview runtime.
2. A Build Attempt claims one immutable Build Config Revision. The claim carries
   the exact `authoring_toml` and Effective Build Plan that the control plane
   stored for that revision.
3. The builder creates a fresh workspace and executes only those claimed bytes.
   It must not re-render a recommendation during execution.

This keeps `capsule.toml` as declaration, the Effective Build Plan as normalized
execution policy, and the produced Resolution Lock as observed materialization
state. These layers must not be merged.

## Measured build execution

For this MVP the v1 Ready-State producer accepts two previously gated authoring
facets:

- `[tools]` may pin only the primary runtime detected from the pinned source
  (`node` for Node source, `python` for Python/static source). A secondary tool
  the producer cannot materialize is refused before execution.
- `[[build.steps]]` is executed in declaration order as Dockerfile exec-form
  `RUN` argv. A small fixed shell wrapper emits control markers but invokes the
  authored program through `"$@"`, preserving every argument boundary. The
  command mutates the single exported guest tree, whose
  `filesystem.view_digest` is already measured; it does not create an
  unmeasured side artifact.

Authoring attempts use `--no-cache` for these steps. Standard retry therefore
starts with a fresh authoring workspace and cannot silently substitute a cached
successful command for the current revision.

## First attempt and Resolution Lock

The first Build Attempt cannot pin a Resolution Lock that does not exist yet.
`CleanReplayRequestV1.resolution_lock_digest` is therefore optional:

- absent: the adapter must return the resolver-produced digest and the signed
  receipt records it;
- present: the adapter must return the same digest or the attempt fails closed.

The control plane verifies the signed receipt and records the first observed
digest. A later clean retry pins that digest. This does not weaken source or
configuration identity: both are already fixed by Source Revision, Source
Closure, Capsule Revision, Program Intent, and Build Config Revision.

## Event contract

Each Build Attempt appends `ato.build-event/v1` events with a strictly increasing
sequence. Events expose only exact argv, cwd, non-secret environment names,
status, timing, exit code, diagnostics, and stdout/stderr after redaction. Secret
values are neither accepted in the plan nor sent by the builder.

The container builder's stdout and stderr pipes are drained concurrently into
bounded chunks. Step markers switch the active command before its output event
is appended and close it with the observed exit code. Output-event persistence
is fail-closed: an attempt does not report success if its append-only console
record could not be stored.

The builder reports terminal failures through the fenced job-failure endpoint in
addition to emitting the terminal event. A success receipt remains the authority
for moving the Authoring Session to `clean_replay_verified`.

## Compatibility

The legacy setup-ready endpoint may remain during rollout, but the default
suggested path uses detect-only completion. Interactive setup terminal and held
preview are not part of this MVP.
