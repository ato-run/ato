# Final placement/validation repair acceptance preregistration

Freeze `2ad8c108b58a170a8673663d03ad8c7eaa6988fa` (CLI, Runtime,
Rust receipt authority), API JS `b20bba7ab9daf282f8d5b1e22affe549ad3492a3`.
Run **SVGOMG 57 only**, original source archive and frozen K, zero known D.
Native/WASM hashes and host/resource snapshot are in the JSON.

This is a new condition after diagnosis of the prior completed repair pilot:
its round1 D was already valid, but empty asynchronous placement inventory
opened two more calls without execution failure. Empty inventory now waits;
a real hard-filter refusal can still trigger repair. UNKNOWN and pending
attempts retain priority, frozen budgets/rounds/CAS do not reset on restart.
Also accept omission of guest_port for static serving only; process plans
still require a nonzero explicit guest port. Shared logical HTTP K validation
is independent of a Python process template. Nineteen core tests passed.

Prompt v4, model/thinking, default3 rounds, attempt4, deadline900 s, source
context16 KiB and the permission ceiling stay unchanged. No post-result change.
Check fresh exact-K/D receipt, actual generation rounds, whether later calls
are permission reduction rather than premature repair, normal verified_routes0,
approval not_assessed, deployed false. Functional acceptance is not measured.

At most 3 CP + 1 DP calls, reservation **32,246 USD micros**. Prior repair
consumption: 10 CP, 0 DP, conservative **98,310 micros**, estimate **28,916**.
Remaining before this gate **623,892 micros**. No full100 wave under remainder.
Keep WBO UNKNOWN and state; it is not a target and will not be retried.
Per-call latency is absent in the existing journal; case wall time is recorded,
not mislabeled as provider latency. Values remain in sealed requester RAM.
