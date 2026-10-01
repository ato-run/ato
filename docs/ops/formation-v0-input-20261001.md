# Formation v0 owner input — 2026-10-01

Depends on Ato #1455 and the companion API owner-input change. The merged-main
base remains `8fca0f4b78b1bed92eae21823e5c5f7207058271`. Prompt 11 adds the new
binding capabilities; versions 1–10 and their pending public capability views
remain available without changing saved inputs or provider configuration.

`ato form-input --search-id <existing Search> --api <same API>
--token-file <owner token file>` inspects the current requirements, purpose,
acquisition guidance, source/service/account/resource/operation/phase scope and
original input deadline. Supply `--item <index> --value-stdin --reuse
this_formation` to accept a private value from stdin. `--reuse reusable` also
requires `--expires-in-seconds`; non-secret embedding requires
`--allow-artifact-embedding`. `--credential-id` selects an explicit existing
candidate. The metadata-only checkpoint fixes registration identity and expiry
across response-loss retries. It contains no value. Resume uses the original
`ato form` config and provider journal; it does not create a fresh budget.

The PWA owner page `/formations/<Search>/input` uses the same cookie-authenticated
API, the Home header and existing form styles. This-Formation/reusable scope,
expiry and embedding permission are explicit. Candidate selection and revocation
are explicit; scope mismatch, ambiguity, expiry and revocation are displayed.
Browser retry storage contains only registration identity/expiry, never values.

The encrypted store/metadata/assignment/private Runtime grant remain shared.
Variable delivery uses the frozen custom retry/deadline policy and durable
transport markers; grant values remain process-private. Deadline and
infrastructure failures are no longer mislabeled as missing input. Cleanup and
saved reporting remain allowed after expiry; UNKNOWN never permits automatic
secret deletion or reexecution.

Single-service OCI accepts Runtime-phase grants through the existing private
environment-file launch path. Values do not become D/image identity or public
provenance. OCI acquisition/build credential grants and runtime egress remain
unsupported. Static builds accept dependency/build grants, with explicit
non-secret embedding permission; browser runtime injection remains unsupported.
Protected process artifact scanning and canonical static extraction/production
have chunk-level deadline guards. Static output still uses the same canonical
materializer and fresh frozen-K Runtime verification.

The v0 source boundary remains root manifests/locks/config/Dockerfile/README,
then literal references. Static Web, single Node/Python app and single-service
OCI are the center. No undeclared monorepo/multi-repo discovery, Compose service
construction or source/Dockerfile rewriting is added. Runtime absence, unbound
capability, broken source, missing input, no progress, budget/deadline exhaustion
remain distinct. Successful D is awaiting risk assessment, not normal Run
permission/publication/deployment.

Local API/Runtime/PWA boundary regressions are retained in the task worktrees.
The final code pin still needs real Coordinator/Runtime acceptance for SVGOMG,
one OCI source build, changedetection.io and Kutt, separately for Codex/API and
for functionality/state/fault/credential cases. No new live provider call,
deployment, remote migration or flag enablement has occurred in this change.
Historical measurements and WBO UNKNOWN are preserved. The 100-app rerun requires
the small gates and an explicitly available aggregate call/cost allocation.
