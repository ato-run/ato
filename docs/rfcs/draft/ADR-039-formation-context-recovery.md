# ADR-039: Bounded information recovery (5b-c0)

Status: implemented, local/offline verification; unmerged, not deployed.
Predecessor: [ADR-038](ADR-038-formation-generation-context.md), unchanged.
Evidence: [c0 ledger](../../ops/formation-context-recovery-c0-2026-09-27.md).

## Scope and version boundary

E1 did not establish model efficacy. Recover lost **lexical evidence**, without
adding authority, a selector, a model invocation, or an execution/repair loop.
`ato.formation-generation-context/2` is an explicit new type in
`generation_context::v2`. Context/1 keeps its exact fields and semantics,
including oversized-file rejection and UTF-8-only scanning. The lexer and marker
summarizer are shared; version-specific decoding and validation are separate.

Requester `generation_context_v2_for_offline` reads only authorized files in its
verified private frozen source. It returns context/2 without changing the
submission, policy, or enabled provider context. Receiver data can contribute
only projected failures/inspections, never source context. Mutable original
source is not reread. Existing provider point/2 and prompt/2 remain unchanged;
context/2 is **not wired to a provider request**. Future integration must define
point/3 and fixed prompt/3 (evidence, not proof), not relabel old payloads.
Old context/1, point/1 and point/2 compatibility remains tested.

Draft/1 is unchanged: operation `python_script` and an authorized opaque
entrypoint ID. No paths, argv, code, shell, patches, URLs, bindings, Runtime
selection, permissions, effects, or K changes can be generated. Context does
not affect identity, admission, receipt authority or UNKNOWN barriers.

## Closed fields and limits

Context/2 retains the prior project presence facts, opaque IDs, fixed import /
framework markers, count buckets and booleans. It adds:

- `source_scan`: `complete`, `bounded_prefix`, `unavailable` (not `too_large`).
- `encoding`: `utf8`, `latin1`, `unsupported`.
- `delegation`: `none`, `python_main`.
- Four fixed failure codes audited below. Unknown codes still map to `other`.

16 entries; 1 KiB per entry; 16 KiB serialized context; 16 failures;
4 inspections; 8 refusal codes per inspection; 11 import / 6 framework markers.
Read at most **65,536 raw bytes per file**. Metadata supplies full size; the core
requires exactly min(full size, 65,536) input bytes, otherwise unavailable.
Latin-1 conversion uses at most 131,072 UTF-8 bytes internally, never a larger
source read. No parser, codec dependency, OS locale or source execution is used.

### Delegation

Recognize an unaliased `import runpy` statement and a complete token statement
`runpy.run_path(<literal>, run_name="__main__")`. The target is discarded.
Comments and strings cannot supply call tokens. This deliberately does not
resolve aliases, variables, reachability or rebinding: it is a lexical hint,
not evidence that delegation will execute or that its target is authorized.
Missing a supported shape means `none`, not proof of no delegation.

### Prefix scan

Large files are read through `Read::take(65536)`, never read-then-truncated.
The shared lexer must close every string and bracket. Conservatively require a
final physical newline or a comment-only last physical line; reject a trailing
explicit line continuation. Cuts in names, operators, strings or brackets are
unavailable. Unavailable entries have no positive markers. A suffix may override
all observed behavior, so bounded-prefix evidence is explicitly weaker than a
complete scan (which itself is not a behavior proof).

### Encoding

Examine only the first two physical lines, each at most 1 KiB. The second line
is eligible only when the first is blank or a comment (including shebang).
Accept default UTF-8, UTF-8 BOM, exact `utf-8` / `utf8`, or explicit
`latin-1` / `latin1` / `iso-8859-1` cookies. Malformed/conflicting/unsupported
cookies, BOM conflicts and invalid UTF-8 become unavailable. This is a
conservative PEP-263-shaped subset, not general Python codec compatibility.
Encoding strings are never emitted, only the enum. No literal/status value,
filename, arbitrary name, comment, secret or URL is projected.

## Actual failure vocabulary audit

`runtime-attempt/src/launch/process_executor.rs::wait_until_ready` (readiness
loop) reports early process exit and timeout through untyped errors. The launch
path reaches `RealizeFailure::Launch` and `attempt.rs::not_observable`; execution
errors reach `failure_of`. Do **not** infer `process_exited` from those messages.
The worker `runtime_network.rs` reports the resulting `AttemptFailure.code`.

| Actual code | Context/1 | Context/2 | Meaning retained |
|---|---|---|---|
| `candidate_not_observable` | other | candidate_not_observable | launch/readiness/observability failure; not necessarily exit |
| `formation_failed` | other | formation_failed | untyped formation/execution failure; cause unknown |
| `candidate_stop_unconfirmed` | other | candidate_stop_unconfirmed | candidate may remain; does not authorize another attempt |
| `candidate_cleanup_failed` | other | candidate_cleanup_failed | cleanup failed; not a startup diagnosis |
| Existing EvidenceCode values | unchanged | unchanged | same fixed vocabulary |
| Anything else | other | other | never parse raw messages |

The first two recover process-startup evidence; the latter two preserve the
associated lifecycle boundary without pretending to know a process exit reason.
`ProcessExited` already exists in context/1 vocabulary, but the audited launch
path does not emit that code. No new exit-specific classification is invented.
Committed E08 observations contain `other`; the original exact code cannot be
recovered from them. No Runtime rerun was used to fill this gap.

## Gate and limitations

E04 delegation, E06 prefix server evidence and E07 valid Latin-1 evidence are
expressible for both opaque-ID permutations (6/6). Candidate bytes are checked
against immutable E1 preregistered SHA-256 hashes. E05/E09 remain equal except ID;
E10 has no invented service markers. Privacy, malformed input, strict schema,
bounds and frozen-source isolation are tested.

E06/E07 negative candidates also have server markers. Their scan/encoding
metadata differs, **not their projected HTTP behavior**; c0 does not claim a
selector can identify the passing candidate. E05/E09 remain intentionally
unresolved. Neither encoding nor file size is a positive correctness signal.
E2 efficacy needs separate approval. 5b is In progress; general 5b-c remains on
hold. No model or Runtime acceptance reruns, deployment, API change or migration.
