# Combined Native / ModelSet / OCI component regression — 2026-10-24

Source: clean root integration `876cb714085e17dc6156c5762ea1752f9aa8cef1`
(`integration/formation-native-models-v1`), exported with `git archive` into this
independent scratch directory. Includes Native #1536, single OCI #1535 and
component inputs #1537. The group PR worktree remains clean/frozen at
`9f2a526fc3d4ce5aabc7bd95bf2f3a589fdb3b42`.

All 568 Rust/Cargo/config inputs were content-hashed and refreshed before the
serialized shared-target build. Manifest SHA-256:
`579f66f55987481fc75e564ca1ecf8e4bcf9a8a637fbda84b99b3eaaf900f40d`.
Linux mirror: `/home/ekohsuke/ato-run/.tmp/model-combined-876cb714/source`.

Same-source Worker binary SHA-256:
`1786c55b2e67cdc49c79808a9468c95f475b0d388919e768ac69f56ec7806bf5`.
It is a test shim, not an installed or published Runner artifact.

Default serial libraries passed 548 tests, 24 opt-in cases ignored:

| Crate | Passed | Ignored | Seconds |
| --- | ---: | ---: | ---: |
| Portable application | 117 | 0 | 93.89 |
| Connected Worker | 169 | 20 | 22.63 |
| IPC | 81 | 0 | 0.05 |
| Runtime attempt | 181 | 4 | 18.70 |

Four-crate all-target Clippy with `-D warnings` passed (19.66 s). Workspace fmt
passed from the independently exported source. Before/after all 568 content
hashes matched. The Worker shim was compiled from this same source before tests.
Runs used fd8192, short fresh workspace `.tmp/gmi.XXXXXX` roots, env-i, fresh
HOME/ATO_HOME/exploration roots and empty Docker config.

The unchanged two-OCI matrix was not repeated; its #1537 real-container evidence
is retained. This run targets combined Worker/context/projection regressions.
No code, PR branch, API, staging deployment, flag, installed/shared Runner,
credential, Provider request or paid resource was changed.

## Existing fresh-root Native GUI + Files exact regression

`native_dispatch_tests::real_host_boundary_pixel_lease_preserves_original_k_and_instance_files`
passed from a fresh root process using the same-source Worker library test
executable. Preparation 6,997 ms, lease execution 4,198 ms, total 11.88 s.
The original Contract remained fully satisfied, selected D remained
`sha256:41773d7bacaa723304b98e1d8fd0d964d9f30ecf68c8872cf92bf9344c50bed4`,
and `result.json` was saved. Existing assertions verified actual GUI attach,
scoped authority, RO input/write rejection, independent application/helper
mounts and trusted UID65534, fake credential canary exclusion, original K and
confirmed stop/container/network/listener/journal cleanup.

This exercises the changed DeliveredInput logical-ID/context wiring with
Native + Files. It uses real Docker/TigerVNC/gateway/frame/K/stop/save clients
and local canary API credentials/Coordinator; it is not a real Hosted/staging
lease or production key acceptance.

Read-only existing fixture use:

- Static application SHA-256
  `31803c4aab6b549a7174ad727f6adb0b898079a92b7015949d06adfd3814bc6d`
  matched before and after execution.
- Helper `ato.local/native-helper@sha256:7076a8aadff1e92a09ba843928380aecfb00395cde9bce697e926174af04a9bd`.
- Application `ubuntu:24.04@sha256:52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf`.

Only uniquely marked `/opt/ato/.tmp/nmg.RsXb42` was created with mode0711; its
private Docker config was empty. Existing `/opt/ato` and `/opt/ato/.tmp` owners
and modes matched before/after execution and cleanup. The exact marked own
fixture was removed after success; no shared fixture or parent was changed.
All 568 input hashes matched again after Native execution.

Worker library test executable SHA-256:
`335422030827f0cdcf289ce1ce13473dae1517f3affd33f675d12353ebf83771`.
Full logs, before/after hashes and cleanup evidence are retained in `logs/` and
in the independent Linux mirror parent. No Cargo/test process remains active;
shared target ownership is released after this report.
