# Hosted common attempt: stage 2d acceptance

Based on main `722f07610365d96d2432dbde29439e8c217c94e6`. Network receipt
WASM stage 3a is an independent PR. API v1/v2 receiver: ato-api#688; that receiver
must be available before v2 worker rollout. No deployment or migration was done.

## Actual execution, simulated publication API

`hosted_attempt_v2` calls the production `run_claimed_job`, source materializer,
planner, common attempt, real executor/server, HTTP observer, Rust verifier and
cleanup. Its loopback API fixture simulates artifact and result publication; it
is not a deployed Hosted service or R2 integration test.

- macOS: Static success; HTTP K failure stops without publication; HTTP 503 on
  artifact upload preserves the satisfied receipt and marks publication failed.
- Linux aarch64 (`oci-linux-test`): the same cases plus real bwrap execution of
  pinned Python 3.12.7 and Node 22.14.0. Their HTTP responses check their own
  runtime versions. No process test skip on this host.
- Same attempt redelivery stops before a second source fetch/execution.
- Candidates no longer answer at their actual endpoint after `Stop`.
- Shared v2 golden originates in the macOS static execution and is identical in
  both repositories. Its canonical digest is asserted in Rust and TypeScript.

The journal contains durable execution state. The atomic, fsynced Hosted outcome
sidecar contains the receipt and independent caller outcomes before uploading.
API registration stores successful result JSON including the receipt. This does
not claim remote persistence of failed-publication evidence or upload recovery.

## Regression checks

- macOS runtime-attempt + formation-worker: 216 passing tests.
- Final focused Hosted suite: 4 passing on macOS; 5 on Linux (including both
  process runtimes in one test).
- Targeted clippy with `-D warnings`, cargo fmt and architecture check pass
  (42 packages).
- API companion: typecheck and 81 related tests pass; 22 affected tests pass after
  the final receipt shape check. v1 canonical digests are unchanged.

## Stage boundary

This replaces Hosted Formation's artifact-derived HTTP inference. Hosted Run,
validator_agent and existing OCI/service group are still stage 2e. Internal
ProgramIntent/EffectiveBuildPlan removal is stage 2f. No AI, transport extension,
search budget, new language support or real-app coverage claim is included.
