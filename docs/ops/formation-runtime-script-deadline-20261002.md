# Source-owned Runtime setup deadline regression — 2026-10-02

Kutt's source-owned `migrate` → `start` path revealed that the trusted Node
Runtime preparation helper could begin another script after the original
execution deadline. The parent Runtime already bounded launch/readiness and
cleanup; the nested script boundary also needs its own check.

The controlled Runtime now passes the original absolute deadline and its
current remaining monotonic allowance in private operational environment
inputs, outside the logical launch spec and D. The helper freezes that
allowance once and checks both clocks before each setup or launch spawn.
Malformed or partial inputs fail closed. The inputs are removed from child
script environments; the existing private variable bindings still reach the
source-owned scripts. No retry, restart or script boundary renews the allowance.

The parent Runtime retains launch/readiness bounds and confirmed process-group
cleanup. The helper does not impose a new lifetime on an already verified
service. Existing immutable D containing the previous helper is preserved;
only newly compiled D includes this change. K is unchanged.

## Validation

- `python3 scripts/acceptance/coverage/runtime-script-deadline-test.py`: eight
  actual helper/child-process cases PASS. Covers controlled/uncontrolled
  success, expiry before the first script, setup finishing after expiry,
  wall-clock rollback, partial/malformed inputs, and unsafe integers. Checks
  private variable delivery without exposing either control input to children.
  This is a component regression, not Coordinator/Runtime acceptance.
- `cargo +1.96.0 test -p ato-runtime-attempt -p ato-formation --lib`: Formation
  122 and Runtime 87 PASS, including the remaining-budget injection test.
- `cargo +1.96.0 clippy -p ato-runtime-attempt -p ato-formation-worker -p ato-cli
  --all-targets -- -D warnings`: PASS.

No paid provider calls, app source rewriting, remote migration, deployment,
ordinary Run permission, publishing or UNKNOWN replay occurred in these checks.
The actual Coordinator/Runtime and Kutt acceptance remain separate gates.
