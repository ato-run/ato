# Static Web Bundle Producer v1

## Status

Draft. This document defines a pure producer boundary only; it does not change
the snapshot-builder daemon's claim loop, Firecracker lifecycle, or any
production deployment behavior.

## Decision

An explicit `StaticWebOutputPlan` selects the lane. It carries a caller-owned
materialization id, a relative built-image output root, entry path, SPA routing
flag, and public `connect-src` origins. The producer must never infer the lane
from the existence of `dist/`, rerun a build, or execute a container.

The initial eligible lane is the existing Vite production image build on
`feat/surface-activation-v2`: `vite build` runs while assembling the read-only
guest image and `vite preview` serves the resulting output. The extraction
adapter copies the chosen output from a mounted/exported built image to a
temporary workspace and deletes that workspace on drop.

## Artifact

The producer emits `static-web-bundle-v1/` with:

- `manifest.json`: exact RFC 8785 JCS bytes for `ato.static-web-manifest/v1`, no newline;
- `receipt.json`: manifest identity, environment labels, R2 object keys, and blob metadata;
- `blobs/sha256/<hex>`: immutable SHA-256 identity blobs.

The `p-*` and `s-*` labels are derived from the digest of the complete
canonical manifest, not a digest of static file content. In particular,
`materialization_id` is part of that manifest: changing it changes canonical
bytes, the manifest digest, and both host labels. Sharing content-addressed
blobs does not imply sharing a public deployment host.

R2 publication is intentionally out of scope. A later API/control-plane PR may
upload the exact files and create mutable deployment records only after durable
artifact registration. The VM Snapshot path remains a separate output boundary;
neither output replaces or weakens the other.

## Safety

The closure rejects links, special files, unsafe/non-NFC paths, source maps,
oversized trees, unsupported MIME types, and runtime secret canary hits. This
is a scan for known runtime-secret bytes supplied by a future builder caller;
an empty canary list does not assert that generic secret detection succeeded.
Actual builder integration must require typed evidence that distinguishes
`no_runtime_secrets` from `runtime_secret_canaries_scanned`; that evidence is
deferred from this pure producer. The schema,
canonical bytes, SHA-256, lower-case base32 host labels, R2 keys, and blob
metadata are aligned to the merged `ato-contents` Worker contract fixture.
