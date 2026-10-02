# Formation build Record retention

Status: Draft — implementation and actual acceptance in progress.

## Problem and boundary

A registered Python sdist operation retains source-owned resolution inputs,
backend/toolchain evidence and completed hash-locked wheels. The installed
runtime workspace and these evidence files currently share one archive. The
changedetection.io observation produced a fresh frozen-K PASS but exceeded both
retained transport and expanded-tree bounds. Compression does not resolve the
duplicate representation of installed files and wheel archives.

Retain build inputs as owner-private, immutable Record evidence and project the
already-installed runtime workspace into its existing process Materialization.
Records describe observed Evolution; they do not become Capsule identity, a new
Kernel primitive, a Run grant or successful verification. Source, D, K and the
operation's offline installation remain unchanged. Existing Capsule wire formats
and the retained-candidate/1 descriptor stay unchanged.

## Registered evidence and projection

Rust recognizes the exact registered Python builtin operation, validates its
plan and identifies its Ato-owned `python-build-N` output root. Exclusive root
creation excludes a pre-existing source root. Arbitrary directories, user
commands and filename heuristics are not retention recipes.

After completed execution, collect every regular file in each registered root,
including original sdists, backend wheels, completed wheels, lockfiles and
provenance. Reject symlinks, missing inputs, path escapes, inconsistent wheel
hash/size/version lineage and byte/count limit violations. The existing Runtime
embedding guard remains in force over the full built tree. Values and host
working-copy paths do not enter a Record manifest, D, model input or public trace.

The strict manifest names the original attempt, D, frozen K and source closure,
registered roots, logical relative paths, file identities, ordered content
chunks and bytes. Canonical JCS and SHA-256 are Rust-owned evidence identities.
API validation invokes that same authority with trusted assignment values;
TypeScript handles storage/ownership, not K or D canonicalization.

Only an acknowledged ready Record permits runtime archive projection to omit
its recorded output roots. Source and installed runtime files remain. The
original built tree is never deleted or rewritten. The retained row associates
the immutable Record reference with its creation attempt; owner inspection can
retrieve all evidence independently of Runtime cache. A smaller Materialization
does not promise universal reproducible rebuilding or approve Run. Actual
retained replay still requires a fresh receipt against the same frozen K.

## Storage, accounting and failure handling

Reuse current assignment/fence authorization and immutable bounded content
stream helpers over the owner-private STORE_BUCKET namespace. Digest possession
grants no access. Reserve one manifest, upload bounded chunks, and finalize only
after all file identities verify. Duplicate requests return the original
reservation. Ready manifests and bytes cannot change. Another owner, Runtime,
attempt or fence cannot publish/read them.

The existing per-attempt stored reservation bounds Record bytes plus the retained
runtime archive together. SQL enforces aggregate reservations atomically; final
settlement cannot report fewer than already retained bytes. Limits are not raised.
Uncertain uploads charge conservatively and retain durable references for
reconciliation; they never reset Search counters or create another attempt.

Reuse the Runtime delivery journal for custom transport retries, including lost
responses and restart. Per-operation retry counters are not refreshed. Reporting
after confirmed execution may continue after its execution deadline; it grants
no new inference, build or launch. Assignment expiry, checksum/size failure,
incomplete Record, budget exhaustion and uncertain execution remain distinct
from an application K failure.

## Acceptance

Cover exact operation recognition, complete lineage, symlink/path substitution,
ready-before-projection, unchanged source/installed files, v1 descriptor
compatibility, ownership/fences, immutable bytes, aggregate caps, lost responses
and cumulative retries. Verify native Rust and actual API WASM authority. Then
pin code, preregister a fresh zero-D changedetection.io Search, obtain same-K PASS
and a usable retained D, and separately verify function through the common
Runtime. Codex precedes an independent API arm with the same initial information.
No remote migration, deployment, 100-case execution, source rewrite or automatic
Run grant is implied.

The Coordinator advertises the supported evidence schema on the immutable source
attempt ticket. An absent declaration preserves historical full-workspace publication.
Projection requires both that declaration and the authenticated ready acknowledgement.
Publication checkpoint recovery checks the original Runtime journal's completed
identity before any storage RPC; UNKNOWN remains dominant. If execution finished but
its detailed report cannot be recovered, redelivery consumes the original resource
reservation conservatively and never re-enters execution or refunds a fresh budget.
