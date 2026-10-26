# Local Formation source capture exclusions

The accepted `PROTOCOL_ADAPTER.md` requires the shared default credential
exclusions at workspace capture boundaries. Local Formation's in-memory and
file-backed source snapshots previously skipped only `.git`, so a selected
directory's `.env`, `credentials.json`, private keys or `.capsule` objects could
be archived before the source verifier or a build ran.

Both local snapshot callers now use the existing
`ato_adapter_api::WorkspaceCapturePolicy::secure_default()` in their common
directory collector. Excluded files, directories and links are skipped before
metadata/content reads. This reuses the existing rule set; no separate denylist,
secret detector or user override is introduced. The existing crate is added as
a direct Worker dependency; Cargo.lock changes only that dependency edge.

The captured tree is measured after filtering by the unchanged v1/v2 source
resolver. A local directory containing excluded paths therefore has different
snapshot bytes/archive SHA/tree/closure from its previous unsafe capture. This
is the actual newly captured input, not a claim that the old tree has the same
identity. A clean directory keeps its previous exact archive bytes; a regression
pins the parent capture digest. Excluded credential value changes do not alter
the safe tree. Normal contained symlinks retain resolver v2 behavior.

This policy applies while capturing a mutable local directory. It does not
filter or rewrite an already-pinned archive: GitHub raw transport, commit,
archive digest, measured tree and SourceEvidence keep the existing verification
path. A regression verifies a raw wrapped archive with a public `.env` canary
unchanged and measures both files, independently from local directory capture.
Transport verification/quarantine is not a recapture or a secret-scrubbing
claim. No content-addressed input is relabeled to conceal a changed tree.

Public canaries exercise both snapshot APIs, materialization, excluded links,
clean byte compatibility and an actual local Static Formation result/artifact.
The previous code fails three new capture regressions (credential bytes,
excluded link, secret-dependent snapshot digest). No real secret is used.

The rule set is filename/path based. The accepted RFC explicitly does not claim
that arbitrary secret-like names can be detected. For example, this change does
not extend the common policy to `.npmrc`, scan arbitrary content, or guarantee
that a copied secret under an allowed filename is absent. Operator credentials
must still remain outside source/workspace/cache roots and out of workload
inputs. Explicit secret Bindings retain their separate authorization and
artifact-embedding policy.

This is a child of the controller-environment fix (`919e53eed`). It changes no
`env_clear`/sandbox/Native code, deployed binary, API, Provider, flag, secret,
lease, K, claim or raw source-only transport. The previous pinned Worker remains
unchanged until a separately coordinated installation of this child.

Validation in this isolated macOS checkout:

- Worker + Runtime tests: 553 passed, 1 existing ignored, 0 failed. Linux-only
  sandbox execution is not claimed by this platform's result.
- Final capture suite: 5 passed after additionally checking packed materialized
  artifact bytes; the actual local Formation artifact regression also passed.
- Worker + Runtime all-target Clippy `-D warnings`, workspace formatting and
  whitespace checks passed. An independent Cargo target avoided another
  checkout's cached local crates; dependencies and tests ran offline.
- Logs are under this worktree's `.tmp/capture-*.log`. The unchanged parent
  run demonstrates the three capture failures; its clean fixture SHA-256 is
  `59a0fb52dc414530475a6019e90c06d743480746d2349802b51fe94bad92a067`.

Separate Linux validation of this child completed on `ubuntu-sugamo` using
source commit `39f75a1a1d4a7f3052df95535d657285a2fdd5b8`:

- A Git archive was extracted into the new owned directory
  `/home/ekohsuke/ato-run/.tmp/formation-capture-policy-39f75a1a1`. Its compressed
  SHA-256 was `6af750c9688fea4c636d507b090f2ddd165b66158c56742fc27dc082c025d0a2`.
  All 539 Rust/Cargo inputs had their mtimes refreshed before compilation to
  prevent reuse of another checkout's local crate metadata. The sorted
  path/NUL/content-SHA-256 manifest matched this checkout:
  `87fc9ab486c0c705bc87486edb3c8c8ac742a96092377b3689a0324113a6aa40`.
- The coordinated existing Cargo target was used with two jobs, debug info and
  incremental compilation disabled, offline dependencies, and `ulimit -n
  65536`. An `env -i` process used fresh `TMPDIR`, `ATO_HOME` and exploration
  roots under `/home/ekohsuke/ato-run/.tmp/cap.UKCPpF`; no application credential
  was passed. The shared target was released after validation.
- `cargo test -p ato-formation-worker --offline --test source_capture_policy_v1
  --test local_formation_v1 --test source_symlinks_v1`: 31 passed, 0 failed
  (5 capture, 20 local Formation, 6 symlink tests).
- `cargo test -p ato-formation-worker -p ato-runtime-attempt --offline`:
  559 passed, 3 existing ignored, 0 failed across 30 test executables and two
  empty doc-test groups. This includes the controller/proc environment canary
  regression and real local Formation artifact capture. The ignored cases
  require the manual Network acceptance input, native Docker, or a built
  connected Worker respectively; they are not claimed as executed acceptance.
- `cargo clippy -p ato-formation-worker -p ato-runtime-attempt --all-targets
  --offline -- -D warnings` and `cargo fmt --all -- --check` both passed.
- The source/input manifest, fresh-root record, full logs and summary remain
  in that source directory's `.tmp/` and this checkout's `.tmp/linux-proof/`.

Only owned source/test files and the coordinated build cache were used over
SSH. No real application credential, Provider call, GPU, deployed Worker,
staging job claim, service or shared staging state was used or changed. The
parent environment fix's separate Linux regression and immutable `919e53eed`
pinned binary remain untouched, as recorded in
`formation-build-controller-environment-20261024.md`. These are child-source
tests, not installation or acceptance of a newly deployed Worker.
