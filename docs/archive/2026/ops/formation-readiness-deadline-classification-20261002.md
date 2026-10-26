# Runtime readiness deadline classification — 2026-10-02

At Runtime pin `b7c322e70d28a986581e6f60fb6d716309e5a6c8`, the preregistered
source-owned Node setup control fixture passed the same frozen K normally.
With a six-second Search deadline, setup entered, the app never started,
no second reasoning exchange began, and the Runtime reported/settled after
expiry with confirmed cleanup and no remaining reservation or UNKNOWN.

That actual Coordinator/Runtime experiment also exposed a classification bug:
the readiness trait carries strings, so its deadline became
`candidate_not_observable`. The new boundary rechecks the **original**
ExecutionControl and preserves its typed `round_deadline_exceeded` error.
An arbitrary log containing that text cannot change the classification.
Private readiness context is not substituted for the public typed message.

Local validation: Runtime 88 tests PASS; CLI/Worker/Runtime all-target clippy
PASS. Linux Runtime 89 PASS / 1 existing ignored. Actual Coordinator/Runtime
regression at `7846b40b9c61fb32ef8823372f013b91dc58b6a0`, API
`1626faa000018d6fcb26823ade31ac32e22c7725`: normal case satisfies the same
frozen K; six-second deadline case reports typed `round_deadline_exceeded`,
confirmed cleanup and terminal `deadline_exceeded`, with zero reservations.
The expiry harness initially stopped services before result ACK. The original
immutable saved report recovered using its one remaining retry, without a
requester resume, new execution/inference, or deadline/round/budget reset.
Original provisional status and dispatch 0 are preserved. The corrected
collector reads `exploration_stop_json`, rather than the legacy termination
column; this collector repair restarted only the read-only Coordinator.

Public metadata [proof](evidence/formation-runtime-script-deadline-20261002.json)
SHA-256 `cc84aeb2b30aab4970622d8f97faf249ee9160eece8e0486d5502d3b02a06497`;
private report-recovery proof SHA-256
`857efa94f638839d392a24812f8eb0f7f883db1aa35e2a65ef9c4490bf127467`.
Both cases expose validated phase times through authenticated owner status.
This change does not alter the common compiler or its WASM: authority source
`b7c322e70d28a986581e6f60fb6d716309e5a6c8` remains valid.

The fixture uses fixed preregistered session exchanges and adds no OSS app
success or paid API call. Earlier evidence is preserved, including the first
receiver transfer failure before any Search or app execution. No deployment,
remote migration, normal Run authorization, source rewrite or UNKNOWN replay.
