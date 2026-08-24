# Record operation model receipt — 2026-08-23

Status: implementation verified locally; Draft PR open

## Base

- repository: `ato-run/ato`
- base PR: `ato-run/ato#1291` (Draft)
- verified base head: `509a05028448dccc684be7769e9d085d39102b6a`
- branch: `feat/record-operation-model-v1`
- Draft PR: `https://github.com/ato-run/ato/pull/1292`
- implementation commits:
  - `760424dc7bfb04780110c39d883cc626c5fc2365`
  - `f252f1478314e514e777e1a38bd9216deb2114fd`
- existing worktrees and receipts were preserved

The remote head matched the expected SHA at the beginning of work. See
`initial-state.json` for the timestamped check.

## Semantic result

- `RecordEnvelopeV2` has Protocol, operation, port, payload reference,
  provenance, local sequence, writer order, causal Record references, and an
  independent BLAKE3 identity.
- `head_before`, `head_after`, and `semantic_frontier` are absent from v2.
- `RecordCandidate` performs no object-store or filesystem I/O and has no
  writer order.
- Stylus emits only applicable HTTP request, PTY input/resize/signal, and
  Binding operations. HTTP response and PTY output remain runtime output.
- Actuator support is declared per operation. `recorded_by` is not used as a
  route identity.
- Player preflight resolves exactly one provisionable route, validates payload
  structure, and does not simulate Record semantics.
- `ato.adapter@1` add/remove/configure semantics live in an extension-owned
  Actuator Provider.
- `ato.replay@1` remains registered for compatibility. `ato.replay@2` derives
  requirements from the complete Record closure and rejects a mismatched
  descriptor summary.
- Canonical Computation residual identity redesign remains explicitly
  unresolved; no Record chain, frontier, or VM bytes are promoted as a new
  ComputationRef identity.

## Local verification

- `cargo test -p ato-cli --test computation_architecture --locked -- --test-threads=1`: PASS (12/12)
- focused adapter/API/player/materializer/object/computation unit tests: PASS
- `cargo clippy` for all changed packages and `ato-cli`, all targets, locked,
  `-D warnings`: PASS
- `cargo run -p arch-check --locked`: PASS (21 packages)
- `git diff --check`: PASS
- `cargo fmt --all -- --check`: PASS
- `cargo check --workspace --all-targets --locked`: PASS
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: PASS
- `cargo test --workspace --no-fail-fast --locked`: all changed and broader
  targets PASS; two existing netd terminal gateway tests FAIL in the long
  worktree with macOS `SUN_LEN`
- the same two netd tests at the same commit in the short workspace worktree
  `/Users/egamikohsuke/Ekoh/projects/ato-run/.tmp/a1s`: PASS (2/2); the
  temporary worktree was removed after verification

GitHub checks are recorded after the Draft PR is published; no local result is
reported as a substitute for CI.

## Production

- inserted rows: 0
- updated rows: 0
- deleted rows: 0
- uploaded production objects: 0
- production approval requested: false
