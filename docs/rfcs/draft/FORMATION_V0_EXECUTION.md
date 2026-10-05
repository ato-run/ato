# Formation v0 execution completion

Status: implemented; release acceptance in progress. Requested 2026-10-01.
The product stack is merged, and staging/production controlled-Source acceptance
is measured. The frozen 100-OSS campaign and independent API comparison are still
in progress. See the [release gate record](../../ops/formation-v0-release-progress-20261006.md)
for historical pins and measured limitations; no unimplemented capability is
declared available and no completed receipt is reassigned to a later Git SHA.

The implementation base is ato `8fca0f4b78b1bed92eae21823e5c5f7207058271`
and ato-api `9e0031900836090612b94e775e26f980a68f32d7`. Reuse the existing
Coordinator, immutable K/D, Runtime attempt journal, contained build/launch,
source-OCI builder, encrypted variable store and assignment/grant services.
Those are the original implementation bases, not current execution pins. The
separately authorized release applied the ordered migration chain and deployed
the measured integration revision. Ordinary Run/publication/fork promotion and
managed procurement remain disabled; Source success requires explicit owner
functional-verification authorization before a functional Instance is created.

## Search control

Capture effective policy once: three proposal rounds, initial dispatch plus three
retries per operation, ten minutes per round, thirty minutes per Search by default.
Inspection, authoring repair and communication retry do not allocate another
round. Restart, response loss and variable input preserve the original identity,
revision, deadline, attempt reservations and provider accounting.

The Runtime ticket carries the frozen custom retry limit. Assignment polling
before a ticket exists is Runtime service discovery; its bounded bootstrap policy
does not create a Search or execute a D. Once assigned, ticket operations use the
Search limit and durable dispatch counts. Result delivery and settlement keep the
same retry limit but may continue after the execution deadline.

One absolute deadline bounds HTTP, archive verification/extraction, workspace
staging, dependency/build steps, launch readiness, HTTP/browser verification and
backoff. Each phase checks before starting effects and bounds its active wait by
the remaining time. Cleanup, durable result delivery and conservative accounting
are permitted after expiry. Expiry never creates a new reasoning call or attempt.
Phase duration and each actual provider request's usage/cost are evidence, not
semantic identity. Unknown usage is conservatively charged, never fabricated.

Distinguish terminal unsupported capability, needs input, evidenced source failure,
no progress, infrastructure failure, budget exhaustion and deadline expiry. An
unsupported declaration cannot repeatedly reopen a round with unchanged source,
capabilities and information. A source failure requires reproduction evidence or
evidence that source changes are necessary. Workload-effect UNKNOWN remains a
separate execution barrier and cannot be converted to a known terminal by retry.

## Supported application boundary

The central v0 routes are static Web, one Node/Python application and one OCI
service. Inspect root manifests, lockfiles, configuration, Dockerfile and README,
then follow acquired explicit references. Do not discover undeclared monorepos or
multiple repositories. Multi-service Compose and automatic source/Dockerfile
rewriting are outside this change.

Publish only measured, bound Runtime/builder capabilities to both providers.
Unsupported routes, a temporarily unavailable Runtime and invalid source are
different outcomes. OCI is an explicit alternative D, not an implicit fallback.
Pin image digests, base images, acquired artifacts and build outputs. Preserve
build provenance and declare fetch/build/runtime network, authority, variables,
ports and state. Build success must still pass normal Runtime observation against
the original frozen K with a fresh attempt-bound receipt.

Python source requirements may resolve wheels or build a wheel from sdist under
declared, isolated build dependencies/toolchains/network/resource bounds. Record
version, digest and provenance; install the completed artifact offline by hash.
Node lifecycle/native rebuilding is a typed operation with declared toolchains and
conditions. Acquiring dependencies with scripts ignored is not a runnable build.
Neither operation embeds application-specific commands in presets.

Node `setup_scripts` names up to four distinct scripts from the frozen manifest.
It requires the Node `launch_script` route and is unavailable for static, Python
and OCI routes. The ordinary process adapter runs these scripts sequentially
after state and private Runtime grants are attached, then runs the declared
launch script. They share its containment, network, resource limits, process
group and original execution deadline. Validate the manifest hash and every
script key before any preparation effect; a preparation failure never launches
the service. Empty preparation keeps earlier D bytes unchanged. This is a
source-owned Adapter operation, not a new Core primitive or a remote migration.

Variables use the existing encrypted owner store, metadata, assignment and Runtime
grant. CLI/UI show needs-input metadata, acquisition guidance, reuse choice and
scope. Stop explicitly on scope mismatch, ambiguity, revocation or expiry. Values
never enter model inputs, D or public evidence. Verify ephemeral deletion and
embedding policy in the actual Runtime path. External credential validation uses
only an explicitly authorized test account.

## Acceptance and delivery

Separate reviewable PRs cover control, OCI, native dependencies and input paths.
Fix final code pins before small actual Coordinator/Runtime acceptance: SVGOMG,
one OCI application, changedetection.io, Kutt and fault/binding cases. Establish
Codex first, then API from the same initial information without seeding Codex D.
Do not weaken K. Functional/UI/persistence observations are separate from typed-K
receipts. Submit successful D as `k_reached_awaiting_assessment`; do not grant a
normal Run, publish or deploy it automatically.

Preserve old results and WBO UNKNOWN; do not rerun unresolved work. Record unique
applications by provider, first/repair PASS, functional/persistence/fault outcomes,
runtime/provider/code pins, unverified changes, CI attribution, actual calls/cost,
unsettled reservations and remaining budgets. The 100-app wave starts only after
small acceptance and an explicit budget/call allocation.
