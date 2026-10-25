# Runner rollout and rollback (connected realization worker)

Status: operating procedure  
Date: 2026-09-19

A Runner binary change is never "swap the binary and restart". A restart
must not leave a workload running that a new Run could meet as a second
writer, and a rollback must land on a binary that can still contain
workloads. Rollout and rollback are the same procedure with a different
target binary.

## Preconditions

- Know the target binary's commit and confirm it contains the bubblewrap
  containment probe fix `42ddbe23` (`git merge-base --is-ancestor 42ddbe23
  <target>`). A binary with `bf16c46b` but without `42ddbe23` stops
  advertising `runtime_launch` on merged-`/usr` hosts (every Ubuntu host we
  run). A binary older than `bf16c46b` uses the previous `PATH` probe and is
  also acceptable.
- Know whether the target understands `ato.runtime-launch-spec.v2`. If it does
  not, it must not advertise `runtime_feature=oci_service_group_v1`, and the
  control plane will stop scheduling OCI service groups to it — confirm that
  nothing depends on groups running there.
- Know whether the target contains Runner run recovery (`run.ato.dev/managed`
  labels, the run journal). An older target does not recover leftovers; step 2
  below must then leave nothing behind before switching.

- Know whether the target understands `ato.runtime-launch-spec.v3`
  (Runner-local persistent volumes). A target without it, or without
  `ATO_RUNNER_STATE_VOLUME_ROOT`, does not advertise
  `runtime_feature=runner_persistent_volume_v1`. Every Instance whose state
  already lives in a volume on this Runner then fails to launch with
  `state_volume_runner_unavailable` until a volume-capable binary is back.
  That is intended: its data stays in the volume and is never rebuilt from an
  older revision or moved to another Runner. Never delete or move
  `<ATO_RUNNER_STATE_VOLUME_ROOT>/<runner_id>/volumes/` as part of a rollback.

## 1. Stop new assignment

Drain every Runner device served by the host:

```text
POST /v1/admin/runners/<runner_id>/drain
```

A drained Runner is excluded from selection and does not claim new leases.

## 2. Stop and confirm the running Runs

- Ask each active Run to stop through the normal stop path and wait for its
  lease to become `stopped`.
- On the host, confirm nothing of the slot is left:

  ```sh
  docker ps --all --filter label=run.ato.dev/managed=true \
    --filter label=run.ato.dev/runner-id=<runner_id> \
    --filter label=run.ato.dev/slot-id=<slot_id>
  ls <work_root>/runtime-launch/journal/
  ```

  Both must be empty. Containers without `run.ato.dev/managed=true` (other
  projects on the host) are out of scope and are never touched.
- A Run whose stop cannot be confirmed stays quarantined on the control plane.
  Do not switch binaries past it: resolve it first (Runner recovery, or an
  audited operator release once the host is verified).

## 3. Switch the binary

Install the target as `/usr/local/bin/ato-connected-realization-worker-<sha>`
and point the service at it. On ubuntu-sugamo each service's effective
`ExecStart` comes from the last drop-in; the OCI service group rollout uses
`zzz-oci-service-group.conf`. Editing or removing that drop-in is how the
binary is switched — it is one step of this procedure, not the procedure.

Volume-capable targets also need the host-wide volume root, shared by every
slot worker of the Runner, owned by the service user and outside any work
root:

```sh
Environment=ATO_RUNNER_STATE_VOLUME_ROOT=/var/lib/ato-runner-volumes
```

```sh
sudo systemctl daemon-reload
sudo systemctl restart <service>
systemctl show <service> -p ExecStart --value
```

## 4. Confirm recovery and advertisement

- The worker logs `[runtime-launch-recovery]` lines for anything it found; a
  `blocked` line means the slot will not take runtime work until resolved.
- The Runner device's heartbeat must show the expected lease kinds and
  capabilities (`runtime_launch`, `execution_abi=oci`, and
  `runtime_feature=oci_service_group_v1` only for a v2-capable target).

## 5. Undrain and smoke

```text
POST /v1/admin/runners/<runner_id>/undrain
```

Run a single-OCI smoke (a Discover OCI app: start, open the Surface, stop) and,
for a v2-capable target, the OCI service group proof. Record the binary commit,
the per-service stop outcomes from the worker log, and that no labelled
container remains after the stop.
