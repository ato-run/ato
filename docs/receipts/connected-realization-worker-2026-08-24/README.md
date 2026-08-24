# Connected Realization Worker receipt — 2026-08-24

Status: **partial; local implementation complete, staging VM Acceptance not
claimed**.

## Starting heads

- ato #1298: `0d72098b2cee50998d55e2fcadfbfd2cc28ecb06`
- ato-api #521: `98a385da96b3d0f7b499af5de9c3164d46907b9a`

Both matched the requested heads after `git fetch` and remained Draft/Open.
PRs #1292–#1299 and #519–#521 were not merged, marked Ready, or configured for
auto-merge.

## Target identity

- target ComputationRef:
  `blake3:b18ad849d301ad6b009e4e6c8ab413667050c87b3514d08ccfb9d9bca8baf291`
- replay anchor:
  `blake3:e6d62e8c2ac0acbec0e12a28db4a77c1d610a042e9ccc4446d958ef55f16a5e8`
- existing replay descriptor:
  `blake3:b1b24a20bc6d26ec61d7d192042c6e5e1a5b75d5ba67a30b14680ca90b49f4ca`

The current compatibility Capsule at
`.tmp/identity-gate-2048/current.capsule` in the preserved #1298 worktree
contains and resolves the target as an existing computation object. No new
ComputationRef was derived from a Record, RecordFrontier, replay descriptor, or
VM bytes.

## Implemented locally

- independent Validator Agent process using the existing claim/index/object/ack
  routes and only `CAPSULE_VALIDATOR_AGENT_TOKEN`;
- shared runtime graph download, digest/size verification, isolated ObjectStore,
  decoded semantic closure derivation, VM target verification, and full
  RecordFrontier verification;
- current Runner heartbeat, lease claim, status, ready, control, and stopped
  client;
- lease-scoped #521 graph access with Bundle/root/graph header binding;
- local graph revalidation before Planner;
- VM-only Hosted/TenantIsolated Planner assembly;
- exactly-one-barrier VM capture and captured-frontier descriptor binding;
- generic fresh Firecracker boot owner for a caller-supplied current bootable
  rootfs;
- per-netns logical TAP, optional namespaced Surface relay, unique per-lease UDS,
  hidden Contract relay, post-Contract publication relay, and cleanup;
- explicit capture-capable dependency injection using the active VM plus its
  Record Writer Capture Barrier.

## Local tests

- `cargo test -p ato-runtime-object-graph --locked`: PASS (9)
- `cargo test -p ato-connected-realization-worker --locked`: PASS (5)
- `cargo test -p ato-materializer-vm-snapshot --locked`: PASS (16)
- `cargo test -p ato-record-writer --locked`: PASS (9)
- focused `cargo clippy ... -D warnings`: PASS

Broader workspace gates are recorded after the final local stack run.

## Staging gate

No staging mutation has been performed from this branch yet. In particular:

- validator deployed: false
- malicious graph rejection receipt: absent
- Connected Worker deployed: false
- new 2048 Firecracker artifact: absent
- RecordFrontier: absent
- VM descriptor: absent
- CAS objects uploaded: 0
- staging Post inserted: 0
- Feed / Detail / Continue: not run
- fresh Firecracker restores: 0/3

The remaining physical blocker is a verified staging path that constructs a
bootable current Firecracker rootfs from the existing 2048 Source/Replay
candidate. The generic kernel/rootfs boot owner is now present, but no
application-specific or legacy-identity shortcut was added. Publication stays
closed until that rootfs, real KVM capture, secret/private scan, independent
validation, and 3/3 restore cleanup all pass.

## Production ledger

- rows inserted: 0
- rows updated: 0
- rows deleted: 0
- objects uploaded: 0
- production deploy: false
- production approval requested: false

