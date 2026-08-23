# Staging Firecracker integration receipt — 2026-08-23

Status: **partial / blocked before staging publication**.

## Immutable starting state

The eight requested Draft PR heads were fetched and matched exactly:

- ato #1292 `5d39069e7c8dfeb34c2aa25a1c8ac8a6f2274ba4`
- ato #1293 `49fdc82a09a857e58324ea921942e43da4398745`
- ato #1294 `9f7a9dffeacb4517632af6d907c7b0bb54e08615`
- ato #1295 `a16efc06468c0132a3e374593ee111ce6510aba3`
- ato #1296 `29128c0b46181fb844e0c52713ba63ed34335926`
- ato #1297 `2b0e6618bee6f71308a2c257af3dedf084a07472`
- ato-api #519 `a4df85ef382ad985f7549b54d2d0536a62d8f717`
- ato-api #520 `872ea0b41ce3953cafa7cd6054172b68e621cf58`

All remained Draft. No merge, Ready transition, or auto-merge was performed.

## Target identity gate

The existing 2048 repository resolved one current target:

`blake3:b18ad849d301ad6b009e4e6c8ab413667050c87b3514d08ccfb9d9bca8baf291`

Its replay anchor is the distinct existing computation
`blake3:e6d62e8c2ac0acbec0e12a28db4a77c1d610a042e9ccc4446d958ef55f16a5e8`.
The target is present in both local and origin session refs and is the target of
the existing replay descriptor
`blake3:b1b24a20bc6d26ec61d7d192042c6e5e1a5b75d5ba67a30b14680ca90b49f4ca`.
It is not the legacy VM
snapshot id (`blake3:18ae...`) and was not derived from VM bytes, Record order,
or a RecordFrontier.

Current code re-encoded the existing replay closure without resealing identity
and resolved the same target. The resulting local compatibility bundle was
825,740 bytes with SHA-256
`bac85e2de72bfef6f74f047565b20ce23da1982af5f55bc265d23568082f57b5`.

## Implementation

Draft PR: https://github.com/ato-run/ato/pull/1298

- branch: `feat/staging-firecracker-realization-v1`
- base: `feat/legacy-vm-library-migration-v1` (#1297)
- recorded head before this receipt: `e172eb8384ccf9448aa62967ee5a5c27ca21e9e7`
- Draft/Open: true

Implemented:

- canonical backend-owned relative `FirecrackerRestoreLayout`
- per-session memory/vmstate/rootfs/API/vsock layout and Firecracker `current_dir`
- per-slot network namespace containing the snapshot's logical TAP name
- Firecracker 1.16+ `vsock_override`, with isolated relative-path handling for
  older versions
- fail-closed `/dev/kvm`, netns, version, and capability probe
- active VM capture ordering: freeze, quiesce, pause, Record Barrier, snapshot,
  integrity sync, resume, unfreeze
- rollback of pause, ingress freeze, Record lease, and capture temp files
- CAS-content RecordFrontier verification including segment digest/length,
  writer order, payload closure, causal cut, and watermarks
- decoded semantic graph rederivation before object upload
- additive accounting names: `declared_object_count`, `client_put_count`,
  `objects_stored_new`, `objects_reused`, and `unique_stored_bytes`

## Local verification

- `cargo test -p ato-objects -p ato-record-writer -p ato-materializer-vm-snapshot -p ato-cli --locked`: PASS (57 unit tests, 13 computation architecture tests)
- focused VM tests: PASS (16/16)
- focused Record Writer tests: PASS (9/9)
- `cargo clippy -p ato-objects -p ato-record-writer -p ato-materializer-vm-snapshot -p ato-cli --all-targets --all-features --locked -- -D warnings`: PASS
- `cargo check -p ato-materializer-vm-snapshot --all-targets --target x86_64-unknown-linux-gnu --locked`: PASS

GitHub checks were still running when this receipt was written; code-green is
not inferred from an in-progress result.

## Staging blocker

The current Draft stack has API validation-job claim/download/ack routes but no
checked-in or deployed graph-aware validator agent. The current ato branch also
does not contain the historical Connected Runner polling/lease process that can
own an active Firecracker Realization and invoke the new capture source. Those
two runtime consumers are required before a safe staging deploy.

Consequently no Firecracker binary/version/capability claim is recorded, no
RecordFrontier or VM descriptor was minted, no CAS graph was uploaded, no 2048
Post was created, and Feed → Detail → Continue / 3 fresh restores were not run.
Source, Replay, and OCI results were not substituted for VM acceptance.

## Production operation ledger

- rows inserted: 0
- rows updated: 0
- rows deleted: 0
- objects uploaded: 0
- production deploy: false
- production approval requested: false
