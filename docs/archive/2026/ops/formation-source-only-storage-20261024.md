# Source-only archive storage implementation

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

Rust branch: `feat/source-only-archive-materialization`, stacked on PR #1519.
Matching API receiver and actual storage authority are a separate change owned by
`ato-api`; no API deployment or installed worker is claimed by this record.

Implemented: explicit source-only physical job admission, dedicated current
Formation/Runner claim and attempt-scoped control wire, bounded verified raw
archive streaming, credential-separated PUT/GET, exact readback verification,
strict terminal physical output and owned-staging cleanup. The application
executor refuses this output; no application/build/Contract/Schema path runs.

Local verification uses a real loopback HTTP authority/store fixture. It verifies
actual transferred bytes, exact attempt context, stale authority refusal,
wrong-ticket/expired-ticket/unsafe-URL refusal, corrupt/short/oversized/redirected
readback, wrong completion acknowledgment, transfer deadline and cleanup failure.
Object requests are checked for absence of the Runner credential. Package and
Python setup scripts are included as source data; no planner/build entrypoint is
called. These fixtures are not D1/R2 or staging acceptance.

Remaining before rollout: matching API routes and CAS/upload authority, generic
claim exclusion, owned pointer/owner admission consumption, immutable raw-format
conflict behavior, server recovery of reserved uploads after worker crash, and a
real receiver+worker storage/admission acceptance. The existing staging deployment
lock is untouched; no paid provider, operator key, flag or production action was
performed.

Verification on macOS arm64 / Rust 1.96, own isolated Cargo target and explicit
worktree `.tmp` as `TMPDIR`:

- `cargo test -p ato-formation-worker -p ato-formation -p ato-ipc --lib --tests`:
  53 suites, 944 passed, zero failed. Two existing ignored cases are the manual
  Linux Network hardening fixture and the fixture-digest generator.
- `cargo clippy -p ato-formation-worker -p ato-formation -p ato-ipc --all-targets -- -D warnings`: passed.
- `cargo check -p ato-formation --no-default-features --lib`: passed.
- Changed Rust sources formatted; `git diff --check`: passed.

The first HTTP fixture run exposed macOS accepted sockets retaining nonblocking
mode; the fixture explicitly sets blocking mode before its bounded reads. The
complete relevant regression command was rerun successfully after that fix.


Follow-up: the one-shot command accepts optional `--job-id`, forwarded to the
existing dedicated claim filter, so acceptance cannot take unrelated queued
source-only work. Invalid IDs and an unrelated returned assignment fail closed;
no flag preserves dedicated queue polling. The focused source-archive suite
passes all 11 tests (one additional test over the 944-test run), and Worker
all-target Clippy was rerun after this additive filter.


The final upload follow-up explicitly sets the descriptor Content-Length in
addition to the finite sized reader, matching the API signed-header constraint.
Actual HTTP PUT fixtures assert exact length and no Transfer-Encoding. All 11
source-only tests and Worker all-target Clippy pass after this change.
