# Portable native format admission — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

Scope: Draft PR #1514, `feat/portable-native-surface-admission`, based on the
frozen technical integration commit
`89f525af9dad56ced892ee4eaf6368f47beeaf42`. This is a review stack, not a release,
deployment, capability advertisement or approval to execute native workloads.

## Implemented boundary

- Optional `BoundDerivation.native_surface` reuses the existing D and logical
  Port graph. Old canonical bytes omit it. Native presentation and private
  HTTP K observations are distinct Ports, owned by one non-root OCI step.
- One shared pure catalog validator constrains actions, dimensions, time,
  output memory and paths. Terminal completion cannot be raw or translated
  line echo. Unknown action/configuration/credential fields fail closed.
- Pixel evidence is a measured 72-byte ASCII SHA-256 line over captured BGRX
  bytes. Expected output stays in K; existing HTTP body limits are unchanged.
- Every action must have exactly one original HTTP K observation with an
  exact body digest. Surface readiness does not satisfy it.
- The Rust validator report preserves legacy Web omission and names native
  kind/profile and the exact per-D declaration. Its explicit native route
  discriminator is unsupported by old Coordinators rather than read as Web.
- The lease DTO reuses `ato_ipc::session_surface::SessionSurfaceDescriptor`.
  No duplicate Surface DTO or new IPC dependency is introduced. Local
  attempt/lowering and Connected Runner native execution remain refused.

## Validation

macOS aarch64, Rust 1.96.0. Final validation uses this worktree's own `target/`
directory, jobs=2, dev/test debug=0 and incremental compilation disabled.

| Check | Result |
|---|---|
| `cargo test -p ato-formation` | 459 passed across library/integration suites |
| `cargo test -p ato-formation --test native_surface` | 8 passed; catalog boundaries, echo translation, compact pixel wire golden |
| `cargo test -p ato-portable-application --lib` | 110 passed |
| `cargo test -p ato-runtime-attempt --lib` | 163 passed |
| Worker `native_session_surface_is_typed_but_refused_before_launch` | 1 passed |
| `cargo check -p ato-formation --no-default-features --lib` | passed |
| Four changed crates, all-target clippy, `-D warnings` | passed |
| Changed Rust source formatting and `git diff --check` | passed |

The four clippy packages are `ato-formation`, `ato-portable-application`,
`ato-runtime-attempt`, and `ato-connected-realization-worker`. These are
source/unit/protocol checks, not live native app acceptance.

The former integration base `8d23bfa4` reproduced three Worker clippy warnings
in an immutable comparison worktree. Its no-planning build also exposed the
existing repository-evidence feature gate mismatch. The owning composite-K
and repository-automation changes corrected them; the final frozen base
contains those corrections. No lint suppression or test skip was added here.

## Remaining execution gates

No staging deployment, GPU allocation, production key or feature flag changed.
Native catalog authoring/generation, digested launch-spec projection, producer
attachment, private endpoint resolution, Adapter capture, authenticated
publication and cleanup still require the following implementation PRs.

In particular, ordinary OCI starts detached without an attached stdin. The
guest-agent stdio control loop exits at EOF, so replacing app argv with the
Agent is insufficient. A common owned control/foreground mode or a proper
Runner-owned input handle is required. Transient UDS mounts must be owned
runtime resources rather than undeclared saved state. Runner/lease stop and
unconfirmed cleanup must preserve the existing state quarantine fence.

See `docs/rfcs/draft/NATIVE_SURFACE_PRODUCER.md` for the bounded declaration
and independent K contract. No native execution success is claimed by format
admission or by a transport-ready result.
