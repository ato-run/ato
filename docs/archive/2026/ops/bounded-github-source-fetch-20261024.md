# Bounded source acquisition verification — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

Branch: `feat/bounded-github-source-fetch`.
Base: `feat/formation-repository-automation`,
`2f6ad830bdd950773526ae589e054c314ad618cb` (Draft PR #1510).

The prior `PinnedSourceFetcher` used `response.bytes()` before source limits
were applied. The job now streams compressed bytes into an owned anonymous
file and reuses `FileVerifiedArchive`; the same monotonic deadline also guards
verification and materialization. No new dependency, API schema, migration,
semantic identity, or environment flag is introduced. See the
[transport contract](../../../rfcs/draft/BOUNDED_FORMATION_SOURCE_TRANSFER.md).

## Local validation

Executed from this branch's isolated worktree/target on macOS arm64 with
Rust 1.96.0, two build jobs and incremental compilation disabled:

- `cargo test -p ato-formation-worker --lib job::source_fetch::tests`: 12 passed.
- `cargo test -p ato-formation-worker --lib --test hosted_attempt_v2 --test source_symlinks_v1`:
  113 library tests, 5 hosted-attempt tests and 6 source-symlink tests passed.
- `cargo clippy -p ato-formation-worker --all-targets -- -D warnings`: passed.

The HTTP tests use real TCP sockets on loopback. They include a 1 GiB declared
body rejected from its header while the server withholds all body bytes,
4 MiB chunked/unknown-length streams rejected at a 4096-byte configured cap,
all redirect statuses refused without contacting their destination, and a
70 ms frozen deadline over header stalls/body stalls/slow-drip chunks.
The infinite-reader fixture consumes exactly 129 bytes with a 128-byte cap,
writes at most 128, and never requests a buffer larger than 64 KiB.
Anonymous scratch files are absent after overflow, timeout, redirect, success
and archive-verifier handle drop. A gzip fixture preserves the previous
tree digest, resolver contract, closure digest and subdirectory materialization.

Logs are retained in the owned worktree's `.tmp/source-fetch-tests-final.log`,
`.tmp/worker-regression.log` and `.tmp/source-fetch-clippy.log`.
Source symlink tests may return early where the required platform sandbox is
unavailable; their macOS result does not establish Linux sandbox acceptance.

## Environment status

No staging or production deployment, live external GitHub transfer, GPU
allocation, production key change, or feature enablement was performed.
The production streaming transport is tested with HTTP fixtures; operational
GitHub/staging acceptance remains separate. No hosted app runtime capability
is advertised by this change.
