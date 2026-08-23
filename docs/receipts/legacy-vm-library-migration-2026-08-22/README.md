# Legacy VM Library migration restack receipt

Status: partial; Draft PR stack implemented, staging VM acceptance blocked

## Restack

- repository: `ato-run/ato`
- branch: `feat/legacy-vm-library-migration-v1`
- Draft PR: `https://github.com/ato-run/ato/pull/1297`
- base: `feat/capsule-object-transport-client-v1`
- base head after the final uploader receipt update: `29128c0b`
- legacy compatibility base retained in ancestry: `ato#1291` at
  `509a05028448dccc684be7769e9d085d39102b6a`
- restack commits:
  - `c34f0312` — VM uploader receipt binds descriptor/frontier
  - `dbd90df7` — uploader receipt documentation
  - `5918cfdd` — blocked staging acceptance receipt
- expected initial heads were fetched and matched on 2026-08-23:
  - `ato#1291`: `509a05028448dccc684be7769e9d085d39102b6a`
  - `ato-api#518`: `b457e2db9b311ec5d28a9f3ed9d5f95a30ec66f1`

The branch inherits the operation Record model, asynchronous Record Writer,
Contract registry, explicit Realization Planner, generic VM Snapshot
Materializer, and content-addressed uploader. The old snapshot inspector and
runtime cleanup fixes from `ato#1291` remain compatibility and investigation
tools; no legacy snapshot identity is converted into a ComputationRef.

## 2048 acceptance state

- candidate: 2048 (MIT; canonical source lineage retained from the prior
  receipt)
- target ComputationRef: not selected
- RecordFrontier ref: not captured
- replay descriptor ref: not generated for this acceptance
- VM materialization descriptor ref: not generated
- Firecracker version: unavailable on the macOS arm64 development host
- staging runner capabilities: not probed by this branch
- object count / logical bytes / unique uploaded bytes: 0 / 0 / 0
- fresh VM restores: 0/3
- Feed -> Detail -> Continue VM path: not run
- browser keyboard/state-change Contract: not run
- cleanup after a real Firecracker restore: not run

These fields are deliberately not populated from VM bytes, old Snapshot IDs,
Record ordering, or RecordFrontier identity.

## Blocking boundary

The generic backend and fake-backend lifecycle are implemented, but the
default CLI/runtime has no `ActiveVmCaptureSource` bound to a live staging
Firecracker Realization. The server has a graph transport and the CLI has an
uploader, but the deployed validator and runner do not yet have a verified
graph-aware download/materialization path. Consequently, creating a staging
Post would present an unaccepted or non-restorable Capsule and is prohibited.
Source, Replay, and OCI execution were not substituted for VM acceptance.

## Writes

- staging rows inserted / updated / deleted: 0 / 0 / 0
- staging objects uploaded: 0
- production rows inserted / updated / deleted: 0 / 0 / 0
- production objects uploaded: 0
- production approval requested: false

SM64 was not cloned, downloaded, restored, copied, or uploaded. No production
deployment or mutation was performed.
