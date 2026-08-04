---
title: "Capsule Bundle Format v3 (source-only import)"
status: accepted
date: "2026-08-04"
author: "@egamikohsuke"
ssot:
  - "crates/capsule/src/import_bundle/"
related:
  - "SIGNATURE_SPEC.md"
  - "TRUST_AND_KEYS.md"
---

# Capsule Bundle Format v3 (source-only import)

## Scope

This document defines the `.capsule` bundle container format **v3**, a distinct
container-format revision from the current writer output in
`crates/capsule/src/packers/capsule.rs` (referred to below as **v2**).

v3 is scoped to **Slice 1: source-only import**. A v3 bundle carries a
canonical source archive plus an authoritative manifest, and is consumed by
the Import Adapter pipeline (`crates/capsule/src/import_bundle/`) — never
executed directly. VM Ready-State Seal, OCI, and Wasm materializations, and
`capsule.lock` inclusion, are explicitly out of scope for v1 of this format
and are deferred to a later slice (see [Non-goals](#non-goals)).

v2 (`capsule.toml` / `capsule.lock.json` / `sbom.spdx.json` / `signature.json`
/ `payload.tar.zst`) is **not replaced**. It remains the writer for
`PublishProfile::Artifact` / `PublishProfile::Source` builds and keeps its own
read path. v3 is a new, additional writer and reader for the source-only
import use case; `packers/capsule.rs` is not modified by this RFC.

## Container layout

```text
.capsule (v3)
├── index.json          # ato.capsule-index/v1 — exact member manifest
├── signature.json       # ato.capsule-index-signature/v1 — signs index.json
├── capsule.toml          # outer, authoritative manifest
└── source.tar.zst        # existing ato.source-archive/v1 encoding
```

Outer container is an uncompressed PAX TAR, consistent with the existing v2
writer. **Exact allowlist** — each of the four entries above appears exactly
once; any other outer entry (including a `README.md`, a stray `capsule.lock`,
or a second `capsule.toml`) is rejected. Path traversal (`..`), absolute
paths, symlinks, hardlinks, device files, and FIFOs are rejected for every
outer entry and for every entry inside `source.tar.zst` during its own
established verification.

`capsule.lock` is **not a valid member of a v1 source-only v3 bundle.** A
`role: "lock"` entry in `index.json`, or a `capsule.lock`/`capsule.lock.json`
outer entry, causes the bundle to be rejected as malformed. Rationale: a
cloud-built lock can carry cloud-side Execution Contract and
platform-specific resolution that must not silently govern a local install on
a different machine. The local resolver generates `capsule.lock` after
import, inside the existing install pipeline (see
[Program authority](#program-authority-outer-manifest-vs-archive-contents)).
A bundle format admitting an optional, Execution-Contract-bound lock is
deferred to the slice that defines VM/OCI/Wasm materializations.

## `index.json` — `ato.capsule-index/v1`

```json
{
  "schema": "ato.capsule-index/v1",
  "members": [
    {
      "role": "manifest",
      "path": "capsule.toml",
      "media_type": "application/toml",
      "sha256": "sha256:...",
      "size_bytes": 1234
    },
    {
      "role": "source",
      "path": "source.tar.zst",
      "media_type": "application/vnd.ato.source-archive.v1+zstd",
      "sha256": "sha256:...",
      "size_bytes": 5678
    }
  ]
}
```

Invariants (all enforced structurally, before any signature or trust
decision — see [Verification](#verification-two-stage-api)):

- `manifest` role: exactly 1, `path` MUST be `capsule.toml`
- `source` role: exactly 1, `path` MUST be `source.tar.zst`,
  `media_type` MUST be `application/vnd.ato.source-archive.v1+zstd`
- `lock` role: rejected in v1 (see above)
- any other `role` value: rejected
- `path` values MUST be unique (duplicate `path` is rejected even across
  different `role`s)
- unknown top-level or per-member JSON fields: rejected
- **duplicate JSON object keys within a single member or the top-level
  object: rejected.** A generic JSON map parser that silently keeps the last
  occurrence of a duplicate key MUST NOT be used to parse `index.json` — a
  parser that detects and rejects duplicate keys is required, because a
  duplicate-key `index.json` is exactly the shape an attacker would use to
  make the signed bytes say one thing while a lenient parser reads another.
  The golden vector suite (see [Golden vectors](#golden-vectors)) includes a
  duplicate-key case.
- every declared `size_bytes` and `sha256` MUST match the actual member
  bytes exactly. **`size_bytes` is untrusted input and MUST NOT be used to
  pre-allocate buffers** — see [Resource policy](#resource-policy-not-a-format-limit).

`index.json` bytes on disk MUST be the exact
[JCS (RFC 8785)](https://datatracker.ietf.org/doc/html/rfc8785) canonical
encoding of the member list. A writer that emits non-canonical bytes produces
an invalid bundle; a reader that receives bytes differing from the JCS
canonicalization of their own parsed content rejects the bundle (this is what
makes `index.json` a well-defined signing target — see next section).

## `signature.json` — `ato.capsule-index-signature/v1`

Reuses the Ed25519 / `did:key` conventions from `SIGNATURE_SPEC.md` (BLAKE3
there is for whole-file `.sync` hashing; here the target is the JCS bytes of
`index.json`, not the outer TAR bytes, since outer TAR byte order is not
semantically meaningful).

```json
{
  "schema": "ato.capsule-index-signature/v1",
  "algorithm": "ed25519",
  "key_id": "did:key:z6Mk...",
  "claimed_issuer": "local-author | publisher | ato-store",
  "index_digest": "sha256:...",
  "signature": "base64url..."
}
```

- **Signed bytes** (domain-separated):
  `UTF8("ato.capsule-index-signature/v1") + 0x00 + <exact JCS bytes of index.json>`
- `index_digest` = `sha256:` of the same JCS bytes; a reader recomputes it and
  rejects on mismatch before even attempting signature verification
- **`claimed_issuer` is self-declared and display-only. It MUST NOT be used
  in any trust decision** — an attacker can write `"claimed_issuer":
  "ato-store"` on any bundle they sign with their own key. Trust is derived
  exclusively per [Signer trust](#signer-trust-not-just-signature-validity)
  below. This rule is fixed by a golden vector and a corresponding unit test
  in the API and CLI verifiers.
- **v3 requires a present, structurally valid signature.** An outer bundle
  with no `signature.json`, or one that fails to parse, is rejected
  (`SignatureValidity::Absent` is a rejection outcome for v3, not a
  degrade-to-unsigned path). This intentionally differs from `capsule.lock`
  handling: signature presence is a format-level structural requirement,
  trust in *who* signed is a separate, policy-level question (next section).

## Signer trust (not just signature validity)

Signature validity and signer trust are two independent axes and must not be
conflated:

```text
SignatureValidity = "valid" | "invalid" | "absent"
SignerTrust        = "trusted_store" | "trusted_publisher" | "trusted_local_key" | "untrusted_key"
```

`SignerTrust` is **never** derived from `claimed_issuer`. It is derived from:

- `trusted_store` — the signing key matches a Store distribution public key
  pinned to the API origin the bundle was fetched from (see
  [Store trust roots](#store-trust-roots) below), or a signed key-rotation
  record chains to one (per `TRUST_AND_KEYS.md` §3.2, `previous_key` /
  30-day grace window convention — reused here, not reinvented)
- `trusted_publisher` — **deferred past Slice 1.** There is currently no
  mechanism for a publisher's private key to reach a builder process, so no
  bundle can legitimately claim this trust level yet. The enum value is
  reserved so API responses and CLI trust output are stable across slices,
  but Slice 1 code MUST NOT produce it.
- `trusted_local_key` — the fingerprint of the signing key has previously
  been recorded by the user via TOFU, per `TRUST_AND_KEYS.md` §2.1
  (`~/.capsule/trust_store.json`). **Deferred past Slice 1** — see
  [Slice 1 signer policy](#slice-1-signer-policy).
- `untrusted_key` — none of the above. Signature is structurally valid
  (integrity holds — the bytes have not been tampered with since signing),
  but the signer's identity carries no established trust.

### Slice 1 signer policy

Only two of the four `SignerTrust` values are actually produced in this
slice:

```text
Store export (ato-api export job, see the ato-api-side plan)
  → signs with a pinned Ato Store distribution key
  → readers resolve this to SignerTrust::trusted_store

Local `ato capsule export` (this repo, PR A1)
  → signs with a bundle-scoped ephemeral Ed25519 key,
    generated fresh, used once, and discarded — never persisted
    with `StoredKey` (crates/capsule/src/foundation/types/signing.rs),
    since that type stores secret_key as plaintext base64 JSON and is
    not a fit for a key that must not outlive the signing operation
  → readers resolve this to SignerTrust::untrusted_key
```

**Store Install requires**: `SignatureValidity::Valid AND SignerTrust ∈
{trusted_store, trusted_publisher} AND recomputed capsule_program_id matches
the value asserted by the API AND recomputed bundle digest matches the
downloaded bytes.` A bundle fetched via Store Install that resolves to
`untrusted_key` is rejected outright — there is no confirmation prompt on
this path, because the whole point of Store Install is that the API is the
trust anchor.

**Local file import requires**: signature present and structurally valid
(integrity), but `SignerTrust::untrusted_key` is accepted **with an explicit
user/importer confirmation** before the bundle is admitted (CLI prompt or, on
the PWA side, an `author_confirmation_receipt` recorded on the Authoring
Session before publish is allowed — see the ato-pwa-side plan). This is
Option B from the design discussion: no new device trust-store
infrastructure is built in this slice. Option A (persisting a confirmed
fingerprint into `~/.capsule/trust_store.json` so a second import from the
same local key upgrades silently to `trusted_local_key`) is deferred; nothing
in this format precludes adding it later.

### Store trust roots

Store public keys are pinned per API origin, not looked up dynamically:

```text
https://api.ato.run          → production Store public key set
staging API origin            → staging Store public key set
explicit ATO_STORE_API_URL override → untrusted unless a matching pin is configured
```

Each origin pins an array of **up to 2** keys (current + next), enabling the
`TRUST_AND_KEYS.md` §3.2 rotation flow (new key signs, `previous_key` is
carried for the grace window, old key is dropped after). General-purpose
key-rotation infrastructure beyond this 2-key array is not needed for Slice
1.

## Program authority: outer manifest vs. archive contents

`source.tar.zst` is the existing, already-deterministic
`ato.source-archive/v1` encoding — **not** a new source-projection format.
This is a deliberate reuse decision: `source_materializations` rows already
hold this exact archive shape, so a Store-exported v3 bundle can point at an
existing archive without re-packing it.

Because `ato.source-archive/v1` is a full checkout snapshot, it may contain
its own `capsule.toml` / `capsule.lock` / `ato.lock.json` at its root (e.g. a
GitHub-sourced archive naturally has these). **The outer `capsule.toml` is
authoritative; anything with the same name inside `source.tar.zst` is
excluded from the Program Source Projection and MUST NOT influence
`capsule_program_id`, execution, or install behavior.**

The existing public mint,
`VerifiedPinnedSourceMaterialization::from_source_archive`
(`crates/capsule/src/contract/program_source_projection.rs`), is documented
as "the only public mint; no directory is self-attested" and always treats
the archive's own root `capsule.toml` as authoritative — it has no parameter
for an external manifest override. **This RFC does not change that
function.** A new function is added alongside it for the import path:

## Verification: two-stage API

```rust
pub struct VerifiedCapsuleEnvelope {
    // signature valid, index members verified (digest + size), private
    // staging populated, member ownership held — outer-manifest authority
    // NOT yet applied
}

pub struct VerifiedCapsuleImport {
    // outer capsule.toml applied over the source projection,
    // capsule_program_id re-derived, runnable workspace contents decided
}

pub struct ImportedCapsuleWorkspace {
    // owns the TempDir and the derived capsule_program_id; the only type
    // the existing install pipeline (resolve_authoritative_input /
    // install_local_directory) is handed
}

pub fn verify_capsule_envelope(
    reader: impl Read + Seek,
    expected_bundle_digest: Option<Sha256Digest>,  // Store Install: required. Local file: optional.
    trust_policy: &CapsuleTrustPolicy,
    import_policy: &CapsuleImportPolicy,
) -> Result<VerifiedCapsuleEnvelope, CapsuleImportError>;

pub fn derive_imported_capsule(
    envelope: VerifiedCapsuleEnvelope,
) -> Result<VerifiedCapsuleImport, CapsuleImportError>;

impl VerifiedCapsuleImport {
    pub fn into_workspace(self) -> ImportedCapsuleWorkspace;
}
```

`derive_imported_capsule` performs, inside a single process-private staging
directory:

```text
1. verify the source archive's encoded digest (already checked structurally
   in verify_capsule_envelope; re-asserted here as a precondition)
2. extract the archive into private staging
3. run existing A1v2 admissibility checks over the extracted tree
4. identify root control files (capsule.toml, capsule.lock, ato.lock.json)
5. exclude those control files from the executable input set
6. parse and normalize the outer capsule.toml
7. compute the control-file-excluded source projection
8. derive capsule_program_id from (outer manifest, source projection)
9. write only the outer capsule.toml into the runnable staging area
10. hand off to the existing install pipeline with no lock present
    (steps continue in crates/cli's resolve_authoritative_input /
    install_local_directory → build/resolve/smoke → InstallRevisionFinalizer,
    which is where capsule.lock gets generated by the local resolver)
```

`FileCapsuleReader` (local `.capsule` path) and the Store Install path (an
existing-distribution URL downloaded to a temp file — no `RemoteCapsuleReader`
exists; both consumers call `verify_capsule_envelope` with the same reader
trait over different `Read + Seek` sources) share this exact function. This
is what makes file-sharing and Store Install provably go through the same
byte-level verification path, not just structurally similar ones.

Golden-vector cases for `derive_imported_capsule` (§[Golden vectors](#golden-vectors)):
inner manifest differs from outer manifest; inner manifest absent; inner
manifest malformed; inner `capsule.lock` present; both `capsule.lock` and
`ato.lock.json` present inside the archive; outer manifest bytes tampered
after signing. In every case, the inner control file MUST NOT be able to
influence the derived `capsule_program_id` or the runnable workspace
contents.

## v2 / v3 dispatch

```text
index.json present at the outer archive root
  → the bundle MUST be validated as v3
  → an invalid index.json or signature.json is a rejection, never a
    fallback to v2 parsing

index.json absent
  → the reader accepts the bundle only if it matches the EXACT v2 outer
    shape: capsule.toml, capsule.lock.json, signature.json, payload.tar.zst,
    all required, plus the OPTIONAL sbom.spdx.json member that the current
    v2 writer (packers/capsule.rs) may emit. Any other shape without
    index.json is rejected by both readers.
```

There is no implicit v2-to-v3 upgrade path. A v2 bundle is never silently
reinterpreted as v3.

## Resource policy (not a format limit)

**This format defines no normative limit on bundle size, member size,
expanded size, or member count.** Importers MUST apply an
implementation-defined resource policy before or during materialization.
Exceeding that policy does not make the bundle malformed — it is a distinct,
non-`capsule_invalid` outcome:

```text
capsule_invalid            structural/signature/digest violation — the bundle itself is wrong
upload_too_large            this upload endpoint's configured limit (API-side, environment policy)
storage_quota_exceeded      account/organization quota (API-side, environment policy)
resource_budget_exceeded    this import worker cannot process it right now (implementation policy)
insufficient_local_storage  local device is out of disk space (implementation policy)
```

```rust
struct CapsuleImportPolicy {
    temporary_storage_budget: Option<u64>,
    available_disk_bytes: Option<u64>,
    max_concurrent_imports: Option<u32>,
}

struct CapsuleTrustPolicy {
    store_key_pins: Vec<PinnedStoreOrigin>,  // per-origin, up to 2 keys each
    accept_untrusted_with_confirmation: bool,
}
```

A verifier streams extraction, hashing, and staging — it never pre-allocates
based on a declared `size_bytes`, and enforces `CapsuleImportPolicy` limits
incrementally as bytes are processed, so a bundle that lies about its own
size in `index.json` is caught by the digest/size mismatch check
(`capsule_invalid`), not by an allocation failure.

## Golden vectors

`crates/capsule/tests/` gains a `capsule_format_v3/` fixture directory with
paired valid/invalid vectors. Required invalid cases (non-exhaustive floor —
implementers should add more as edge cases are found):

- duplicate outer member path
- duplicate outer member with different role
- `role: "lock"` present
- unknown `role` value
- unknown top-level or member JSON field in `index.json`
- duplicate JSON key inside `index.json`
- `index.json` bytes not exact JCS canonicalization of their own content
- member digest mismatch
- member `size_bytes` mismatch
- `signature.json` absent
- `signature.json` present but invalid (bad signature bytes)
- `index_digest` mismatch
- `claimed_issuer: "ato-store"` on a bundle signed by an unpinned key (must
  resolve to `untrusted_key`, not `trusted_store` — the specific regression
  this format is designed to prevent)
- path traversal / absolute path / symlink / device / FIFO in outer archive
- inner manifest differs from outer manifest
- inner `capsule.lock` present alongside outer `capsule.toml`
- both `capsule.lock` and `ato.lock.json` present inside `source.tar.zst`
- outer `capsule.toml` bytes tampered after signing (index_digest mismatch)
- v2-shaped bundle that is missing one required v2 member (must be rejected
  by the v2 reader, not silently accepted as v3)

## Non-goals

Deferred to a later slice, not addressed by this format revision:

- VM Ready-State Seal, OCI, and Wasm materialization members
- `capsule.lock` inclusion in the bundle (Execution-Contract-bound lock
  semantics need to be defined first)
- `trusted_publisher` signer trust (publisher key delivery to builders)
- `trusted_local_key` via persisted TOFU fingerprint (Option A)
- general key-rotation infrastructure beyond the 2-key pin array
- `source_snapshots` (legacy) / `source_revisions` (current) consolidation
- unifying `capsule::adapters::capsule::CasStore` and
  `capsulefs::BlobManifest` into one CAS
- auditing `packers/bundle.rs` (the unrelated self-extracting nacelle
  bundle format)
