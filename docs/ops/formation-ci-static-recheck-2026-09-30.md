# Formation #1449 — final reviewed head CI recheck

The additional Ubuntu failure is **base-reproduced and intermittent in the
isolated observations**. Reviewed head
`06330897500092555c64cf143b241fbf93770a8d` produced FAIL / PASS / PASS.
Exact base `b43eaa0c55be0229d55b8d82054d355b616d04b1` produced PASS from
the shared identical-code Cargo cache, then FAIL after independently compiling
the tested package from the base directory. Both failures are at
`apps/portable-application/src/lib.rs:3566:43`, with OS error 104,
`ConnectionReset`, `Connection reset by peer`. This does not establish the
timing mechanism or fix the test. It is not a stable head-only failure.

## Full CI remains red

Reviewed head Rust CI [36658258823](https://github.com/ato-run/ato/actions/runs/36658258823)
completed with failures: Windows Unix API compilation; macOS process ownership
and five hosted-validator tests; Ubuntu
`hosted_python_and_node_use_the_common_process_runtime` and
`local_static_server_rejects_browser_state_without_the_run_token`.
These are executed failures, not billing failures.

Exact base Rust CI [36654141449](https://github.com/ato-run/ato/actions/runs/36654141449)
also failed the static-state-token test at the same source location with the
same OS error. This full-CI evidence is separate from the isolated reruns.
The earlier initial-head / parent comparisons and their unconfirmed failures
remain as recorded in the [current support ledger](formation-current-support-2026-09-30.md).
No full workspace rerun or Rust fix was made for this docs PR.

## Isolated runs

Host: sugamo, Ubuntu 26.04 LTS, Linux x86_64, kernel `7.0.0-27-generic`,
rustc/Cargo stable `1.96.1`. This is not the GitHub-hosted runner image or its
exact compiler build. Source, manifests, Cargo configuration and sample
fixtures were exported from the two exact Git commits; their exported Git
tree entries are byte-identical. Historical docs/screenshots and root scripts
were excluded from the test export. No source was modified.

Each invocation had fresh `TMPDIR` and `ATO_HOME`. Cargo used `--offline
--locked`, two build jobs and a dedicated task target directory. No model
credential was read and no dependency was fetched.

```sh
cargo +stable test --offline --locked -p ato-portable-application --lib \
  tests::local_static_server_rejects_browser_state_without_the_run_token \
  -- --exact --nocapture --test-threads=1
```

| Pin | Run | Result | Build / observation |
|---|---|---|---|
| Head `06330897` | 1 | FAIL, exit 101 | Fresh package compilation; same ConnectionReset as CI |
| Head `06330897` | 2 | PASS, exit 0 | Cached package, fresh test environment |
| Head `06330897` | 3 | PASS, exit 0 | Cached package, fresh test environment |
| Base `b43eaa0c` | 1 | PASS, exit 0 | Cargo reused the identical-code executable; not an independent base compilation |
| Base `b43eaa0c` | 2 | FAIL, exit 101 | Package-only clean in the dedicated target, independent compilation from exact base |

The second base invocation resolves the first base invocation's cache
provenance limitation. All five invocations actually ran exactly one selected
test. No retry was added to the test or workflow. A preliminary archive
transfer/extraction error stopped before any test ran and is excluded from
these counts.

The [JSON ledger](formation-ci-static-recheck-2026-09-30.json) records exact
pins, export hashes, commands, results, environment and evidence hashes.
[Raw evidence](evidence/formation-ci-static-recheck-20260930/) includes all
isolated logs, both full-CI run metadata records and bounded static-test log
excerpts. The export and test drivers are retained for provenance.
Raw logs retain their original whitespace and hashes. Text whitespace checks
exclude those byte-preserved `.log` artifacts; JSON, Python syntax, evidence
hashes and relative document references were checked separately.

## Merge boundary

The reviewed head changes no Rust, Cargo, workflow or sample files relative to
the exact base. The follow-up change adds only this CI evidence and its current
support reference. Historical Formation results and functional acceptance are
unchanged. The user's head-only-stable-failure stop condition was not observed;
the same failure is reproduced on the exact base, including an independently
compiled isolated run.

The user authorized merging this lineage after the recheck, including an admin
merge if required after confirming the new exact head. Branch protection
settings remain unchanged. Full CI remains red; an authorized admin merge is
not reported as CI passing. No deploy, remote migration, model call or #1421
change is included. The 100-app wave remains work for the next chat.
