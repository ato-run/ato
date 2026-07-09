# Staging snapshot builder — ubuntu-sugamo runbook

**Status: authoritative as of 2026-07-10 (Hardware Binding Layer flag-day).**

## Why the staging builder lives on ubuntu-sugamo

Ready-State snapshots are **host-bound**: every sealed artifact records the
`runner_class_id` of the host that built it, and a restore on any other
runner class is refused by the runner
(`runner class mismatch: snapshot built for blake3:… cannot restore on blake3:…`).
There is no cross-class portability until named hardware contracts ship
(`hardware_contract_id` is NULL / `portability_tier=host_bound_snapshot` today).

The staging preview runner is `ubuntu-sugamo-staging`. Therefore **staging
snapshots must be built on ubuntu-sugamo** — a snapshot built anywhere else
(e.g. the Hetzner box) is dispatchable by the control plane under
`legacy_passthrough` but will always fail at restore time on this host.

**Do NOT re-enable the Hetzner `ato-snapshot-builder` unit for staging
rebuilds.** The Hetzner box (`hetzner-ato-runner`, 65.109.37.38) is the
production runner; its builder unit is intentionally `inactive` and its
`/etc/ato/runner-builder-override.env` still points at the staging API — if it
is started it will claim staging jobs and produce Hetzner-class artifacts that
Sugamo cannot restore. (Verified live on 2026-07-09: this exact failure mode
broke every staging preview.)

## Unit layout on ubuntu-sugamo

- Unit: `/etc/systemd/system/ato-snapshot-builder.service`
  (canonical `ato runner setup` template + a second `EnvironmentFile` and an
  `ExecStopPost` scrub; runs as root — loop-device mounts and /dev/kvm need it).
- Binary: `/usr/local/bin/ato-snapshot-builder` (build from `main`:
  `cargo build --release -p snapshot-builder`).
- Guest agent: `/usr/local/lib/ato/guest-agent-musl` — **must be the musl
  static build** (`cargo build --release --target x86_64-unknown-linux-musl
  -p guest-agent`). A glibc build cannot exec inside Alpine-based
  dockerfile-import rootfs: the guest dies with
  `/sbin/init: /usr/local/bin/ato-guest-agent: not found` and the job fails as
  `guest never became healthy within timeout`.
- Env files (unit reads both, in order):
  - `/etc/ato/runner.env` — shared runner host env (written by `ato runner setup`).
  - `/etc/ato/runner-builder-override.env` — root-owned `0600`; holds
    `SNAPSHOT_BUILDER_AGENT_TOKEN`, `ATO_API_URL=https://staging.api.ato.run`,
    `ATO_BUILDER_SUPERVISOR=1`, `ATO_FC_VSOCK=1`,
    `ATO_GUEST_AGENT_BIN=/usr/local/lib/ato/guest-agent-musl`, and
    `ATO_FC_WORK=/var/lib/ato/fc-work`.

## /tmp capacity: why ATO_FC_WORK is set

Sugamo's `/tmp` is a **16 GiB tmpfs**. The Firecracker backend's default work
root is `/tmp/ato-fc`, whose `rootfs/`, `mem/`, and `vmstate/` caches grow by
roughly 1–2 GiB per build and are not pruned between jobs — a ~10-job batch
fills the tmpfs and every subsequent build fails with ENOSPC
(`failed to setup loop device` / `tar: Cannot write: No space left on device`).

Mitigations in place:

1. `ATO_FC_WORK=/var/lib/ato/fc-work` moves the scratch space onto the real
   disk (the env var is honoured by `crates/snapshot/src/firecracker.rs`).
2. `ExecStopPost=/bin/rm -rf /var/lib/ato/fc-work/{rootfs,mem,vmstate}` scrubs
   the caches on every unit stop/restart, so a crashed batch cannot strand
   partial artifacts.

For very large batches, restart the unit between batches
(`systemctl restart ato-snapshot-builder`) to trigger the scrub.

## Operational notes

- A builder restart mid-build orphans the claimed job in status `building`
  until its `claim_expires_at` (15 min); the job is then reclaimed
  automatically (`attempt_count` increments). Do not hand-edit the job row.
- The sealed ack **requires** `snapshot_format_id` / `snapshot_codec_id`
  (flag-day, ato-api #217): a pre-#1011 builder binary's acks are rejected by
  staging/production. Always build the daemon from `main`.
- `capsule_snapshot_metadata` is *not* written by the ack route. If snapshots
  are rebuilt outside `scripts/store-snapshot-batch-smoke.mjs` (ato-api repo),
  the preview offer fails closed (`preparing`) because the metadata row's
  `snapshot_artifact_manifest_hash` no longer matches the latest artifact —
  use ato-api's `scripts/store-snapshot-metadata-resume.mjs` to reconcile from
  the real job receipts.
