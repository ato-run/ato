# Formation source measurement evidence — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

Branch: `feat/formation-source-measured-receipt`.
Base: Draft Rust #1517, `feat/bounded-github-source-fetch`,
`e226642544195d59e4b1f391bd4b7a8c96783d86`.

The existing archive verifier's actual regular-file byte count and safe-entry
count now reach the current worker's actual `source_evidence` result. The same
file-verified SHA-256/tree/resolver and successfully selected subdirectory are
captured before any build changes the source. Job ID must match the reserved
attempt before acquisition; a declared expected archive digest is now checked
against the actual file rather than using only the acquired digest as expectation.
No extra fetch, archive-sized allocation, storage upload, receipt schema, dependency
or sidecar was introduced. K/D/closure algorithms and old golden bytes are intact.

See [contract and detected mismatch](../../../rfcs/draft/FORMATION_SOURCE_MEASUREMENTS.md).
The removed legacy snapshot SourceMaterializationReceipt producer is not restored.
Actual `ato.source-resolver.v1/v2` measurements are never aliased to the historical
`ato.capsule-program-source-projection/v1` identity. The receiving API must accept
these current resolvers and paired metrics before this worker is deployed.

## Verification

All commands use the owned worktree's isolated Cargo target and `.tmp` TMPDIR,
macOS arm64/Rust 1.96.0, two jobs, debug info/incremental disabled.

- `cargo test -p ato-formation --lib --tests`: 449 passed across 21 test binaries,
  including existing identity/receipt/golden regressions. The added fixture checks
  tar/gzip/zstd, directory/wrapper exclusion, contained symlink/empty-file counting,
  full-tree counters despite subtree selection, wrong archive digest and file cap.
- `cargo test -p ato-formation-worker --lib --test hosted_attempt_v2`: library 113,
  Hosted 8 passed. All new negative fixtures passed on their first execution.
- Previous complete Worker run also passed source-symlink 6. Total distinct
  Formation/Worker/source regression cases: 576.
- Two-crate all-target clippy with `-D warnings`: passed. Formation no-default
  library check, changed-file rustfmt and `git diff --check`: passed.

Hosted tests run the actual common executor, Static Web loopback HTTP observation,
Contract verification and result publication against a simulated artifact API.
They assert the source evidence in that HTTP result, the same job/attempt/fence,
execution receipt correspondence, untouched K/receipt, full archive metrics for a
selected app, rejection before fetch on wrong assignment, and no publication after
archive/tree/selection mismatch or a failed application Contract.
This is distinct from live GitHub acquisition or a deployed API acceptance.
Mac tests that require Linux containment may return early; their passing result
does not establish Linux sandbox acceptance.

Logs: `.tmp/formation-regression.log`, `.tmp/worker-final.log`,
`.tmp/worker-regression.log`, `.tmp/source-evidence-clippy.log`.

## Remaining operational boundary

No staging/production deployment, flag change, GPU request or production key action
was performed. No raw codeload archive was uploaded to `source-archives/`; the
result is actual source evidence, not a persisted source materialization. API
materialization must stay null until real storage is confirmed. Source-only
execution through existing source-materialization queue/claim/upload/report
authorization and truthful format/retrieval verification is the next separate
slice. This PR does not claim first-GitHub-archive promotion is executable today.
