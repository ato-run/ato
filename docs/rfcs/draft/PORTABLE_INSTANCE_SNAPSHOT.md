# Portable Instance Snapshot

Status: draft implementation contract  
Date: 2026-09-17

## Context

A portable Application bundle describes an immutable Application, one Contract
K, and one or more Derivations. That is sufficient to realize the same app in a
different place, but it does not preserve data created while a person uses one
Instance.

Saved data is not transport metadata. If the data a receiver must restore has
changed, the observable Contract has changed too. Conversely, local Instance
IDs, Asset IDs, storage paths, signed URLs, and credentials are placement state
and must not enter K or D.

## Decision

Portable bundle v4 may name one saved-state object from
`index.instance_snapshot_ref`:

```json
{
  "schema": "ato.portable-instance-snapshot/1",
  "resources": [
    {
      "slot": "main",
      "protocol": "ato.data.json@1",
      "content_ref": "sha256:..."
    }
  ],
  "assets": [
    {
      "alias": "asset-1",
      "content_ref": "sha256:...",
      "filename": "photo.jpg",
      "content_type": "image/jpeg",
      "size": 123456
    }
  ]
}
```

The v0 resource protocols are:

- `ato.browser-instance-state@1`
- `ato.data.json@1`

Slots and aliases are sorted and unique. Asset filenames are metadata, not
paths. Every content reference is canonical SHA-256 and every referenced byte
object is embedded in the v4 closure. A v3 bundle rejects
`instance_snapshot_ref` instead of reinterpreting its profile.

## Identity

Snapshot export adds exactly one requirement to the existing Contract:

```json
{
  "id": "instance-snapshot",
  "verifier": "ato.contract.instance-snapshot@1",
  "digest": "sha256:<portable-instance-snapshot>"
}
```

Therefore:

```text
K_base + saved snapshot identity = K_saved
D_before                         = D_after
```

Changing only thin, cached, or offline packing keeps both K and D unchanged.
Changing saved data mints a new snapshot object and a new K while preserving
the declared DerivationRefs. Replacing a snapshot removes objects that are no
longer reachable, but retains content shared by another root.

## Validation and verification

Bundle validation verifies the index reference, Contract binding, schemas,
canonical bytes, object digests, exact closure, supported resource protocols,
Asset sizes, and embedded content. It does not claim that a runtime restored
the data.

`ato.contract.instance-snapshot@1` is satisfied only when the runtime launch
path reports the exact snapshot digest it installed into the receiving
Instance for the current Run. Merely validating or unpacking the bundle is not
runtime evidence. A runtime without a successful snapshot restore must fail
with `instance_snapshot_missing`; it must not copy the index value into its
observation.

For Hosted import, the Rust validator authenticates the snapshot descriptor and
each content object's digest before the API can restore anything. The API
creates fresh receiving-side Resource and Asset identities, persists the exact
restored snapshot digest with the published import, and only creates a runtime
verification job from that persisted evidence. A publication failure after a
dynamic launch requests that lease to stop. Runtime verification receives the
persisted restore evidence, not a client assertion or an unauthenticated
bundle-index value.

## Local Instance materialization

A durable local import creates an Instance-owned materialization:

```text
instances/<instance-id>/
  instance.json
  snapshots/<snapshot-digest>/
    snapshot.json
    resources/000000.bin
    assets/<new-local-asset-id>/body
```

The portable alias is rebound to an Instance-local Asset ID derived in that
Instance namespace. Two independent imports therefore have different Asset
IDs while preserving the same body SHA-256. The exporter never copies an
origin Instance ID, Asset ID, signed URL, grant, owner ID, or secret.

Snapshot and bundle files are prepared before `instance.json` is atomically
replaced. An active Run fences snapshot updates: the Instance must be stopped
before new saved data can be sealed. Old immutable snapshot directories may be
garbage-collected later; they are not selected after metadata commit.

`ato app export <instance-id>` writes the bundle currently selected by the
Instance. Library callers may seal a new `InstanceSnapshotV1` first; this
upgrades v3 transport to cached v4, stores the new immutable bundle, and updates
Instance metadata only after snapshot materialization succeeds.

## Security boundaries

- Snapshot content is limited to declared resource slots and Assets; arbitrary
  filesystem capture is not inferred.
- Relative materialization paths are generated locally and revalidated before
  use.
- Asset body digest and declared size are checked after materialization.
- Secrets, Binding values, cookies, signed URLs, and credentials are excluded.
- Runtime verification remains separate from import-time integrity checks.

## Current implementation boundary

This increment implements the v4 wire object, K binding, semantic and closure
validation, replacement/pruning, durable local materialization, independent
Asset IDs, local saved-snapshot sealing, current-Instance export, and Hosted
restore of browser state, JSON Data Resources, and Assets before launch. The
PWA requires the exact restored snapshot observation before a snapshot-bearing
import can become Ready and displays the saved-data restore result separately.

It does not yet implement browser-state flush/capture from a live Hosted
Instance, PWA export UI, field-aware Asset alias resolution inside saved JSON,
or Process/OCI filesystem-state injection. No staging acceptance has yet
proved the Hosted roundtrip; the current Hosted implementation is covered by
the validator/API/PWA integration tests described in the progress record.

## Acceptance properties

- Two imports have different Instance IDs and Asset IDs.
- Imported Asset bytes retain the declared SHA-256.
- Saved-data changes change ContractRef and preserve DerivationRef.
- Repacking preserves ContractRef, DerivationRef, and snapshot ref.
- Missing or altered snapshot content fails before route selection.
- Altered materialized content fails before use.
- An active Run cannot seal a new snapshot.
- Runtime acceptance requires an exact restore observation.
- Hosted import restores before launch and never reuses origin Instance,
  Resource, or Asset IDs.
