# Connected Realization Worker receipt — 2026-08-24

Status: **partial; local implementation complete, staging VM Acceptance not
claimed**.

## Starting heads

- ato #1298: `0d72098b2cee50998d55e2fcadfbfd2cc28ecb06`
- ato-api #521: `98a385da96b3d0f7b499af5de9c3164d46907b9a`

Both matched the requested heads after `git fetch` and remained Draft/Open.
PRs #1292–#1299 and #519–#521 were not merged, marked Ready, or configured for
auto-merge.

## Integration PRs

- ato #1299 (Draft), `feat/runtime-object-graph-validator-v1`, base
  `feat/staging-firecracker-realization-v1`, implementation commit
  `60c659abbfefb7f4a47d23b34dfca65798beb136`, dist exclusion
  `158cc281`
- ato #1300 (Draft), `feat/connected-realization-worker-v1`, base
  `feat/runtime-object-graph-validator-v1`, implementation commits
  `182c63e5`, `bc5f053c`, `062b8d5c`, `badd855e`, and `632953ae`

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
  client, including active-slot heartbeats for long-lived leases;
- lease-scoped #521 graph access with Bundle/root/graph header binding;
- local graph revalidation before Planner;
- VM-only Hosted/TenantIsolated Planner assembly;
- exactly-one-barrier VM capture and captured-frontier descriptor binding;
- generic fresh Firecracker boot owner for a caller-supplied current bootable
  rootfs;
- per-netns logical TAP, optional namespaced Surface relay, unique per-lease UDS,
  bounded guest-ready connection retry, hidden Contract relay, post-Contract
  publication relay, and cleanup;
- explicit capture-capable dependency injection using the active VM plus its
  Record Writer Capture Barrier.

## Local tests

- `cargo test -p ato-runtime-object-graph --locked`: PASS (9)
- `cargo test -p ato-connected-realization-worker --locked`: PASS (5)
- `cargo test -p ato-materializer-vm-snapshot --locked`: PASS (16)
- `cargo test -p ato-record-writer --locked`: PASS (9)
- focused `cargo clippy ... -D warnings`: PASS
- `cargo fmt --all -- --check`: PASS
- `cargo check --workspace --all-targets --locked`: PASS
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`:
  PASS
- `cargo run -p arch-check --locked`: PASS (27 packages)
- `cargo test -p ato-cli --test computation_architecture --locked`: PASS (13)
- `cargo test --workspace --no-fail-fast --locked`: all targets PASS except two
  existing macOS netd tests at the long worktree path (`SUN_LEN`)
- the same two netd tests from the short worktree
  `/Users/egamikohsuke/Ekoh/projects/ato-run/.tmp/af2`: PASS (2/2); the temporary
  worktree was removed afterwards
- ato-api `pnpm typecheck`: PASS
- ato-api focused object transport, runner lease/runner routes, importer, and
  Capsule Network suite: PASS (121/121)
- local `dist plan --output-format=json`: PASS after both staging-only agents
  were explicitly excluded from release packaging

GitHub computation CLI and architecture checks passed on the pre-fix heads. The
Release workflow `plan` job initially failed because the two staging-only agent
binaries were treated as distributable Windows installers and had no WiX GUIDs.
That repository configuration defect was corrected with
`package.metadata.dist = false`; replacement GitHub checks were still settling
when recorded, so the overall CI state is not reported as green.

## Staging gate

No staging mutation has been performed from this branch. The prerequisite
read-only command
`pnpm exec wrangler deployments list --env staging` was tried twice at the
exact #521 head. Both attempts failed before returning deployment state: the
first could not refresh authentication after a network timeout and the second
returned `fetch failed`. `CLOUDFLARE_API_TOKEN` and `CF_API_TOKEN` were unset.
The requested deployment sequence therefore stopped before #521 deploy. In
particular:

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
- staging API deploy attempted: false
- production deploy attempted: false

After staging access is restored, the remaining physical blocker is a verified
staging path that constructs a
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
