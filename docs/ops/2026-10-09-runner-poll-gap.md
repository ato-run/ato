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
groups, and Runner persistent volumes. At 01:10 UTC, all six still ran the new
binary with zero restarts and no recovery-blocked or control-plane error
messages. Scheduling was enabled and there were zero open workloads,
Runner-labelled containers, journal files, or lease directories.

There were no feature-flag, migration, credential, persistent-data, Formation,
or production changes in this rollout.

## Hosted smoke results

Both existing Library fixtures were started through the staging PWA and
stopped through the admin UI's normal graceful-stop action. Their existing
volumes were preserved; no application data was written.

| Fixture | Lease | Created → claimed | Created → ready | Stop request → stopped |
| --- | --- | ---: | ---: | ---: |
| OCI service group | `01M4F2XBRKBB3C8CANGY116XEY` | 136 ms | 2,368 ms | 1,570 ms |
| Single OCI | `01M4F34QH6AB5DMH6XJ9DH3XEB` | 176 ms | 2,653 ms | 1,485 ms |

The single-OCI Surface rendered its endpoint instructions in Chrome. The
service group reached backend readiness but Chrome displayed
`ERR_BLOCKED_BY_CLIENT` both directly and inside the PWA. This remains a
separate unresolved display failure, not a passed browser smoke. The final
lease statuses were `stopped`, with no error code. Host inspection confirmed
no containers or journal entries remained for either Run.

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
