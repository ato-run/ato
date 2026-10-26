# Formation agent CI comparison — 2026-10-03

Measurement ID: `formation-agent-ci-20261003-01`. This is a new comparison and
CI fix record, separate from all prior OSS exploration measurements. It does
not change their execution pins, Search states, receipts, or budget ledgers.

The exact PR baselines were Ato `3b284210` → `31a6d6b4` (#1476) and API
`02a9e58b` → `9b483d4a` (#727), in new clean detached worktrees. Rust was
1.96.0 throughout. API comparisons used the CI's exact Node 22.23.3; the
official archive SHA-256 was verified. The host was macOS 15.8.1 arm64.

See the [machine-readable evidence](evidence/formation-agent-ci-comparison-20261003.json)
for exact commands, failure names, log paths and SHA-256 values.

| Boundary | Before | Change and local result | Remaining evidence |
| --- | --- | --- | --- |
| Windows Runtime compile | Exact base and head each had the same 13 errors | Unix browser launcher internals are gated; unsupported hosts keep the public API and refuse without starting a browser. Missing `bail` and unused Unix-only helpers/imports are fixed. Runtime all-targets clippy passes for `x86_64-pc-windows-gnu`. CLI cross-target check passes. | GNU cross-compilation on macOS does not prove native Windows/MSVC execution. The CLI check retained two pre-existing non-Unix Formation import warnings. |
| macOS durable process test | Exact base and head each pass all 15 portable application tests locally. CI's generic worker early-exit failure lacks its child log. | Empty PATH reproduces that parent error; the child log proves Python 3.12 is unavailable and the Run claim is released. The fixture now reads its own non-secret worker log before cleanup, accepts only the existing admission reasons, and checks receipt/claim absence. Patched suite passes 15 tests; controlled runtime rejection passes the focused test. CLI CI declares Python 3.12. | This controlled cause is not proof of the original CI cause. No new macOS CI run is claimed. Admission rejection is not a successful process receipt. |
| API browser state sync | Exact base and head both fail the WebKit `coop-presence` handshake under Node 22.23.3 | Register the real existing `CoopPresenceRoom` in the fixture. Align the authenticated non-member assertion with production's `404/not_found` concealment. Full state-sync browser acceptance, typecheck and formatting pass locally. | Native Linux execution is pending; Docker was stopped. Telemetry's `nodeCrypto.randomBytes` warning remains on base, head and the passing run. No broad error filter or skip was added. |
| API Activity/CORS CI | Exact base and head each fail the same 13 tests and pass 155 | Classified with identical Node/toolchain conditions; no production or test suppression change | Existing failures remain; full CI is not reported as passing. |

Review units are Ato `47482409` (Runtime portability), Ato `b6f55a2d`
(durable test diagnostics and CI runtime declaration), and API `42d464d9`
(state-sync acceptance fixture). The fixture fix is independent of Session
and input/state implementation. CLI verification includes parallel Session
changes only where explicitly recorded in the JSON evidence.

The changed Runtime also passes 95 native library tests. A subsequent full
Windows-target CLI clippy check reached the new parallel Session code and
stopped on `boxed_local` and `too_many_arguments`; those findings were returned
to the Session owner. This is distinct from the fixed Runtime compile failure
and is not reported as a full CLI clippy pass. The JSON records that attempt's
Session file hash and log hash so a later result can use its own measurement.

No paid reasoning API call, explicit CI rerun, deployment, remote migration,
feature flag change, normal Run authority change, or 100-case remeasurement
was performed. Previous Formation evidence is unchanged.
