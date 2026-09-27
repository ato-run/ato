# ADR-038: Bounded source evidence for typed generation

Status: Implemented in an unmerged local/integration stack; no deployment.
Predecessor: [ADR-037](ADR-037-formation-typed-generation.md), retained as the
historical 5b-a design and prompt-v1 acceptance (one valid decline).

## Decision

5b-b changes model information, not model authority. The only draft remains
`ato.formation-derivation-draft/1`, `python_script`, an owner-authorized opaque
entrypoint ID. Canonicalization, static validation, same frozen K, ordinary
Runtime admission/execution/verification and fresh receipt acceptance remain
mandatory. Context never enters D/K identity or supplies capabilities.

Before generation, all authorized known Ds are explored within the Runtime
constraint. Exact restricts placement, not the known-D frontier. A known PASS
prevents generation; generated D is subject to that same Exact constraint.
No-generation-policy Exact behavior is unchanged. This correction is in the
5b-a core/receiver PRs rather than hidden in the evidence slice.

## Source projection

Requester retains its verified private frozen source until Submission drops.
It never re-reads the original mutable directory. Explicit generation
context enablement reads only the authorized regular files in that snapshot.
The existing archive verification and symlink refusal are unchanged.
`DetectorEvidence` is reused only for presence facts; its arbitrary versions,
module names, manifest contents and paths are not provider inputs.

`ato.formation-generation-context/1` contains:

- `entrypoints`: sorted opaque `id`, fixed `language=python`, size bucket,
  scan status, fixed import/framework markers, function/class count buckets,
  `main_guard`, `server_listen`, `custom_http_handler` booleans.
- `project_summary`: Python/Node, manifest/lockfile/README/static HTML presence
  booleans and regular/Python file count buckets.
- `failures`: fixed status and code vocabulary.
- `inspections`: safe kind with bounded fixed refusal codes or typed failures.

No Python AST parser dependency exists in this slice. A small bounded lexer
skips comments, quoted/triple-quoted strings and f-strings. Malformed scans
become unavailable. Markers are lexical hints, not AST or behavior proofs.
There are no arbitrary module/function/class names or README headings in the
projection. Thus source-derived strings are a closed vocabulary, not an
instruction channel. The only variable string identity is the owner's existing
opaque ID (1–32 ASCII alphanumeric/underscore, excluding `none`).

Limits: 16 entrypoints; read at most 64 KiB per authorized file; larger files
are not prefix-scanned; entry summary at most 1 KiB; whole context at most
16 KiB; at most 16 failures, 4 inspections, 8 refusal codes per inspection;
11 import markers, 6 framework markers. Count/size buckets have fixed enum
values. Dynamic evidence deterministically retains recent eligible records.
Unknown fields/enums, duplicate or unordered identities, and oversized contexts
are rejected. Unknown failure codes become `other`, never echoed.

## Requester and provider boundary

The receiver's existing bounded `generation_point.evidence` projection is now
consumed. It is projected again for the provider: no attempt/runtime IDs, raw
messages, logs, receipts, arbitrary payloads or host identity. The immutable
source summary is combined with current failure/inspection evidence after
checking the owner-authorized entrypoint set.

The requester-local point explicitly uses `ato.formation-generation-point/2`.
Historical points without a version and prompt-v1 APIs remain supported.
Prompt `ato.formation-generation-prompt/2` puts the typed context only in state;
question instructions and option descriptions are fixed. It asks for a
candidate plausibly satisfying the same K, or decline if evidence is inadequate,
never code, shell, argv, paths, source patch, URL or new permissions. Presence
of markers does not prove K. This slice intentionally does not send K contents
or invent a success heuristic.

The one-shot durable claim precedes any model invocation; no retry after claim.
Deadline, generation/decision/attempt/transfer/expanded/stored bounds and UNKNOWN
barriers remain Rust/receiver authority. Source evidence does not change these
budgets. v2 requires a validated context before HTTP. No new receiver schema or
migration is needed: migration 0303 remains in the unmerged predecessor.

## Provenance and acceptance

Provider/exact model/prompt version/input and output tokens are recorded for
validated drafts and declines. Acceptance logs the privacy-projected request,
call count, provider latency, total elapsed, attempts, selected ID/decline,
resulting D ref and fresh same-K receipt result. Estimated cost uses separately
cited pricing; unknown historical usage is never backfilled.

G9 runs once only after offline observability checks. Decline/invalid/failure is
preserved, never retried until success under the same prompt version. A valid
model draft or HTTP startup alone is not completion. Overall 5b remains in
progress; results belong in the operations ledger, not this normative boundary.
