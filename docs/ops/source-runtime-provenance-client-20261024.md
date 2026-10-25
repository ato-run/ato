# Explicit source provenance in the existing Requester client

Date: 2026-10-24. Base is frozen `integration/source-request-base-v1` at
`6251745338a2601e744e7788f14ad5ad3f5cea0d`. It is not advanced by this child.
This change uses the existing `proposal_search` example and Requester engine;
it adds neither a second orchestration loop nor a new product CLI.

## Boundary and inputs

Previously the explicit raw-archive example already accepted an owner-frozen
Satisfy request and verified source, but `Client::submit` used the legacy
`/sources` upload and plain `/satisfy`. It therefore did not carry the accepted
source-only Job's API provenance into the existing dedicated handoff.

The new optional members of the **existing** example configuration are:

```json
{
  "formation_source_pointer": {
    "source_archive_job_id": "fjob_saved_source",
    "binding_digest": "sha256:<exact saved physical binding>"
  },
  "expected_source_origin": {
    "result": "the complete saved SourceArchiveOutput object, not this string",
    "source_selection": "the complete saved pinned git_hub selection, not this string"
  }
}
```

Both must be supplied together with the existing `source_job`, `source_archive`
and `frozen_request` fields. No body/source default is generated. `source_job`
is the existing local immutable uploaded-archive verification envelope; it is
not submitted as a new Job or treated as authorization for the accepted Job.
The new pointer names the actual accepted source-only Job. The full saved
origin comes from its existing API transfer response; it contains no bearer,
provider credential, signed URL, K or success verdict.

The following remain exactly owner-authored: `frozen_request`, its K/ref,
initial source, authorized D set, constraints, policy and budget. The existing
example compares the duplicate `contract`, `search_id`, `authorization`,
`policy` and `budget` configuration against those frozen values. In pointer mode, the decoded `frozen_request`
must also serialize to exactly the original JSON value; explicit nulls or other
typed normalization are rejected instead of repaired. This initial
explicit archive lane retains its existing zero-known-D restriction; it does
not silently drop known D or generate K. Root subdirectory only is supported,
matching the current API source intake.

Local validation recomputes the physical Binding and source archive proof,
including digest/tree/closure/format/resolver/counts. It **does not** establish
ownership, approval, current attempt or runtime authorization. The server still
rechecks the accepted output, owner, current fence/UNKNOWN/cancellation, ready
bytes and exact request at its existing admission/CAS boundary.

## Existing command

Use the same example entry point, with its complete existing configuration and
the optional members above:

```sh
cargo run --offline --locked -p ato-formation-worker --example proposal_search -- \
  .tmp/source-request-config.json
```

Set `preregister_only: true` first. This returns after local proof validation,
before constructing the API client or opening `token_file`; the offline result
is no claim of server approval or application formation. A later explicit
invocation with `preregister_only: false` permits the two existing POSTs and the
same existing provider/observation behavior stated in that configuration.
Provider credentials and journals retain their existing distinct configuration
and finite budget rules. Merely copying this command authorizes no provider or
shared environment change.

The actual online operations are:

1. `POST /v1/runtime-network/sources/from-formation`, pointer only.
2. Validate the full transferred source, output attempt/binding and selection
   against the saved origin.
3. `POST /v1/runtime-network/satisfy?source_archive_job_id=...&binding_digest=...`,
   unchanged typed Satisfy body; the pointer stays out of K/request fields.
4. Continue the existing status/proposal/receipt loop.

No raw archive is uploaded or re-encoded. A refusal, changed source/origin,
malformed/oversize response or redirect does not fall back to legacy upload or
retry a POST. A manual repeat can receive the same existing `satisfy_id` without
resetting its frozen budget; `resume` additionally requires that exact returned
identity. The original Search deadline and remaining capacity stay server-owned.
The example also bounds its own source handoff/observation wait without turning
that wait into a new K verdict or durable cancellation.

## Validation scope

All runs used an isolated macOS target and workspace `.tmp`, fake credentials,
fixed fixture source and a loopback stub. No shared Linux Cargo target, real
source Job, real API, provider inference, Runner install, flag, deployment,
production key or paid resource was used.

The full macOS `ato-formation-worker --all-targets` regression passed with
399 tests and one existing ignored case (36 suites), followed by two additional
example decoding tests after the final pointer-only JSON equality check.
All-target Clippy with `-D warnings` and workspace formatting passed again on
the final source. These are local CPU checks, not Linux workload acceptance.

The wire tests cover unchanged body/query separation and manual replay, local
pointer/body/measurement/deadline refusal before any POST, transferred source /
attempt / binding / selection / started / unknown / oversize refusal before
Satisfy, cancelled/UNKNOWN refusal and redirect without retry/fallback, and the
unchanged legacy `/sources` + plain `/satisfy` path.

The compiled **actual example binary** was also invoked twice against a local
stub using the Rust-produced raw gzip/source/origin fixture. Offline
preregistration succeeded with a deliberately nonexistent token file and zero
HTTP calls. Online it made exactly transfer POST, source-bound Satisfy POST and
one existing status GET. The captured Satisfy body equaled the owner request;
K was unchanged, source reupload was absent, provider calls were zero, terminal
status was `stopped` and accepted routes were empty. This is a concrete client
wire acceptance, not an executed Formation, successful Contract or staging E2E.

Task-local logs and the smoke harness are retained under
`.tmp/frequester/.tmp/` (`handoff-tests.log`, `cli-smoke.log`,
`cli-smoke-result.json`, `full-tests.log`, `clippy.log`, `example-tests.log`,
`final-clippy.log`, `final-cli-smoke.log`). The independent PWA
owner input uses the same server operations and is a separate PR.
