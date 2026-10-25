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

No real credential, remote call/mutation, GPU, deployed Worker or shared staging
state was used or changed. The parent environment fix's separate real Linux
regression/pinned binary evidence remains recorded in
`formation-build-controller-environment-20261024.md` and is not replaced by
these local capture checks.
