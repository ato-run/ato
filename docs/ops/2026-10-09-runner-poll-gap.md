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

## Staging status and limitations

The polling fix is not deployed yet. The shared Runner had no open leases at
the read-only inspection, but rollout must still drain scheduling first and
recheck leases, labelled containers, and journals according to
`runner-rollout-and-rollback.md`. The staging admin UI currently requests
Cloudflare Access login. No drain, restart, feature-flag change, migration,
credential provisioning, or Formation has been performed for this fix.

The last 60 historical claimed leases had median created-to-claimed 176 ms
(range 90–581 ms). This is not end-to-end startup latency and does not establish
that the additive sleep dominates every request: six staggered slots can mask
it. Actual cold/warm Hosted startup and user-interaction p95 still need measurement.

The fix branch is based on current `origin/main` (`db59a58dd`) because the
deployed worker belongs to that history and `origin/dev` has no connected worker
package. No main merge or production rollout is implied. A staging binary
should contain only this fix over the currently deployed `119400ea` source.
