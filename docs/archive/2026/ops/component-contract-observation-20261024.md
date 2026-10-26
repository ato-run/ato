# Component Contract observation implementation — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

Dependent on the explicit service-dependency authoring change (ato#1504).
The independent Linux source-OCI compiler fix ato#1505 is required when building
the CLI on Linux. Neither change alters an existing K or D.

## Local verification

- Targeted Cargo check of portable application, CLI and connected worker: PASS.
- The same three crates with `--all-targets`: PASS.
- Portable application library: 105 tests PASS, including byte-identity fixtures,
  static/process/OCI routes, independent imports and state/snapshot regression.
- Added component Contract test: missing component, wrong status and wrong Port
  all fail the whole K; complete evidence passes; unknown Port is refused.
- Added real-socket Runner test: exact signed Port/path is required, wrong Port
  and path/injection are refused, and no proof reaches the private upstream.

The local Mac was heavily swapping (16GiB RAM); elapsed build/test time is not
runtime performance evidence. API D1 observer tests and Linux OCI tests are
recorded below once complete. No production deploy, paid GPU or operator key
use is involved in these tests.

## Native Linux and real Docker acceptance

Linux isolated checkout combined this PR with the independent #1505 source-OCI
compiler correction. Portable library 105/105 PASS. The connected worker library
initially exhausted the host's 1024-descriptor shell limit under default test
parallelism. Three subsequent session fixture failures were test-environment
problems: the library test executable was used as the worker shim, and a long
temporary path exceeded the Unix socket path limit. With a process-local limit
8192, two test threads, `ATO_TEST_WORKER_BIN` pointing to the actual connected
worker binary and a fresh short temporary root under the workspace `.tmp`, the
integrated Runner suite passed: **148 pass, 0 fail, 3 existing ignored** in 19.20s.
The tested integration SHA is `2d31db6bfd6502a5bbf7069359a0dce61e789d7a`;
no source skip or implementation change was needed to resolve these failures.
The new private-observation real-socket test passes on both hosts.

Actual two-container execution used an already-cached pinned Python image,
non-root UID:GID 1000:1000, an internal Docker network and fresh CLI environments.
No image build/pull, host service changes or paid infrastructure was needed.

| Case | Time to verify and stop | Exit | Whole K | Components removed |
|---|---:|---:|---|---|
| Backend + frontend + real sibling call healthy | 23,275ms | 0 | fully_satisfied=true | both confirmed gone |
| Backend unhealthy; frontend `/` still 200 | 23,545ms | 1 | fully_satisfied=false | both confirmed gone |

Both use ContractRef
`sha256:084a5af45d5761ec2279261b745706459b151bff9786a43d0947f17e4baa7c73`.
The failed receipt has failed backend-health and sibling-connection observations
and a satisfied web-health observation. Expected K was never rewritten.
Receipts and the measured summary are in `evidence/component-contract-20261024/`.
Times are one execution each, not a statistical performance claim or Hosted
staging measurement.

This experiment exposed an existing CLI output gap: failed receipts persisted
in the private attempt directory but were not copied to the explicit
`--verification-receipt` target. The CLI now copies the exact recorded bytes on
that failure path and retains the original nonzero result. The repeated real
negative run verifies that behavior. Existing cleanup and receipt immutability
remain owned by the common attempt.

## Failure receipt regression follow-up

GitHub CLI CI (macOS/Linux) caught an old test expecting no receipt even when
`--verification-receipt` was explicitly requested. The test now requires the
existing false verdict, exact K/D, failed root observation/digest reason, a
closed execution endpoint and the finished failed attempt record, while CLI
exit remains nonzero. Admission failures still must not emit receipts. The
full 16-test portable CLI integration suite passes locally on Python 3.12.8.
This is an expectation update for the existing explicit evidence export, not
a relaxation of Run acceptance or a successful receipt for failed K.
