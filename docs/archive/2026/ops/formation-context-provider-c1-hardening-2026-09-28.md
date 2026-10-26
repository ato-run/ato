# c1 review hardening: claim boundary

Reviewed parent: `0e35d54dd4551cbc9e60d8dbd2029ad44f72a23f` (#1420).
Base: `660dbf6eaedc3384017137f1f20282a359d423aa`.

- Public `generation_request_v3` and `validate_response_v3` require
  `validate_claimed()` (structural validation plus `claimed=true`). Pre-claim
  construction still uses structural `validate()`.
- After durable claim, `serve_generation` applies its revision, sets claimed,
  and revalidates before calling any provider implementation.
- Point/3 revision matches the receiver's existing nonnegative JavaScript-safe
  integer domain. An invalid claim revision must never reach a custom provider.
- A deliberately unchecked custom recording provider verifies that the requester
  enforces this boundary, independently of Jev or provider-side validation.
- ADR-040 now says **provider/model HTTP**, not all HTTP, requires a durable claim.

Verification: full four-package regression **581 passed, 0 failed, 1 existing
ignored**; Clippy all targets with `-D warnings` passed. New tests directly reject
unclaimed public request/response calls (draft and decline), reject unsafe
post-claim revisions before an unchecked provider, and confirm a legitimate
custom provider sees only claimed=true and the returned revision.

This supplements, not rewrites, the original c1 verification record. E1/c0
observations remain untouched. No API, migration, compiler, K, permissions,
Runtime/effect authority or prompt changes. Model calls 0; Runtime efficacy
reruns 0; E2 not started. No deployment. Merge status/SHA is recorded by GitHub;
this note describes the reviewed hardening delta and local verification.
