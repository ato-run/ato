# VM Snapshot Materializer receipt — 2026-08-23

Status: implementation verified locally; Linux/KVM and staging acceptance pending.

## Repository state

- repository: `ato-run/ato`
- base branch: `feat/contract-verifier-registry-v1`
- base commit: `9f7a9dff`
- branch: `feat/vm-snapshot-materializer-v1`
- implementation commit: `e96d123d0b920ccafd171ad8b323532d867bd8f1`
- Draft PR: https://github.com/ato-run/ato/pull/1295
- initial `ato#1291` head after fetch: `509a05028448dccc684be7769e9d085d39102b6a` (matched instruction)
- initial `ato-api#518` head after fetch: `b457e2db9b311ec5d28a9f3ed9d5f95a30ec66f1` (matched instruction)

## Implemented boundary

- canonical semantic ID: `ato.materialize.vm.snapshot@1`
- legacy alias retained: `ato.snapshot@1`
- canonical workspace ID: `ato.materialize.workspace.snapshot@1`
- explicit `RealizationPlanner`; no lexical Materializer selection
- capability-based, fail-closed VM compatibility
- existing target computation and sealed RecordFrontier verification
- chunked memory/rootfs/vmstate/metadata closure validation
- backend-owned process/API socket/TAP/vsock/session/slot cleanup
- hidden candidate Contract gate before Surface publication
- operation replay through Player and provisionable Actuator routes
- no Materializer-internal fallback

## Identity assertions

- VM bytes do not create or modify `target_computation_ref`.
- changing `record_frontier_ref` does not create or modify the target.
- descriptor reference, artifact digest, and target computation remain distinct.
- the Record Writer verifier is injected at the app layer; the Materializer
  does not advance the computation.

## Focused verification

- `cargo test -p ato-materializer-snapshot -p ato-materializer-vm-snapshot -p ato-realization-planner -p ato-player -p ato-record-writer --locked`: PASS (29 tests)
- `cargo run -p arch-check --locked`: PASS (25 workspace packages)
- `cargo test -p ato-cli --test computation_architecture --locked`: PASS (13 tests)
- `cargo check -p ato-cli --all-targets --locked`: PASS
- focused clippy with `-D warnings`: PASS
- `cargo fmt --all -- --check`: PASS
- `cargo check --workspace --all-targets --locked`: PASS
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: PASS
- `cargo test --workspace --no-fail-fast --locked`: all targets PASS except
  the two known long-path macOS netd `SUN_LEN` cases
- the exact two netd cases at commit `e96d123d` in short worktree
  `/Users/egamikohsuke/Ekoh/projects/ato-run/.tmp/bshort`: PASS 2/2;
  the temporary worktree was removed after verification
- local `target/debug/ato` SHA-256:
  `90ec5eaf275c5c2c332492ec80fa436106c9e2ab37bfa8a87d100f3b04a4d420`

Fake-backend coverage includes descriptor roundtrip; existing computation and
frontier references; physical identity separation; missing, duplicate,
overlapping, length-mismatched, and digest-mismatched chunks; every declared
compatibility facet; restore/activate/publish/wait/quiesce cleanup; Contract
failure; three repeated restores; and the prohibition on backend fallback.

## Not yet established

- Firecracker binary/version and runner capability receipt: `firecracker` is
  not installed on this macOS arm64 host; the probe correctly returns no
  Firecracker compatibility.
- target ComputationRef and RecordFrontier for 2048: not selected or captured.
- replay descriptor and VM materialization descriptor for 2048: not created.
- object graph count/logical bytes/unique upload bytes: not uploaded.
- Linux/KVM real restore: not run.
- fresh isolated browser restores: 0/3.
- staging Feed -> Detail -> Continue: not run.
- staging database changes: 0.

## Production invariant

- inserted rows: 0
- updated rows: 0
- deleted rows: 0
- uploaded production objects: 0
- production approval requested: false
