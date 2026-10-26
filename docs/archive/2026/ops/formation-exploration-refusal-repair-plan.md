# Preregistered refusal-feedback repair pilot

Frozen Runtime/CLI/authority source: `dfced9182159c143dd1ca994f42bf160eddc9f40`.
API JS stays `b20bba7ab9daf282f8d5b1e22affe549ad3492a3`.
Native/WASM hashes and resources are in the two JSON plans. Prompt v4,
DeepSeek model selector, 3 rounds, 4 attempts, 900 s Search deadline,
source limits, frozen K, toolchains and the external permission ceiling stay
identical to the first repair pilot. No implicit network grant or source rewrite.

The first repair pilot stopped at WBO: one resolved CP call produced a valid
Node D with npm_ci but no network requirements. Actual npm requests returned
403 and kept waiting; the requester deadline expired without a JSON result.
The harness then raised JSONDecodeError and stopped its owned services.
Coordinator state is **UNKNOWN**, attempt and reservations are kept. WBO is
excluded from this new pilot and is never retried or marked failed/completed.
The other three original cases were not executed. Their old plan is preserved.

The repair observes positive refusals recorded by the owner-controlled phase
gate and stops that step's process group promptly. A confirmed stop returns
`network_denied`, actual execution facts and the gate report to the next round.
Unconfirmed stop uses the existing Abandoned/UNKNOWN path. Observational
reports are bounded and can drop records; absence never proves permission.
This changes failure feedback, not scope, budgets, K or rounds.
The harness preserves actual untyped requester failures with raw Coordinator
state instead of parsing them as successful results or retrying the app.

Before new results, select the same remaining cases from the prior evidence:

| Case | Fixed evidence and purpose |
|---|---|
| SVGOMG 57, zero known D | Original archive and K; source-owned build and existing static serving |
| Copyparty 69, zero known D | Original archive and K; source context/inspection representative |
| SVGOMG, prior-live-D replay | Byte-exact original generated failed D `257d10b9...`; actual failure before live repair |

The replay is a separate seeded repair gate, never an automatic cohort success
or zero-known-D case. No fabricated failure. Every PASS requires a fresh receipt
for the exact frozen K and actually executed D. No functional/persistence test.

Group accounting includes the interrupted WBO journal and both new run roots.
Prior WBO conservative charge: **9,831 micros** (one resolved call).
Remaining before new cases: **712,371 micros**.
At most 9 new CP + 3 new DP calls: **96,738 micros** future reservation.
The group ceiling including WBO is **106,569 micros**. Credential values remain
in the sealed requester RAM channel, never logs, argv or saved evidence.
No full 100-app wave under the current remainder. No merge/deploy/migration.
