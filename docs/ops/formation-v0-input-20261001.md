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

## Temporary binding across rounds

The first Kutt Search at Ato `3f07cd8e` / API `c7f990ce` created a temporary
JWT with the first round deadline. A later round stopped before native execution
with `variable_registration_conflict`; the original expired value and failed
measurement are retained. New temporary values expire at the original Search
deadline; each redemption remains bounded by the current fenced attempt/round.
Existing expired/revoked rows return `needs_input` and are never re-registered
with a later expiry. Eight encrypted-store tests and five relevant Coordinator
route tests passed. Retained replay now redeems the same fenced Runtime grants
instead of launching with an empty private input list. No credential value is
added to the descriptor or derivation. Actual scope/fault/cleanup acceptance
remains required separately from these boundary tests.

## Contained process artifact aliases

The actual Kutt trial at Ato `90e09582` / API `b027f1c8` completed the typed
rebuild/audit of better-sqlite3 and msgpackr-extract, then failed Runtime artifact
protection before launch. The scanner unconditionally refused symlinks, including
ordinary npm command aliases. Reuse the same contained-relative-link validator
used by source and retained archives; inspect link target bytes for protected
values without following aliases. The normal tree walk still scans all regular
files under the original entry/byte/deadline bounds. Escapes and secret values
in paths, link targets or file bytes fail closed. Seven guard regressions and
all-targets Runtime clippy PASS locally. Real Kutt launch remains pending.

The same trial's persisted local Coordinator shows two D assignments referencing
one temporary credential with its original Search expiry `1790890134677`. After
known terminal infrastructure failure, the value row was deleted and metadata
revoked at `1790889138945`. This verifies private grant reuse and cleanup through
the actual Coordinator/Runtime transport; workload injection, owner CLI/UI
input, scope boundaries, revocation and persistence still require acceptance.
No value, ciphertext or credential identifier is included in this record.
