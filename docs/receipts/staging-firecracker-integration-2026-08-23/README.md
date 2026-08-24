# Staging Firecracker integration receipt — 2026-08-24

Status: **complete**.

2048 was captured as a new `ato.materialize.vm.snapshot@1`
Materialization of its existing ComputationRef, uploaded as a content-addressed
object graph, independently validated, published as a staging Post, and
restored from the staging Feed / Detail / Continue flow three times. Source,
Replay, and OCI execution were not accepted as substitutes for VM restore.

## Draft stack

All PRs remained Draft and open. No merge, Ready transition, or auto-merge was
performed.

The table records the implementation heads immediately before this receipt
commit; the final #1300 PR head is reported by GitHub after the receipt push.

| Repository | PR | Branch | Implementation head | Base |
| --- | --- | --- | --- | --- |
| ato | #1298 | `feat/staging-firecracker-realization-v1` | `0d72098b2cee50998d55e2fcadfbfd2cc28ecb06` | `feat/legacy-vm-library-migration-v1` |
| ato | #1299 | `feat/runtime-object-graph-validator-v1` | `158cc2814d461d15bfa205654e42f9e0bf39a046` | `feat/staging-firecracker-realization-v1` |
| ato | #1300 | `feat/connected-realization-worker-v1` | `8324be012bb9eb1fc5db618f1d9d135493106960` | `feat/runtime-object-graph-validator-v1` |
| ato-api | #521 | `feat/capsule-graph-runtime-access-v1` | `98a385da96b3d0f7b499af5de9c3164d46907b9a` | `feat/legacy-vm-library-migration-v1` |

The final #1300 integration fixes advertise portable dispatch capabilities,
surface bounded Firecracker startup diagnostics, keep Firecracker Unix sockets
within `SUN_LEN`, and refresh the generated Windows installer manifest.

## Staging deployment

- API health: `200`, `{"status":"ok","service":"ato-store","version":"0.1.0"}`
- API deployment/version: `a6a3f53b-9e98-41bc-95a7-59dcdd9b17c3`
- deployment created: `2026-08-24T01:53:11.284Z`
- Worker: `ato-store-staging`
- PWA: existing `https://stg-app.ato.run` build; not redeployed
- production deployment: false

Cloudflare authentication was provided through Wrangler OAuth. No Cloudflare
credential was copied into a command, receipt, artifact, Validator process, or
Runner process.

## Validator

- service: `ato-capsule-validator.service`, active
- binary SHA-256:
  `98c0c90873ddfea2ec65750e4e9543ca51d4880eca625f5918166222d73b0113`
- authority: dedicated staging Validator agent token only
- Firecracker/KVM/Runner/user/Cloudflare authority: absent
- malicious private graph: `cog_01M0RXCCVC0NYEJ2HH6WEVJTP4`
- independent rejection: `unreachable_objects`
- malicious graph cleanup: 1 object removed
- prior unfinished test graph cleanup: 105 objects removed
- accepted graph `cog_01M0RXDV9S714CG84ECJ5FMHX7`: independently
  decoded and verified before Bundle readiness

The agent downloaded every declared object into an isolated store, rechecked
size and digest, rederived semantic references from decoded content, resolved
the root ComputationRef, and validated the VM target and sealed RecordFrontier.
Client-provided `references` were not the semantic traversal authority.

## Connected Runtime Worker

- service: `ato-runner-agent.service`, explicitly switched to the connected
  worker; no concurrent legacy poller
- runner id: `01KX0SWDPP2GA41NEXQXNDCC0D`
- binary SHA-256:
  `364c5e05b5b1cd50752826805276652d1ea21189aed5b915c2166b4b313e0cbc`
- Firecracker: `1.16.0`, architecture `x86_64`
- `/dev/kvm`: available
- actual netns, TAP, vsock override, Surface relay, and slot lifecycle: PASS
- advertised execution ABI: `process`
- advertised isolation: `untrusted-v1`
- advertised materializer: `ato.materialize.vm.snapshot@1`
- supported lease kind: `portable_capsule_v2`
- public slot 0: `https://s0-rstg002.ato.run` -> `127.0.0.1:8420`
- hidden Contract Surface: `127.0.0.1:18420`

The Worker independently downloaded and validated the authorized ready graph
for each lease. The Planner selected only the VM candidate. The connected
worker contains no VM-to-Source/Replay/OCI fallback for this flow.

## 2048 target and single-cut capture

- license: MIT
- pinned source revision: `68ae33b75d799c211d5a48ee2ef2e3c3e30a5766`
- target ComputationRef:
  `blake3:b18ad849d301ad6b009e4e6c8ab413667050c87b3514d08ccfb9d9bca8baf291`
- sealed RecordFrontier:
  `blake3:9e7afe0897ebb624b976e9871a8de1e0dec37a7ae646cb462d7808388966adbf`
- VM descriptor:
  `blake3:eb33112d27a2c193e22421362544cce2cb9864e7151bf5a3f80ed5a7e9cf20af`
- capture Firecracker PID: `1729598`
- capture netns: `ato-capture-2048-1`
- rootfs backing path: `vm/rootfs.ext4`
- Capture Barrier calls: **exactly 1**
- hidden candidate ready before capture: true
- external Surface before publication: unreachable
- capture rollback/cleanup: PASS

The VM descriptor target is the pre-existing target above. Neither the VM
bytes nor the RecordFrontier created or changed ComputationRef identity.

## Object graph

- graph: `cog_01M0RXDV9S714CG84ECJ5FMHX7`
- Bundle: `bnd_01M0RXKR9HGXMA8CZ09QQEGB19`
- bundle/index digest:
  `blake3:618723986d2294eec0585d2ce357751bc36a6553971ebd5085953f80336fc27e`
- declared object count: 105
- client PUT count: 105
- objects stored new: 105
- objects reused: 0
- logical bytes: 256,533,404
- unique stored bytes: 256,533,404
- Validator result: PASS

`objects_uploaded` in the legacy migration accounting means objects newly
stored in tenant CAS (`objects_stored_new`), not client PUT attempts or reused
objects.

## Staging Post and Chrome acceptance

- Post: `cpost_01M0RY2XRDRK6SKQQF6Z0RET63`
- public share: `cps_56f07ef0662c336741f07e095b951d05f23e5dc4caefedfb41d16a7dcc40762a`
- Feed: PASS; public 2048 card rendered
- Detail: PASS; title, thumbnail, original link, Continue, and root identity
  rendered without an error state
- lease source: `portable_capsule_v2` for all three runs
- Planner selection: `ato.materialize.vm.snapshot@1` for all three runs
- Firecracker `snapshot/load`: PASS in a fresh process for all three runs
- hidden Contract: PASS before each Surface publication
- public Surface before publication: unreachable
- public Surface after publication: HTTP 200 and real 2048 UI rendered
- Chrome refresh during run 1: PASS

Actual Chrome key input, rather than an HTTP-only or Playwright substitute,
changed both DOM tile state and the visible board:

1. `ArrowLeft`, `ArrowUp`, `ArrowRight`; score advanced to 4 and tile
   positions/merge state changed.
2. `ArrowLeft`, `ArrowDown`, `ArrowRight`; DOM tile positions and visible board
   changed.
3. `ArrowUp`, `ArrowLeft`, `ArrowDown`, `ArrowRight`; DOM and visible board
   changed, including an `8` tile.

Screenshots of Detail, Continue/ready, initial board, and post-input board are
presentation/acceptance evidence only. They are not Records and are not stored
in the Record Store.

## Fresh physical restores and cleanup

| Attempt | Lease | Run | Execution | Firecracker PID | Result |
| --- | --- | --- | --- | --- | --- |
| 1/3 | `01M0RZEMJT9XVXJ1EPCEHZD0BZ` | `01M0RZEMJTXQ3V7VEV4TJQA124` | `vm:01M0RZEMJTXQ3V7VEV4TJQA124:01M0RZEMJT9XVXJ1EPCEHZD0BZ` | `1737275` | PASS |
| 2/3 | `01M0RZP06QDPG0B9F21Q2PPFF1` | `01M0RZP06QZJ46VFK8YZF94QN1` | `vm:01M0RZP06QZJ46VFK8YZF94QN1:01M0RZP06QDPG0B9F21Q2PPFF1` | `1740410` | PASS |
| 3/3 | `01M0RZTNAV75S29683FPQKY4JJ` | `01M0RZTNAVE64NR6F833B14RVW` | `vm:01M0RZTNAVE64NR6F833B14RVW:01M0RZTNAV75S29683FPQKY4JJ` | `1742422` | PASS |

Each attempt had a new lease, execution id, Firecracker PID, VM session,
network namespace, TAP, vsock UDS, and runner slot. After each Stop:

- Firecracker processes: 0
- run netns: 0
- TAP devices: 0
- vsock UDS: 0
- Surface relay UDS: 0
- temporary restore/session directories: 0
- runner slot locks: 0
- public Surface: unreachable (`502`)
- browser profile created on Runner: 0

## Security

The new memory, rootfs, vmstate, and restore metadata passed a bounded scan for
private keys, bearer/Authorization material, cookies, Cloudflare and runner
credentials, API-key patterns, user databases/notes, and known secret markers.
No secret value was recorded. SM64 and its ROM were not fetched, copied,
restored, or uploaded.

## Verification

- `cargo test -p ato-connected-realization-worker --locked`: PASS (8)
- `cargo test -p ato-materializer-vm-snapshot --locked`: PASS (17)
- focused connected-worker/materializer Clippy with `-D warnings`: PASS
- `cargo fmt --all -- --check`: PASS
- `cargo check --workspace --all-targets --locked`: PASS
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: PASS
- `cargo run -p arch-check --locked`: PASS (28 packages)
- `cargo test -p ato-cli --test computation_architecture --locked`: PASS (13)
- GitHub architecture and CLI checks on Linux/macOS/Windows: PASS
- GitHub dist plan initially failed on a stale generated WiX manifest; the
  manifest was regenerated in `8324be01` and a replacement CI run was queued

## Production operation ledger

- rows inserted: 0
- rows updated: 0
- rows deleted: 0
- objects uploaded: 0
- production deploy: false
- production approval requested: false

No production command, connection, migration, upload, Post creation, flag
change, deploy, or approval request was made.
