# Formation v0 exploration control — 2026-10-01

Base pins: Ato `8fca0f4b78b1bed92eae21823e5c5f7207058271`; API `9e0031900836090612b94e775e26f980a68f32d7`.

Search/round deadlines remain absolute across resume. Runtime source hashing,
expansion, copying, build steps, process launch/readiness and HTTP/browser
verification use the remaining deadline; execution refuses after expiry.
Cleanup, saved result delivery and provider accounting can continue. A realizer
without a deadline enforcement implementation fails closed. The source-to-OCI
adapter integration is a dependent change, not verified by this PR.

Custom retries reach the attempt ticket and Runtime input/result delivery.
Each operation persists its limit and dispatch reservations before HTTP.
Restart cannot restore consumed retries or replace a frozen limit. Bootstrap
assignment polling precedes receipt of a Search ticket and keeps its own default.
Source acquisition uses atomic owner files; a response lost after promotion
reuses those bytes and the ordinary source digest validator.

Rust owns typed terminal classification: unsupported capability, required input,
source broken (validated supporting evidence), no progress, infrastructure,
budget and deadline. Unsupported outcome rows remain byte-identical; validated
decline metadata is recomputed separately from raw proposals.

Actual provider transport reservations retain per-call usage and latency,
conservative charges for missing responses, unsettled reservations and remaining
call/cost limits. Unknown usage is never converted to a successful zero-token
round. Codex session token usage and price remain unavailable.

Validation: 799 tests passed in 46 related Rust suites, one existing test ignored;
subsequent source deadline test passed, final Runtime/Worker library checks cover
later control changes. API typecheck and 50 related Worker/WASM tests passed;
19 SearchState tests include the added terminal/deadline boundaries. Exact final
counts and tool output are in this task worktree `.tmp/control-*.log` and will be
included with the pinned acceptance report.

No new live provider calls, remote migration, deployment, feature flag change,
normal Run authorization, or public submission occurred. Old measurements and
WBO UNKNOWN are retained. OCI/native/input integration and small real-provider
acceptance remain required; 100-app remeasurement has not started.
