# Runner empty long-poll pacing

## Deployed path and defect

On 2026-10-09, staging Runner `01M0YQGVA0Q8YDXEH3VG0ZZBPV`
(`ubuntu-sugamo-staging-hosted`) had six active slot services. All six ran
`ato-connected-realization-worker-119400ea`, from
`119400eaeb9727784955e0e5d5744453f0d8b90a` (binary SHA-256
`6df3dc1f30beb180a67736f6d679611c20426bb6ef93ff5588ea4d00192a4e1b`).

That version and current main request `/leases/next?wait_ms=20000` but sleep
`next_poll_seconds` again after an empty response. Staging returns 5 seconds,
so every completed empty long poll leaves a slot unavailable for another
5 seconds plus the next heartbeat. The API already wakes pending polls when
work arrives; no API wire change is needed.

The fix measures claim elapsed time with a monotonic clock and sleeps only
the remaining bounded interval. Immediate capacity/drain/legacy-server empty
responses retain pacing; the existing 1–30 second bounds, transient-error
backoff, permanent-error handling, one-shot behavior, and recovery gates remain.

## Local evidence

All temporary output and test roots are inside this worktree's `.tmp/`.
The fake control plane returns no lease and never launches a workload.

| Actual worker loop, loopback API | Before | After |
| --- | ---: | ---: |
| Empty response delayed 1.2 s, next interval 1 s: reply to next claim | 1,010 ms | 2 ms |
| Immediate empty response: claim-to-claim interval | 1,014 ms | 1,012 ms |
| `--once`, empty response | 1 claim, exit 0 | 1 claim, exit 0 |

These are single controlled regression samples, not application startup p95.
The second continuous-loop claim returns HTTP 401 and must terminate the worker.
The manual harness excludes external runtime binaries from its PATH so recovery
and capability probes cannot contact the developer's Docker daemon.

Validation on macOS, Rust 1.96.0:

- New timing tests failed against the original delay policy, then passed.
- `cargo test -p ato-connected-realization-worker --lib --locked`: 162 passed,
  3 existing ignored tests.
- `cargo clippy -p ato-connected-realization-worker --all-targets --all-features --locked -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- `cargo build -p ato-connected-realization-worker --locked`: passed.
- `python3 tests/manual/runner_polling.py target/debug/ato-connected-realization-worker`: passed.

Build/test commands used `TMPDIR="$PWD/.tmp/tmp"`,
`CARGO_PROFILE_DEV_DEBUG=0`, and `CARGO_PROFILE_TEST_DEBUG=0` where applicable.

## Linux and CI evidence

The deployment was built from `eb944f68e19b501f1eb42d1518487637f98d13fb`:
the existing staging source `119400ea` plus this polling change, without the
unrelated changes on main. Linux Rust 1.96.0 release build and package Clippy
(all targets/features, warnings denied) passed. The actual release executable
passed the manual loop test: 3 ms after the delayed empty response, 1,002 ms
between immediate-empty claims, and one successful claim with `--once`.

The deployed-source library suite passed serially: **153 passed, 3 existing
ignored**, using `--test-threads=1`, a fresh temporary root, and
`ATO_TEST_WORKER_BIN` pointing to the release binary. Parallel runs were not
reliably green: the first hit the SSH shell's 1,024-file limit; after increasing
only the test shell limit to 8,192 and setting the worker binary, later runs
each failed one existing lock test (`only_one_worker_can_hold_the_same_slot`
or `a_delete_removes_only_its_own_volume`). A same-configuration baseline run
also failed, but on a different loopback-CDP timeout. That does not prove the
lock failures existed on the baseline; their cause remains unconfirmed. No
tests were skipped or changed to conceal these failures.

The PR code commit `71068fa8515a5b8240fc096d56719a06b19834b7` passed the
complete Rust CI on Linux, macOS, and Windows:
[run 37866269809](https://github.com/ato-run/ato/actions/runs/37866269809).
The staging integration uses the older deployment baseline, so its test
counts differ from this main-based PR.

## Staging rollout

Following `runner-rollout-and-rollback.md`, scheduling was drained at
`2026-10-09T00:56:32.656Z`. Preflight confirmed no open leases, Runner-labelled
containers, journal files, or lease directories. All six slot services then
switched to `/usr/local/bin/ato-connected-realization-worker-eb944f68`:

- Source: `eb944f68e19b501f1eb42d1518487637f98d13fb`.
- Binary SHA-256: `3e4084608b026cbd7f603088373235441ad683aa6a0577ef43e8ae66efada3f0`.
- Per-service drop-in: `zzzzzzz-idle-poll-eb944f68.conf`, containing only the
  existing effective command with its binary path replaced.
- The previous versioned binary and `zzzzzz-recovery-119400ea.conf` remain
  available for rollback. Rollback requires the same drain and empty checks
  before removing the new drop-ins and restarting.

Scheduling was re-enabled after all six services were active and the expected
heartbeat capabilities were present, including `runtime_launch`, OCI service
groups, and Runner persistent volumes. At 01:19 UTC, all six still ran the new
binary with zero restarts and no recovery-blocked or control-plane error
messages. Scheduling was enabled and there were zero open workloads,
Runner-labelled containers, journal files, or lease directories.

There were no feature-flag, migration, credential, persistent-data, Formation,
or production changes in this rollout.

## Hosted smoke results

Three existing Library applications were started through the staging PWA and
stopped through the admin UI's normal graceful-stop action. Their existing
volumes were preserved; no setup, note, or test-data writes were submitted.

| Fixture | Lease | Created → claimed | Created → ready | Stop request → stopped |
| --- | --- | ---: | ---: | ---: |
| OCI service group | `01M4F2XBRKBB3C8CANGY116XEY` | 136 ms | 2,368 ms | 1,570 ms |
| OCI persistent volume proof (two-service group) | `01M4F34QH6AB5DMH6XJ9DH3XEB` | 176 ms | 2,653 ms | 1,485 ms |
| OCI service group, repeat | `01M4F3HHR17R6F6S7R1M5E86D2` | 112 ms | 1,715 ms | 1,733 ms |
| Trilium Notes (single OCI) | `01M4F3PHPGWSN7223XFRHH1FKJ` | 126 ms | 3,605 ms | 2,924 ms |

The persistent-volume fixture rendered its endpoint instructions in the PWA.
Its launch spec and worker logs identify **two services**, despite the initial
report incorrectly counting it as a single-OCI smoke. The separate Trilium
launch spec has `realization.kind = "oci"`; Chrome rendered its language setup
screen at the direct Surface URL, while PWA embedding was refused. Setup was
not advanced.

The whoami/nginx service-group fixture reached backend readiness but Chrome
displayed `ERR_BLOCKED_BY_CLIENT` both directly and inside the PWA. DevTools
showed an HTTP **200**, `Content-Type: text/plain; charset=utf-8`, followed by
`(blocked:devtools)` for the document. DevTools request blocking was already
disabled; no blocking or security settings were changed. The origin of the
client-side interception remains unconfirmed. The successful volume fixture
shows that service-group rendering is not universally broken.

All four leases finished `stopped`, with no error code. Worker stop evidence:
Trilium's container and both volume-fixture services reported graceful exit 0;
the whoami group in slots 6 and 5 reported graceful web exit 0 and backend exit
2, with teardown confirmed. Final host inspection found no labelled containers,
journal files, or lease directories. The two refused embedding paths remain
unresolved and are not counted as successful PWA browser acceptance.

These are individual warm-image, stopped-workload starts, not application
interaction p95 or Contract verification receipts. They do not meet the
one-second end-to-end goal.

## Remaining limitations

The last 60 historical claimed leases had median created-to-claimed 176 ms
(range 90–581 ms). This is not end-to-end startup latency and does not establish
that the additive sleep dominates every request: six staggered slots can mask
it. Actual cold/warm Hosted startup and user-interaction p95 still need measurement.

The fix branch is based on `origin/main` (`db59a58dd`) because the
deployed worker belongs to that history and `origin/dev` has no connected worker
package. No main merge or production rollout is implied.

## Merge-time integration validation

At the user's October 9 request, integrated main `ff5265485544b512f5aba86488fac5c717cd20f1`
into this PR without conflicts. On the combined code, formatting, worker-library
tests (148 passed, 3 existing ignored, serial execution), all-target/all-feature
package Clippy and worker build passed. The real worker/fake HTTP control-plane
regression measured a 2 ms post-long-poll idle gap, a 1,010 ms immediate-empty
interval, and one claim in one-shot mode. Each case used a fresh local work root.

The smaller package test count reflects main moving data-plane tests into the
shared runtime crate; it is not a skipped-test change in this PR. Earlier
platform CI is retained as historical evidence, not claimed for this integration.
Merge uses `[skip ci]` to avoid push-triggered publication; production deployment
and the remaining end-to-end performance acceptance remain separate.

### Full-CI integration findings and test repair

The first integrated CI run `37901492674` failed Linux Formation assertions
that still expected the old readiness error wording. Tests now check the shared
`StartupTimeout` type and `StartupProcessExited` type plus exit code 3, retaining
output, duration and process-cleanup assertions. The same CI run exposed a macOS
WouldBlock panic in the new preparation HTTP fixture. Both local HTTP fixtures
now explicitly make accepted streams blocking while keeping bounded read
timeouts and nonblocking listener shutdown. These changes are test-only; no
production error handling, timeout or socket policy changed.

Local repair validation: formatting passed; runtime-attempt library 161 passed;
temporary-realization suite 14 passed; Clippy for runtime-attempt and
formation-worker with all targets/features passed. This Mac's temporary
realization suite uses its existing containment-refusal path; Linux startup and
cleanup execution still need the new CI run. No test was skipped or weakened.
