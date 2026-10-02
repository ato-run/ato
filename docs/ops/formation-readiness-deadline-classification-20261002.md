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
PASS. The corrected pin still needs an actual Coordinator/Runtime regression.
This change does not alter the common compiler or its WASM: authority source
`b7c322e70d28a986581e6f60fb6d716309e5a6c8` remains valid.

The fixture uses fixed preregistered session exchanges and adds no OSS app
success or paid API call. Earlier evidence is preserved, including the first
receiver transfer failure before any Search or app execution. No deployment,
remote migration, normal Run authorization, source rewrite or UNKNOWN replay.
