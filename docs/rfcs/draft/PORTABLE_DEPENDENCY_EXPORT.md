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
must not be reinterpreted as a sparse bundle. A new versioned transport
envelope is required for externalized object payloads. Semantic structured
objects, including K, Application, D, and tree descriptors, remain embedded
so that a validator can prove their references before any network request.
Only explicitly described immutable dependency blobs may be external.

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

The transport metadata reports the profile, embedded/external counts and
bytes, required host capabilities, and estimated encoded size. These values
are derived from the validated transport descriptor; a caller-provided
`network_required: false` is not accepted as evidence. The selected profile
and sources are excluded from K and D. The runtime receipt records which
objects came from the bundle, a local verified cache, or a network source.

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
A `docker save` tar alone is not sufficient evidence of the original registry
manifest digest: it may omit that manifest and carries a different archive
hash. Such a tar may be used as a Docker loading optimization only after the
semantic OCI content graph is independently verified. OCI archive/chunk bytes
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
