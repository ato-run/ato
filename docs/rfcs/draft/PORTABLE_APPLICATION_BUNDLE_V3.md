# Portable Application Bundle v3

Status: Draft  
Profile: `ato.portable-application/1`  
Wire version: `3`

## Scope

This profile carries one static HTML/JavaScript application surface and the
immutable files required to serve it. The first increment has no external
bindings, saved data, instance assets, build step, runtime dependency, or
multiple surface/derivation selection.

The existing bundle v2 format remains a computation-root format. A reader must
dispatch on `index.version`; it must not reinterpret a v3 ContractRef as a v2
ComputationRef and must not fall back to the v2 reader for an unknown v3
profile.

## Identity and transport

`index.root_contract_ref` is the CapsuleId. It is the SHA-256 digest of the JCS
canonical `ato.contract/1` object. The SHA-256 of the complete `.capsule` bytes
is transport integrity and provenance only. These values are never cast or
substituted for one another.

The JCS envelope contains `index` and `payloads`; unsigned files omit
`signatures` or encode it as an empty array. Objects and payloads are ordered by
lowercase `sha256:` reference. Structured objects declare a schema and contain
canonical JCS bytes. Blobs declare no schema and remain opaque.

The root set is the Contract, Application, and selected Derivation references.
Only schema-owned reference extractors may add closure edges. The declared
object set must equal the reachable set exactly.

## Verification

Formation may seal a candidate when every requirement is either satisfied or
deferred to a named runtime verifier. Interoperability succeeds only when an
actual run returns `fully_satisfied=true`; a deferred result is not success.
Both local and hosted execution emit
`ato.contract-verification-receipt/1`. The receipt records the bundle hash,
ContractRef, DerivationRef, target, and per-requirement verdicts.

For an HTTP body digest, Formation proves the exact artifact path exists and
defers the byte comparison. The runtime must issue the declared request, read
the response body, calculate SHA-256, and compare it with the original bound
Contract. It must not capture a replacement expectation.

## Hosted import

Hosted import uses the existing bundle upload and validation transaction. A
validated v3 bundle is materialized directly as a static application; it is
never submitted as source ZIP and never sent through Formation. Stored
provenance includes bundle id, bundle SHA-256, root ContractRef, and selected
DerivationRef.

## Initial acceptance fixture

`samples/interop-static-k` contains `index.html`, `proof.txt`, and the existing
`ato.capsule/1` authoring grammar. The generated `samples/interop-static.capsule`
bytes are used
unchanged by CLI-local and hosted tests. The Contract binds source identity and
requires `GET /proof.txt` to return status 200 with the SHA-256 of the exact
bytes `ato-k-interop-v1\n`.
