# Portable dependency export policy

Status: draft implementation contract
Date: 2026-09-16

## Decision and identity

Export packing is a transport decision, not a new Capsule or Derivation kind.
For a fixed Application, Contract K, and Derivation D, `thin`, `cached`, and
`offline` exports retain the original `ContractRef` and `DerivationRef`.
Their whole-file SHA-256 values normally differ. A dependency's immutable
identity is its content digest (and, for an OCI image, the selected platform
and digest-pinned image manifest); a source URL is only a retrieval hint.

`ato.portable-application/1` wire v3 continues to mean a completely embedded
declared object closure. Its schema, decoder, digests, and existing fixtures
are not reinterpreted as a sparse bundle. Wire v4 uses
`ato.portable-application/2` and a separate `portability` transport manifest.
The manifest names the profile and the exact set of external blob references
with retrieval hints. Semantic structured objects, including K, Application,
D, and tree descriptors, remain embedded so that a validator can prove their
references before any network request. An offline v4 bundle cannot list an
external object. The source hint is not its identity; fetched bytes must match
the descriptor's size and SHA-256.

## Profiles

| Profile | Dependency payloads | Network claim |
|---|---|---|
| `thin` | External where a digest-verified source is declared | Network-dependent; no offline promise |
| `cached` | Embed selected reusable objects, fetch missing ones only by digest | Receipt records each actual external fetch |
| `offline` | Embed the complete dependency closure for every selected D | No external dependency fetch after import |

Host capabilities are separate from portable objects. Python interpreter,
Docker Engine, compatible OS/architecture, kernel, and device drivers may
remain prerequisites even for `offline`. An exporter must refuse an offline
claim when a required portable object is unavailable or its digest cannot be
validated. It must never silently downgrade to `cached` or use an installed
host package/image as if it were part of the bundle.

The export planner derives embedded/external counts and bytes, required host
capabilities, and estimated encoded size. A caller-provided
`network_required: false` is not accepted as evidence. The selected profile
and sources are excluded from K and D. A CLI receipt for wire v4 uses
`ato.contract-verification-receipt/2`, identifies its portability profile,
records each actual external fetch in `execution.dependency_fetches`, and
records an embedded OCI image load when one occurred. Wire-v3 receipts retain v1.
Cache-hit provenance and OCI image acquisition provenance remain to be added.

The first v4 exporter handles PyPI wheel payloads. It confirms an exact
filename plus SHA-256 against the PyPI release metadata and subsequently
fetches only from the HTTPS PyPI file host, without following redirects.
`cached` currently embeds all wheel payloads; the v4 decoder and repacker
also support an explicit mixed set of embedded and external blobs. Thin and
cached OCI still need a registry on a clean host. An offline OCI export must
include an OCI-layout archive whose manifest, config, and every layer match
the fixed D image digest and declared platform. An absent or invalid archive
is rejected rather than mislabelled.
The current Datasette v4 bundles are CLI-local artifacts; API/PWA import of
wire v4 is not yet an accepted hosted path.

## Validation and execution boundary

1. Decode the versioned envelope and verify canonical encoding, every
   embedded payload digest, sorted uniqueness, descriptor sizes, semantic
   reference closure, and the explicit external-object set. A missing
   `offline` payload is a bundle failure before D selection.
2. Select one declared D. Admit its host capabilities and network policy.
   A valid bundle with an unavailable dependency is not called tampered.
3. Resolve each needed object by digest. Embedded bytes take precedence;
   verified cache bytes may be reused; an external fetch is allowed only by
   the selected policy and only if its bytes match the declared digest. Do
   not let a source URL define object identity.
4. Start the existing process/OCI Adapter and verify the original K against
   the actual Run Port. Dependency resolution cannot rewrite K or D.

An OCI offline payload must preserve and verify the registry manifest and
layer/config blob digests that justify D's pinned image digest and platform.
An arbitrary `docker save` tar is not sufficient evidence: some formats omit
the registry manifest. Docker Engine 29's OCI-layout save archive for the
Datasette image contains that manifest and all referenced blobs. The exporter
checks the exact raw manifest hash against D, all config/layer hashes and
sizes, the platform, and the Docker load manifest before accepting the tar.
The Runner loads only the declared platform, inspects the loaded local image
by the verified manifest or config digest, and runs that local ID with
`--pull=never`. A tagless archive need not create a `repository@digest` alias
in Docker's image store; the absence of that alias must never trigger a
registry pull or permit an unrelated local image. The receipt continues to
name D's pinned registry digest, not the physical local load ID.
The archive hash itself is not D identity. OCI archive/chunk bytes
belong to the transport and are bounded; the exporter must not increase the
hosted upload limit or rely on an untracked side channel to claim acceptance.

## Acceptance

- Thin and offline Datasette exports have equal ContractRef and equal
  DerivationRefs for matching routes, while their bundle hashes/sizes differ.
- Thin fails `dependency_unavailable` under registry/index blocking; offline
  succeeds on a compatible host with all external dependency endpoints blocked
  and no fetch attempted.
- Embedded wheel, OCI layer/config/manifest, and workspace blob tampering or
  omission fail validation. A fetched object with the wrong digest is refused.
- Canonical sorting makes input packing order irrelevant to K and D.
- CLI export planning prints object sizes, total estimated size, required
  host capabilities, and the network guarantee before writing the bundle.

This RFC does not authorize a new scheduler, source re-Formation, GPU support,
production deployment, or durable saved-data capture.
