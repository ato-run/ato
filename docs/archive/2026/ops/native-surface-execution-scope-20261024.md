# Native Surface execution scope — 2026-10-24

Base: #1507. Branch: `feat/native-surface-execution-scope`.

The shared Surface assertion DTO, netd verifier and Runner-owned sidecar can now
bind an assertion to the exact Run/Lease/Runner while keeping the old omitted
wire field unchanged. Scoped and legacy authorization cannot downgrade into each
other. Readiness probes use the same authority. This enables the existing
per-Runner key to replace deployment-global signing material at User Runners.

macOS validation:

- `cargo test -p netd --all-targets`: 75 library, 1 sidecar, 3 control-ingress,
  11 ingress, 5 lifecycle, 9 resolver tests passed.
- `cargo test -p ato-ipc --test session_surface_contract`: 21 passed.
- `cargo clippy -p netd -p ato-ipc --all-targets -- -D warnings`: passed.
- Tests cover all three changed execution fields, legacy/scoped downgrade,
  scoped readiness, and omission of the optional field from old claims.

These are transport/authentication tests, not a hosted GUI or Contract K
acceptance. No deployment, global signing-key distribution or native capability
advertisement is performed by this change.
