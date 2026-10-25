# Legacy VM Library migration restack receipt

Status: complete for the 2048 staging VM acceptance; production migration remains intentionally out of scope

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

- candidate: 2048, MIT, pinned source revision
  `68ae33b75d799c211d5a48ee2ef2e3c3e30a5766`
- target ComputationRef:
  `blake3:b18ad849d301ad6b009e4e6c8ab413667050c87b3514d08ccfb9d9bca8baf291`
- RecordFrontier:
  `blake3:9e7afe0897ebb624b976e9871a8de1e0dec37a7ae646cb462d7808388966adbf`
- VM materialization descriptor:
  `blake3:eb33112d27a2c193e22421362544cce2cb9864e7151bf5a3f80ed5a7e9cf20af`
- Firecracker version: `1.16.0` on the staging Linux Runner
- object count / logical bytes / unique uploaded bytes:
  `105 / 256,533,404 / 256,533,404`
- fresh VM restores: **3/3 PASS**
- Feed -> Detail -> Continue -> `portable_capsule_v2`: PASS
- Planner selection: `ato.materialize.vm.snapshot@1`
- real Chrome keyboard and DOM/visual state change: PASS 3/3
- hidden Contract before Surface publication: PASS 3/3
- cleanup after real Firecracker restore: PASS 3/3

The target remained the existing ComputationRef. It was not populated or
derived from VM bytes, old Snapshot IDs, Record ordering, or RecordFrontier
identity.

## Acceptance closure

The graph-aware Validator Agent and Connected Runtime Worker were deployed to
staging. A new current-source VM was captured with exactly one Capture Barrier,
security-scanned, uploaded, independently validated, and published only after
validation. The staging Post is
`cpost_01M0RY2XRDRK6SKQQF6Z0RET63`; its ready graph is
`cog_01M0RXDV9S714CG84ECJ5FMHX7`. Source, Replay, and OCI execution were not
substituted for VM acceptance. Full execution evidence is in
`../staging-firecracker-integration-2026-08-23/README.md`.

## Writes

- staging publication: one ready 2048 object graph, Bundle, and Post
- staging objects stored new: 105
- production rows inserted / updated / deleted: 0 / 0 / 0
- production objects uploaded: 0
- production approval requested: false

SM64 was not cloned, downloaded, restored, copied, or uploaded. No production
deployment or mutation was performed.
