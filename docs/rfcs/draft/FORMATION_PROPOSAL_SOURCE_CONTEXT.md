# Formation D2-A: source observations, not execution authority

Status: core contract slice. D1 receiver metadata merged as API #701
`9fa4be45adf7dbda1fa68689c365a1806921569f`. No live calls, deployment or remote
migration. Requester source extraction/transport is deferred to D2-C, **after**
D2-B updates the receiver from this slice's merged Rust source.

## Versioning and compatibility

`ato.formation-proposal-request/1`, `ProposalRequest`, fixed producer and their
serialized fields are unchanged. New `ProposalRequestV2` uses the singleton
schema `ato.formation-proposal-request/2`, all /1 data fields, and required
`source_context`. Unknown/duplicate fields and unsupported schema/entry kinds
are rejected by typed deserialization. Validation/canonicalization additionally
requires the owner's authorization. A /2 payload cannot deserialize as /1.

`CandidateProducerPolicy` accepts exactly false/0 or true/1..=65536. One round,
max four proposals, timeout <=30s and raw <=16 KiB remain unchanged. Enabling
source observation does not alter compiler/catalog/Derivation/K semantics.
Old false/0 frozen bytes retain their meaning. HTTP, credentials, provider
products and model defaults do not belong in this core module.

## Source context

Each entry contains `source_id`, `kind` (entrypoint/module), `logical_id`,
`encoding: utf8`, `truncated`, `content_sha256`, `text`. No paths or private
source-domain mapping. README, arbitrary manifests and crawling are deferred.

The caller supplies `AuthorizedSourceText` **only after** verified frozen
archive regular-file membership checks; the DTO is not itself a membership
proof. The core checks logical ID against the owner's entrypoint/module map.
No file is read in core. D2-C must reuse C2 inventory, not mutable checkout,
links, provider-selected paths or host-installed packages.

Sources are sorted by typed kind/logical ID; duplicate/unauthorized entries
reject even if their bytes are unavailable. Invalid UTF-8 and text containing
non-whitespace control characters are omitted, never lossy-decoded or base64
encoded. Available sources get deterministic equal shares of the aggregate
budget, each capped independently at **8192 bytes**, so one source cannot
starve all others. Prefix truncation rounds down to a UTF-8 character boundary;
zero-byte prefixes are omitted and `truncated` reports any shortened entry.

`content_sha256` hashes exact transmitted text, after truncation. `source_id`
is `s_` plus SHA-256 of canonical (kind, opaque logical ID, full-content digest),
never a path or path encoding. `source_context_sha256()` hashes JCS context
bytes; request canonical bytes are also JCS. Same bytes/authorization/policy
produce the same context regardless of input traversal order.

Source text is untrusted application data. Prompt instructions will not be a
security boundary. Ato ProposalValidator alone admits operations; only Verifier
establishes K. No new D from a context hash, a producer statement or compilation.

## Gates

Core tests cover policy combinations, strict /1-/2 separation, old bytes,
source-enabled zero-D eligibility with byte-identical D/K compilation,
UTF-8/binary filtering, per-entry/aggregate bounds, deterministic sorting and
truncation, logical authorization, context/hash corruption and path absence.
This is not source-inventory, HTTP, Runtime or live-model acceptance.

D2-B must rebuild the receiver WASM from the D2-A merge SHA and widen only its
matching policy DTO; migration 0304/0305 remain unchanged. D2-C follows that
receiver merge. DeepSeek live calls and spend remain zero; USD 5 total is the
later D3 ceiling, not permission to call before preregistration approval.

Local D2-A verification (2026-09-28): formation tests **310 PASS**; four-package
C2 scope (`ato-formation`, `ato-formation-worker`, `ato-runtime-attempt`,
`ato-receipt-authority`) **645 PASS / 0 FAIL / 1 existing ignored**. Clippy for
all targets of those packages, fmt and diff checks PASS. A first sandboxed run
could not observe local HTTP candidates; rerunning the same code with local
network permission passed, without changing tests/implementation or skipping
failures. These are local regressions, not a new P0–P11 actual Coordinator run.
