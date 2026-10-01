# ADR-040: Context/2 provider integration (5b-c1)

Status: implemented, pending review/merge; not deployed; no live evaluation.
Predecessor: [ADR-039](ADR-039-formation-context-recovery.md), historical c0
semantics/evidence unchanged. Corrected c0 merged as #1419 at
`660dbf6eaedc3384017137f1f20282a359d423aa`, the exact c1 base.

## Decision: explicit opt-in, no authority expansion

The new local `ato.formation-generation-point/3` carries context/2 and uses
`ato.formation-generation-prompt/3`. Context/1, point/1, point/2 and prompts/1–2
keep their meanings and existing tests. No default provider or selector changes.
No goal/Contract projection is added: mixing it with source-context recovery
would confound a later evaluation. Safe K/goal information is only a potential
separate c2 hypothesis, not implemented or scheduled here.

Use `Submission::enable_generation_context_v2()` and
`JevGenerationProvider::from_env_v3(timeout)` together, then the existing
`serve_generation`. Constructors do not call a provider; no credentials were
configured or read for this work. V1/V2 provider constructors remain unchanged.
Older providers reject `generate_v3` by default; V3 rejects legacy `generate`
instead of silently downgrading. This explicit library opt-in is not a rollout
or an efficacy claim. Existing acceptance modes are not changed.

## Requester owns source evidence

The opt-in builds and caches only source/project facts from the verified private
frozen snapshot and owner-authorized entrypoints. Source reads remain capped at
65,536 bytes per entry. A shared private projection helper is used by offline c0
and by enablement; the production path does not call the offline API.
Serving a generation combines cached facts with bounded, fixed-vocabulary
failure/inspection projections. It does not reread the mutable source directory
or re-read files at claim time. Reauthorization clears both context caches;
context/1 and context/2 enablement are mutually exclusive.

The receiver's `schema` and `context` are removed before local interpretation.
Even a valid-looking receiver context/2 cannot replace local facts. Its raw
failure/evidence fields are projected, never copied to the provider point.
The local point consists only of:

```
schema = ato.formation-generation-point/3
revision, expires_at, claimed, entrypoint_ids
context = ato.formation-generation-context/2
```

Public request and response validators require `claimed=true`; pre-claim local
construction uses structural validation only.

Point/3 is limited to 18 KiB serialized; raw `from_json` input is bounded before
parsing. Expires-at is at most 64 bytes. Context keeps its 16 KiB limit, entries
16 × at most 1 KiB, failures 16, inspections 4, refusal codes 8. Context IDs must
exactly equal the finite offered ID set, with no duplicates. Mixed context/point
versions and malformed public-field mutations fail boundary validation.

## Claim ordering and restart boundary

1. Read status; recompile any admitted draft through the unchanged authority.
2. Strip receiver schema/context; reject a claimed or non-running point.
3. Validate offered domain against frozen policy, build/validate local point/3.
4. Obtain the existing durable generation claim.
5. Set the returned revision and `claimed=true` locally.
6. Revalidate the claimed point, including the receiver's JavaScript-safe revision
   domain, before invoking any custom or Jev provider.
7. Call the v3 provider once and submit through the unchanged completion API.

No provider/model HTTP request occurs before a successful durable claim.
Jev's v3 method additionally rejects unclaimed or invalid points before transport. A lost claim response
causes no provider call; stale status after restart still cannot win a second
claim. A lost completion response cannot authorize a retry. Unit integration
uses a loopback Coordinator mock and an in-memory recording provider; it is
not a fresh actual-Coordinator acceptance or an efficacy run. Durable receiver
fences, deadlines, UNKNOWN and budget semantics are unchanged.

## Provider-visible payload and prompt

Provider `state` is **exactly context/2**: sorted opaque IDs, closed lexical
markers/count buckets, encoding/delegation/scan enums, project presence flags,
and fixed failure/inspection summaries. No revision, expiry, claim metadata,
raw evidence, attempt/runtime/host IDs, receipt, source text, filename/path,
delegation target, comments, arbitrary literal/name, URL, secret or argv.
Two finite questions are unchanged: operation `python_script|decline` and offered
entrypoint IDs plus `none`. Instruction and criteria values are fixed strings;
source-derived data never interpolates into them.

Prompt/3 says all markers are evidence, not proof:

- python_main is lexical delegation evidence, not proof of success;
- bounded_prefix covers only a prefix; suffix semantics remain unknown;
- latin1/utf8 describes scan provenance, not correctness;
- unavailable leaves source semantics unknown.

No preference for complete/UTF-8 or penalty for prefix/Latin-1 is embedded.
No fixture label, E1 case, HTTP status literal or expected outcome is supplied.

## Response and wire compatibility

The existing strict finite-answer validator is reused. Accepted drafts remain
exactly `ato.formation-derivation-draft/1`, `python_script`, offered opaque ID.
Decline must pair with `none`. Unknown answers, fields, permissions, paths,
argv, code, K changes, Runtime/effect instructions cannot become a draft.
Canonical compiler, admission, same-K verification and receipts are untouched.

Provenance preserves provider `jev`, exact configured model, prompt/3, and exact
bounded input/output token usage. V3 uses the existing V2 rules for observed
decline/rejected-response metadata; V1 historical semantics are unchanged.
Tests use synthetic responses only: there is no live usage or cost observation.

API main `f7d866cbef7768f67b46840766497fbf806640fe` was inspected read-only.
`GenerationProvenanceSchema.prompt_version` is already a bounded string;
completion body is unchanged. Point/3 is requester-local, not persisted by the
receiver. No API change/suite, new migration or 0303 edit is necessary.

## Completion and non-claims

Offline provider-request tests cover both ID permutations for E04/06/07 (6/6),
unchanged indistinguishability for E05/09 and no new service marker for E10.
Fixture bytes are checked against immutable E1 preregistration hashes. Existing
V1/V2 tests pass unchanged; privacy canaries are checked in the complete request
JSON, including question text and criteria. Bounds, version mixing, response
injection, cache isolation, concurrency and lost-response/restart are covered.

E1 B 9/20, C 2/20, robust additional success 0 and unmet efficacy gate remain
historical facts. C0 offline observations are not rewritten or reaggregated.
5b remains In progress. E2 is **not started**: preregistration requires reviewed,
merged point/3+prompt/3 artifacts and separate approval. Model calls 0; Runtime
efficacy reruns 0. No API/remote migration/deployment/staging/production change.
