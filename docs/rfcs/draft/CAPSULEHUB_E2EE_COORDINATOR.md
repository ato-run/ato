# Capsulehub — E2EE blind capsule registry coordinator

Status: Draft (reviewed 2026-11-09; v0 profile scope and key-handling trust
boundary revised on review; no implementation yet)
Owner: ato CLI / Objects
Related: [K_D_MODEL](K_D_MODEL.md),
[PORTABLE_APPLICATION_BUNDLE_V3](PORTABLE_APPLICATION_BUNDLE_V3.md),
[PORTABLE_LOCAL_INSTANCE](PORTABLE_LOCAL_INSTANCE.md),
[CAPSULE_OBJECT_TRANSPORT_CLIENT](CAPSULE_OBJECT_TRANSPORT_CLIENT.md)

## Purpose

Capsulehub is a distribution channel for already-formed `.capsule` bundles that
is separate from the Store pipeline. The organizer holds a user-supplied link
and retrieves the capsule without registering anywhere and without the hub
being able to read the capsule content.

The implementation target is a **self-hosted coordinator in this repository**:
any operator can run one, and `ato.run/capsulehub` is a hosted deployment of
that same coordinator, not a distinct service. Store publishing, category,
review, and discovery flows are unchanged and remain separate.

## Non-goals

- No server-side content inspection, validation, scan, or category. The
  coordinator stores opaque ciphertext and nothing else.
- No discovery, search, ranking, or listing. Links are shared out of band.
- No author identity, publisher review, or Store admission.
- No new semantic root. A capsule hub entry is transport evidence; identity
  stays `ContractRef` per the K/D model. Game, adult, or any other content
  genre adds nothing to the model.
- No mirroring, federation, or replication in the first increment.
- No change to Store publishing or the hosted-import quarantine path, which
  remains the authority for ato.run-hosted runs.

## Trust model

The coordinator is **not trusted for integrity or confidentiality**:

1. Confidentiality — the client encrypts the bundle client-side before upload.
   The coordinator only ever receives ciphertext. The encryption key is
   encoded in the shared link fragment (`#…`), which browsers and HTTP clients
   do not transmit. Key bytes in logs, storage, or API payloads are a
   coordinator bug, not a design assumption.
2. Integrity — the hub object id is derived from the uploaded ciphertext
   (`sha256:<hex>`). Pull-side comparison of the served bytes against the id
   detects any coordinator-side modification or truncation.
3. Identity — the link additionally carries the claimed CapsuleId
   (`root_contract_ref`). Pull recomputes the ContractRef from canonical
   `ato.contract/1` bytes after decryption and refuses to proceed when the
   recomputation disagrees with the link claim. This keeps the coordinator
   unable to swap contents between ids it serves.

### Key-handling clients are a separate trust root (v0)

The fragment keeps keys out of ordinary HTTP requests, but it does **not**
protect the key from code the hub serves: any script executing on a hub page
(e.g. a browser tab at `/c/`) can read `location.hash`. A malicious or
compromised coordinator can therefore recover keys simply by swapping its
own delivered HTML. The fragment is a transport property, not an
origin-blinding guarantee.

Accordingly, v0 is CLI/desktop-only for key handling:

- the trusted local CLI/desktop is the only client that parses link keys,
  decrypts, and verifies; key-bearing links are **never** opened against
  hub-served HTML, and the coordinator never becomes the decryption front end;
- browser consumption of capsulehub links is out of scope for v0. If added
  later, the key-handling client assets must come from a trust root separate
  from the coordinator (with non-collusion and integrity defined there), and
  the confidentiality claim must be rewritten against that trust root, not
  inherited from this document.

## "Formed capsules only" is a client-side admission rule

The coordinator cannot enforce that it stores formed capsules; it cannot read
them. The rule is therefore implemented on both client ends:

- `ato hub push` validates the bundle **before** encryption: dispatch on
  `index.version` / profile, fail closed on unknown profile or fields, and
  for contract-root v3/v4 recompute `root_contract_ref` from canonical
  `ato.contract/1` bytes. v0 additionally rejects computation-root v2 (see
  Profile scope) and validates all declared Derivations, as the local Run
  path does. A bundle that fails that validation must not be pushed.
- `ato hub pull` re-runs the full validation after decryption and again fails
  closed before import or Run.

This asymmetry (server cannot enforce, clients must) is a documented property
of the design, not a defect to hide.

## Profile scope (v0): contract-root v3/v4 only

v0 handles **only** contract-root bundles, with the version/profile pairing
the validator enforces:

| wire `index.version` | `index.profile` |
|---|---|
| `3` | `ato.portable-application/1` |
| `4` | `ato.portable-application/2` |

The computation-root v2 format is a different reference domain — its root is
a `ComputationRef`, which is never cast into or read as a `ContractRef` — and
the current decoder already dispatches it into a separate document variant.
A v0 client therefore:

- rejects computation-root v2 (and any unknown version/profile) client-side,
  naming the mismatch — the hub never silently relabels its identity domain,
- validates at the natural client boundary: push-side validation runs
  **before encryption**, pull-side **after decryption and before import or
  Run**, and
- carries an identity claim field that names its own reference domain, so a
  future profile extension must add its own claim kind rather than reinterpret
  `cid`.

Client-side formed validation reuses the existing validator rather than
re-coding it: validate the schema/canonical digests/closure of **all declared
Derivations** (the `validate_bytes_all` path), and for v4 additionally verify
that the Instance snapshot is bound into K (the snapshot requirement must
reference the indexed snapshot). Push-side and pull-side run the same
validation; only the direction differs.

## Wire sketch (first increment)

One coordinator stores opaque objects addressed by their own ciphertext
digest:

```text
PUT    /v1/capsulehub/objects/{sha256-hex}    upload ciphertext
GET    /v1/capsulehub/objects/{sha256-hex}    download ciphertext
HEAD   /v1/capsulehub/objects/{sha256-hex}    existence + size
DELETE /v1/capsulehub/objects/{sha256-hex}    removal by id (operator/abuse)
```

The shared link has the shape:

```text
https://<hub base>/c/#o=<object id>&k=<key>&cid=<contract ref>
```

The link is a user artifact, not a coordinator state: the coordinator has no
account, no registry row, and no knowledge of the tuple.

## CLI

```text
ato hub push <selector> --hub-url <base>
ato hub pull <link>
```

Conventions follow the Capsule Object Transport Client: configuration may come
from `ATO_HUB_URL`; there is deliberately **no production default URL**; loop
HTTP is a coordinator deployment decision and must be made explicit in the
client rather than silently allowed. A pull that fails ciphertext digest,
decryption, bundle validation, or ContractRef recomputation fails closed with
the failing boundary named.

## Coordinator deployment policy

The coordinator is small and boring on purpose. Per-deployment knobs:

- maximum object size and rate limits (required at deployment),
- upload auth mode: `open` | `token` (coordination instance may also admit
  fully anonymous reads; there is nothing to authorize for reads),
- retention (default persistent; optional TTL garbage collection),
- storage backend (filesystem first; object stores are a later concern).

## Abuse and operations obligations

A blind hub cannot inspect content, so it cannot enforce law, age policy, or
license terms over the content it relays. Operators must accept that:

- takedown is possible only by object id (reported links), never by content
  classification,
- the operations policy (removal contact, retention, limits, jurisdiction) is
  a deployment duty and must be published where links point,
- this liability posture is why capsulehub is deliberately separate from the
  Store and from ato.run's normal content surfaces.

These obligations belong in the coordinator deployment documentation at
implementation time, not as an afterthought.

## Relationship to the Store

| | Store | Capsulehub |
|---|---|---|
| Admission | author review pipeline | client-side formed-capsule rule |
| Identity | Application / Release / K/D | ContractRef claim in the link |
| Visibility | listed catalog | unlisted links |
| Server trust | full | none (blind ciphertext store) |

## Undecided (needs review)

- AEAD and KDF choices (candidate: XChaCha20-Poly1305 with random nonce; key
  derivation for the fragment form).
- A future browser key-handling client: the separate trust root for its
  assets and the conditions under which the fragment claim could change are
  deliberately out of v0 scope (see the key-handling section).
- Local HTTP loopback admission rule for self-hosted coordinators.
- Whether `hub pull` imports a durable local Instance (PORTABLE_LOCAL_INSTANCE
  path) or only offers an ephemeral run in v0.
- Whether bundle signatures (`signatures` entries in the v3 envelope) are
  verified client-side on pull in v0 or deferred.
- Streaming/multipart for large artifacts; the native-build sizes from
  [PORTABLE_NATIVE_DERIVATIONS](PORTABLE_NATIVE_DERIVATIONS.md) set the real
  limit.
- Coordinated deletion vs retention as the abuse-removal default when the
  coordinator is operated without an abuse contact.

## First consumer capsules (from the 2026-11-09 discussion)

- static-web game capsules as the smallest end-to-end shape: seal → hub push →
  share link → hub pull → run,
- a PSP player capsule as the flagship: one K, multiple D (static-web wasm and
  native builds, see [PORTABLE_NATIVE_DERIVATIONS](PORTABLE_NATIVE_DERIVATIONS.md)),
  with user-supplied ROM/BIOS as Bindings that are never part of identity.
