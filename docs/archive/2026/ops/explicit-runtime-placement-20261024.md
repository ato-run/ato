# Explicit runtime placement — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

## Change and source

- Frozen PR base: `integration/explicit-runtime-placement-base`,
  `b6341239acea53132ed7583c44b32d731387d8dc`.
- Implementation: `51248d272` (`feat/explicit-runtime-placement`).
- Worktree: `.tmp/fenc`; original encoding-compatibility branch is preserved.
- Scope: connected Worker config/advertisement/admission, its private lease
  envelope, related tests and draft contract. No IPC/Core, K/D/spec/digest or
  canonical launch bytes changed.

`--require-explicit-runtime-placement` /
`ATO_RUNNER_REQUIRE_EXPLICIT_RUNTIME_PLACEMENT` defaults to false. When true,
`runtime_feature=explicit_runner_assignment_v1` is advertised and actual
supported lease kinds are intersected with `runtime_launch` (including an
honest empty result). All other kinds are refused after existing lease
validation and before storage probing, preparing reports or directory creation.

The optional private command `explicit_runner_id` is Coordinator placement
provenance, not authority. Some always must exactly name the authenticated
local Runner, even with the flag off; empty/whitespace/mismatch fails. Missing
or null remains compatible only with the flag off. The runtime direct handler
repeats the same command guard. Existing authorized lease claim, ownership,
execution authorization, runtime admission and cleanup remain required.

## Verification

The old implementation failed the new operator-flag test as expected (one
failure: unknown opt-in flag). After implementation:

- macOS Worker `--all-targets`: **189 passed, 19 ignored, 0 failed**; the
  environment configuration subprocess separately passed its one selected test.
- Worker `clippy --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- New tests include the default/CLI/environment flag, capability marker,
  actual-kind intersection/empty set, old missing/null envelopes, wrong field
  types, Some exact/mismatch/empty in both operator modes, v1/v2/v3 launch digest
  independence, all three non-runtime kinds, and both execution entrances.
- Side-effect tests use an absent work root and a test-owned loopback API
  listener: refusal is specifically the routing error, the root stays absent,
  and the API listener receives no preparing/workspace/binding request. They
  do not launch an app, Docker container, Runner or owner Run.

Commands used an own `CARGO_TARGET_DIR=$PWD/.tmp/target`, own workspace
`TMPDIR`, `CARGO_BUILD_JOBS=2`, dev/test debug info disabled,
`CARGO_INCREMENTAL=0`, `--offline --locked`:

```sh
cargo test --offline --locked -p ato-connected-realization-worker --lib \
  explicit_placement_tests::explicit_runtime_placement_operator_flag_is_opt_in \
  -- --exact --nocapture
cargo test --offline --locked -p ato-connected-realization-worker --all-targets
cargo clippy --offline --locked -p ato-connected-realization-worker \
  --all-targets -- -D warnings
cargo fmt --all -- --check
```

Local logs are `.tmp/explicit-red.log`, `.tmp/explicit-worker-all.log` and
`.tmp/explicit-clippy.log` in this worktree. Test-only fake identity/token values
are not credentials. Shared Linux Cargo was never acquired or used.

Independent read-only review confirmed all four frozen content hashes against
`.tmp/explicit-review.sha256` (manifest SHA-256
`40f96c95b724fec657439e71167dbad5cf2af3183a070ad1d9294f92034fbc8b`),
with no new P1/P2. The reviewer did not edit, build or execute the tests.

## Cross-repository envelope check

The explicit test-only receiver for API-produced existing `ClaimedLease` JSON
is ignored by default. It performs only bounded-fixture parse, existing lease
validation, routing policy and existing launch-spec digest validation:

```sh
ATO_EXPLICIT_PLACEMENT_TEST_ENVELOPE=/absolute/own/claim.json \
ATO_EXPLICIT_PLACEMENT_TEST_RUNNER_ID=runner-fixture \
cargo test --offline --locked -p ato-connected-realization-worker --lib \
  explicit_placement_tests::api_produced_explicit_assignment_envelope_validates_without_execution \
  -- --ignored --exact
```

No new placement schema or semantic golden is introduced. The companion API
produces the optional field only from saved owner-authored explicit placement,
excludes marked Runners from automatic selection, and rechecks its current
Run/lease/owner fences. That API-side authorization is not reproduced here.
The cross-repository fixture is not claimed executed by this record.

## Limits and rollout

No Linux acceptance, deployment, installed Runner/credential/config changes,
new owner Run, real external requests, provider acquisition or paid operation
was performed. Existing ignored Docker/GUI/terminal fixtures were not executed
for this routing-only change. Receiver compatibility, API routing/claim
counterpart, an immutable installed Runner artifact and explicit operator
opt-in are separate rollout steps; none is enabled by this PR. The API must
be receiver-first before a marked Runner is enabled. The marker grants no
Native, OCI, isolation, secret or network capability on its own.
