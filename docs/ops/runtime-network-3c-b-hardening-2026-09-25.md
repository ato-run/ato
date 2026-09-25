# 3c-b final archive hardening — 2026-09-25

Implemented and Linux-validated for re-review; unmerged, undeployed.
[ato #1403](https://github.com/ato-run/ato/pull/1403), paired with
[API #691](https://github.com/ato-run/ato-api/pull/691); integration record
[#1404](https://github.com/ato-run/ato/pull/1404).

Baseline: `2a5d0bf935351489f706ae1e70bd53ecbc33dcb1`.
Fix: `9fad7cdcad971b388b2cdbcf063ba4e63abdb14a`; later commits only record
this evidence. Locked tar remains **0.4.46**, with no dependency update.
API executable code remains `b738af62e2052963f75e44e5b95d78361b0b4d87`;
API head `2bf5d54288715bb7f118aed7fee12b24e932496d` adds explanations only.
The historical [3c-b record](runtime-network-3c-b-2026-09-25.md) and its
identities/results remain unchanged, except for a link to this follow-up.

## First reproduce, then fix

Two regression tests were added and run against the baseline implementation
before editing production code (`cargo test --locked -p ato-formation pax_size_
-- --nocapture`). Both archives have correct SHA-256, tar headers/checksums
and physical payload; they are not digest-corruption tests.

| Case | Baseline result | Fixed result |
|---|---|---|
| A: regular size 0, PAX size 65536, actual 64 KiB zeros, expanded cap 1 KiB | Accepted; materialized **65536**, reported **0** | Typed `source_unsupported_entry` / `PAX size`, before payload/materialization |
| B: A followed by a 65537-byte GNU longname | Raw scan ended in zero payload; cooked iteration reached `max_path_bytes` after reading the oversized metadata | Same typed boundary refusal before payload/hidden metadata reads |

The exact baseline test diagnostics are retained in the adjacent evidence JSON.
A reader that panics beyond the 526-byte header/PAX prefix proves rejection
before any file payload or later metadata can be read. Separate header-only
readers prove GNU L/K and PAX x/g bodies above 64 KiB are refused before their
body is read. Merely failing a final path length check is not the criterion.

## Safety and compatibility

The bounded preflight refuses every PAX `size` key and GNU sparse entries
before advancing raw boundaries. This is option B: a documented transport
restriction, not a new tar parser. Invalid PAX records and malformed header
sizes fail closed. Ordinary PAX path/linkpath/mtime/uid/gid/codeload comment
remain supported within existing metadata/path bounds. See ADR-032.

Both memory/file measurement and expansion run this same preflight before any
cooked iterator. For permitted entries, raw/cooked boundaries agree. The file
budget uses effective `entry.size()` and actual 64 KiB read/write chunks, not
`header().size().unwrap_or(0)` or decompression framing allowance. Truncation
is an error. Extraction uses the same checked copy as measurement; ordinary
mode bits, mtime, unlink-before-overwrite and symlink/path containment remain.
No whole-archive Vec is introduced into the Network path.

Normal tar/gzip/zstd memory/file v1/v2 closure equality passes. Existing v1/v2
identity tests and memory/file closure/K/D parity pass. Normal PAX yields the
same tree identity as the equivalent plain tar; reported and written bytes
match, including executable mode and mtime checks. The historical single-file
64/128 MiB sources retain the exact same closure, K and D in this rerun.

## Real object transport: unsafe archives

Ubuntu ARM64, existing isolated `3c-branch/{src,target,scratch}`; fresh local
Miniflare D1/R2/token under `3c-api/hardening`, loopback port 19441. This uses
real API routes/auth/R2/finalization/claim/fence and compiled Rust receipt
authority, not mocked storage or simulated size. The new Coordinator's initial
Wasm relative-path setup failure was corrected before acceptance.

A normally planned route contains an exec step that prints `BUILD_STARTED`
and writes `build-started.marker`; only its source transport reference is
replaced. Each archive was actually uploaded and finalized to ready. Final
binary validation repeated finalization/acquisition of those immutable bytes.

| Case | Verified archive bytes | Object integrity | Runtime outcome |
|---|---:|---|---|
| A | 68,096 | ready, actual size/SHA-256 verified | ticket_unplannable; PAX size |
| B | 135,168 | ready, actual size/SHA-256 verified | ticket_unplannable; PAX size |

Both have execution_started=false, attempt_record=not_started, no formation
attempt, no build marker/output, no VerifiedRoute, zero expanded/stored charge
and no retained Runtime work files. Transfer remains charged for the actual
claimed download. Thus expanded usage **0** agrees with actual materialization
**0**, unlike the baseline. The same object cannot become a reusable unverified
tree. Independent acceptance searches are not UNKNOWN-resolution retries.

The file-source regression also supplies the malicious bytes after an earlier
Started record: the result stays StartedUnfinished. A later attempt in that
request stays BlockedByUnknown. Existing history-unavailable/finished/source
failure tests pass; no uncertainty is downgraded or automatically retried.

## Separate normal large-source rerun

Existing random single-file sources, serving only their small `public/` tree;
actual raw archive bytes are transferred, with no compressed padding or fake
size. Both run through attempt -> PASS receipt -> receipt authority -> one
VerifiedRoute -> requester acceptance.

| Payload | Archive bytes | Actual source / expanded charge | Requester peak RSS KiB | Runtime peak RSS KiB |
|---|---:|---:|---:|---:|
| 64 MiB | 67,113,984 | 67,109,528 | 15,524 | 16,472 |
| 128 MiB | 134,222,848 | 134,218,392 | 15,564 | 16,608 |

GNU `/usr/bin/time -v`, fresh requester/runtime processes. The physical sum of
source file sizes equals charged expansion. Doubling the archive adds 136 KiB
Runtime peak RSS, not another archive-sized allocation. Stored artifacts remain
50,509 bytes. API process memory was measured in the historical record and was
not remeasured per size here. 128 MiB remains local acceptance, not a claim
about deployed Cloudflare plan ingress limits.

## Checks and cleanup

- Linux and macOS Formation: **183 passed**, including **41 source tests**.
- Linux Runtime Network: **30 passed**; one manual request-export helper ignored
  by default and explicitly executed for the real API fixture.
- Existing wire fixture bytes unchanged and identical between repos.
- fmt, targeted clippy (formation/worker, all targets, warnings denied), and
  arch-check (**43 packages**) pass.
- Dedicated Coordinator stopped and its temporary token removed. Runtime and
  requester work directories, plus dedicated Rust scratch, are empty. D1/R2,
  sources, outputs and logs remain for inspection; no shared toolchain cleanup.

[Machine-readable proof](runtime-network-3c-b-hardening-2026-09-25.json) contains
actual attempt/receipt/budget results, identity comparison, source/binary hashes
and cleanup counts. Hosted/Chrome/Python/Node/P0 results from the prior integrated
record were not rerun by this archive-only follow-up. No past success or identity
was rewritten. No claim of 3d saved-object replay is made.

Merge order remains #1399 -> #1401 -> API #691 -> ato #1403 -> #1404. #1399
is still protection-blocked; no admin bypass or automatic merge was used.
All new commits use `[skip ci]`. No CI rerun, workflow change, deployment,
remote migration or environment flag change occurred.
