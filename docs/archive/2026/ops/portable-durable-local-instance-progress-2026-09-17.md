# Portable durable local Instance progress — 2026-09-17

This records PR 3 of the Portable v0 sequence. It is local-only and was not
deployed to staging or production.

## Scope completed

The existing `ato run <file.capsule>` remains ephemeral. The new, separate
local lifecycle is:

```text
ato app import <file.capsule> [--derivation <sha256:...>]
ato app start <instance-id> [--no-open] [--verification-receipt <path>]
ato app inspect <instance-id>
ato app stop <instance-id>
```

Import validates the canonical bundle and selected Derivation before saving
the original bytes. Application directories reuse immutable bundle bytes by
digest, while every import creates an independent Instance. Each start creates
a fresh Run-specific workspace, log, and verification receipt. The worker is
published active only after all original K observations are satisfied.

Active Run claims and releases are token-fenced. Stop matches boot session,
process start time, PID, and process group before signalling the worker. The
worker drops its Static, Process, or OCI runtime before acknowledging cleanup.

## Manual acceptance

The manual trial used `samples/interop-static.capsule` with a fresh `ATO_HOME`
under this worktree's `.tmp/` directory. One import produced:

- Instance:
  `linst_79f0ddc70c1ee9fc2bd48102f9ee3b514a1ee3c56f0713d67b344c263f9322fa`
- bundle SHA-256:
  `sha256:95b837f4e8ed3c3354a4560e56820030cdb2ba34ba2a7c65aa6d26e139bdd813`
- ContractRef:
  `sha256:b565e9d771c53ba03548b1a151c0ebbc2b884c1d6ec532f72596ce33a4b741db`
- selected DerivationRef:
  `sha256:87a0ac84ccdf72ce5ffde7ffc12277d60f3097fb277332a48173c4ddadb7a3cb`

The same Instance was then started, inspected, stopped, and started again
without re-importing. The Run IDs were distinct:

- `lrun_1367494f1d840e4b1939a234eb5d701dd15a2c0be63a0571627ef5b028e1e726`
- `lrun_5e27b19fc9b098831b461b5ce6a772bd08ec04212bb10a0ce13e2aa8efe07d05`

Both Run-scoped receipts retained the same bundle SHA, K, and D and reported
`fully_satisfied: true`. Both workers stopped cleanly, and no worker from the
trial or integration suite remained running afterward.

## Automated validation

- `cargo test -q -p ato-local-execution`: 6 passed
- `cargo test -q -p ato-portable-application`: 23 passed
- `cargo test -q -p ato-cli --test portable_application`: 8 passed
- `cargo clippy -q -p ato-local-execution -p ato-portable-application
  --all-targets -- -D warnings`: passed
- `cargo clippy -q -p ato-cli --all-targets --no-deps -- -D warnings`: passed
- `cargo fmt --all -- --check`: passed

The full CLI clippy invocation still stops in unchanged parent code at
`lib/sandbox/src/macos.rs:213` (`clippy::redundant_closure`). The same failure
reproduced from the PR 2 parent worktree under the identical command; it was
not hidden or changed in this PR.

Unit and integration coverage includes independent duplicate imports,
explicit selection for a multi-D bundle, stored-bundle tamper rejection,
one-active-Run fencing, stale process identity rejection, and the complete
import/start/stop/restart flow.

## Deliberately not claimed

This PR does not preserve user-edited data. `data_snapshot_ref` and `bindings`
are empty placeholders in Instance metadata, not evidence that Saved Data,
Assets, filesystem state, or secrets are portable. It also does not implement
Instance export/delete, Hosted import transactions, PWA UI, User Runner
placement, or production deployment. PR 4 must add the versioned portable
Instance Snapshot and new saved-state Contract before a data round trip can be
claimed.
