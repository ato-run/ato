# Native build evidence retention before runnable workspace projection

Code pin: `3ac36fa2739cf8d9555026e93a4ca79905a6d4a7`.
API counterpart: `fa825a176b715962351466dc1a7149bec5979c7f` (requires migration 0315).
Authority WASM: ABI 1, Rust 1.96.0, SHA-256
`cc7445874bcea354b0ea03d96a71e10ea4e34b80b5516111457f1bdc92af3c03`, 2,196,846 bytes,
built from the code pin above. New PR heads may add this work record only;
application/authority code remains pinned.

The prior changedetection.io observations remain preserved: source-owned dependency
resolution and isolated sdist wheel construction reached the unchanged HTTP K,
but the complete workspace exceeded retained expanded/stored transport limits.
This change does not raise those limits, change the app's dependencies or source,
or silently remove build evidence. It retains all registered original acquisition
inputs, completed wheels, backend wheels, hash locks, package versions and toolchain
provenance in an immutable owner-private `ato.build-record/1` manifest/chunk set.
Record is evidence, not Capsule/Contract/Derivation identity or a Run permission.

Only the exact registered generated operation/plan owns the helper roots. The
collector validates file hashes, declared bytes, locks, source requirements, backend
versions, toolchain versions, network/isolation claims and the complete output set.
Ready is accepted only after authenticated storage acknowledgement. The built tree
stays unchanged; packing projects those acknowledged exact helper roots out of the
runnable archive while preserving source and installed dependencies. Mutation,
unregistered output and runtime links into omitted evidence fail closed. Default
packing and retained-candidate/1 / portable wire schemas remain unchanged.

A new Coordinator ticket explicitly declares the evidence schema it supports.
Historical tickets do not enable this path. The original attempt/fence, frozen K/D,
source, budgets, deadlines and retry limits stay immutable. Before evidence upload,
the Runtime checkpoints its confirmed finished/verified journal identity and
value-free publication provenance. Restart resumes reporting/storage only; it never
runs source acquisition, build, inference or launch from that checkpoint. Consumed
RPC retries remain consumed. Already-finished execution with no recoverable report
is conservatively charged its original resource reservation rather than refunded.
UNKNOWN history still fences execution; the old WBO UNKNOWN has not been rerun.

## Validation at the code pin

- Formation library 122 tests and exploration integration 33 tests: PASS.
- Worker library 73 tests: PASS, including registered evidence preservation,
  mutation/unknown output/symlink rejection, frozen publication retries and refusal
  to resume publication from unconfirmed/UNKNOWN execution.
- Runtime Network 31 PASS, 1 explicitly ignored platform-dependent case; authority
  library 7 PASS and authority integration 8 PASS.
- Process transport 2 PASS and retained replay 3 reported PASS. On this macOS host,
  the existing process containment guard skips actual Python/Node realization;
  this is not Linux process acceptance. Static replay is executed normally.
- Rust formatting and changed-package clippy with `-D warnings`: PASS.
- Linux CLI/worker build from the same code pin: PASS. Real pinned zero-D OSS
  trials, retained fresh same-K replay and functional/persistence gates follow.
- Counterpart actual WASM + real isolated D1/R2/Coordinator distinct tests: 201
  completed after correcting one fixture's other-Runtime expectation to the existing
  404 ownership guard; migration/wire 15 PASS; typecheck and schema bootstrap PASS.

No additional paid provider calls, staging/production deployment, remote migration,
feature flag change, normal Run grant or publication was performed. Isolated local
SQLite/D1 acceptance initialization is separate from remote migration. The approved
small API ceiling remains 24 calls; no 100-app allocation or rerun is implied.
