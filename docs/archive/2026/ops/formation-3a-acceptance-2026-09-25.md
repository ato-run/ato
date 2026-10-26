# Stage 3a receipt authority — local evidence, 2026-09-25

Foundation base: ato #1396 merged `722f07610365d96d2432dbde29439e8c217c94e6`;
ato-api #686 merged `c0f2682e752fce91f0d6e72e4d51b67b31e0725d`.
No deployment, remote migration, flag change or manual CI rerun.

Before merging #1396, latest macOS CI also reported
`reserved_paths_keep_their_own_verifier`: ConnectionReset at CRW lib.rs:5931.
Rebuilt the unchanged test on pre-3b `60e08462`; parallel independent invocations
reproduced the identical failure. No CI fix was mixed into 3a. The known macOS
portable process/Windows compile failures remain outside this scope. API CI
was not started because of billing. #1396 required reported ruleset bypass.

## Tests

- Native ato-formation unit/integration tests: 173 pass.
- Native receipt-authority golden/boundary tests: 5 pass.
- Actual Rust Runtime Network worker tests: 21 pass, including static execution
  and historical journal/source-pause fault injection from 3b.
- API Coordinator: 53 pass, including 7 new forged-receipt/frozen-K tests and
  existing UNKNOWN/late-result/concurrent settlement/crash-recovery tests.
- Actual Worker WASM: 19 pass; shared wire: 7 pass; D1 migration: 3 pass.
- Targeted clippy with warnings denied, fmt, architecture (43 packages), and
  API typecheck.

Coordinator tests use simulated Runtime reports, native Rust-generated receipt
fixtures and real local D1/R2/Worker WASM. They are not deployed Runtime tests.

## Real local Runtime Network vertical slice

Fresh isolated local D1 through migration 0295; synthetic owner and runner;
`wrangler dev --local` on loopback. The actual rebuilt `ato runtime-network serve`
and `ato form --runtime-network` binaries ran a new Static source with fixed K/D.

- Satisfy `01M3A7F3D8RK904SDC245QEM62`.
- Attempt `01M3A7F3DHBHFFT2XN6KE70CWY`, Runtime `rt_3a_local`, environment `native`.
- Common attempt executed the candidate and observed actual loopback HTTP.
- Durable finish, receipt `fully_satisfied=true`.
- Coordinator accepted through bundled Rust WASM and persisted one VerifiedRoute.
- Actual Requester independently accepted the receipt and exited successfully.
- Worker completed its one-attempt limit and local Coordinator was stopped.

Browser receipt validation is covered by native/Worker fixtures; this run did
not launch a real Browser judge. Production or Linux worker measurements were
not performed for 3a.

## Measurements (local Apple Silicon)

Reproduce in API: `node scripts/measure-receipt-authority.mjs`.

| Measurement | Value |
|---|---:|
| WASM artifact | 385,425 bytes |
| gzip | 130,945 bytes |
| Browser PASS input | 3,015 bytes |
| Actual WASM linear memory after call | 1,179,648 bytes |
| Enforced linear memory ceiling | 67,108,864 bytes |
| Node reference CPU, 1,000 calls | 533.5 microseconds/call |
| Node reference wall, fresh instance/call | 0.232 ms/call |
| Local Worker HTTP median / p95, 100 calls | 3.07 / 4.36 ms |

The module imports nothing. Linear memory excludes JS/Worker heap. Node CPU is
a reference measurement, not billed Cloudflare CPU; local Worker latency also
includes HTTP dispatch. Production resource metering remains a canary concern,
not a claim established by this local benchmark.
