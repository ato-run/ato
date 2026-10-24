# Formation worker update lifecycle

Status: operating procedure
Date: 2026-10-24
Scope: `ato_managed` Formation workers (`management_pool_id = formation`) running
`ato-formation-worker serve` under systemd.
Wire contract: the state report (`POST /v1/internal/formation/workers/self`), the
`409 runner_drained` / `runner_update_required` claim refusals, and the journal
schema below. The ato-api side implements the same three pieces.

Related: [formation-worker-build-identity.md](formation-worker-build-identity.md)
(what a binary claims and how to pair it with a deployment record),
[runner-rollout-and-rollback.md](runner-rollout-and-rollback.md) (the connected
Runner counterpart; its binary-swap mechanics also apply here).

## Why this exists

Before this lifecycle, restarting a Formation worker was "swap the binary and
`systemctl restart`". There was no host-side record of an update in progress,
`drain` did not stop Formation claims, an in-flight build died with the process
without a terminal report, and there was no way to tell a restart-after-swap
from a restart-of-the-old-binary. Rollout and rollback were a purely manual
runbook.

Now the control plane is the authority and the worker keeps local evidence:

- The worker reports `lifecycle_state` and its binary identity every ~15s
  (`POST /v1/internal/formation/workers/self`).
- The control plane refuses claims with `409 runner_drained` /
  `409 runner_update_required`; either is an authoritative stop signal.
- The worker records its phase in `$WORK_ROOT/worker-update.json`, durably, so
  a restart can decide whether it may claim, must verify a new binary, or must
  roll back.
- A restart with the wrong binary is a **fail-closed non-claiming exit**, not a
  serving worker of unknown provenance.

The worker never downloads, installs, or replaces its own binary. The operator
performs the swap; the worker verifies and reports what is running.

## State machine

```text
active ──▶ draining ──▶ quiescent ──▶ updating ──▶ verifying ──▶ active
                                         │            │
                                         └──▶ rolled_back ◀──┘
                              any phase ──▶ quarantined
```

| phase | meaning | may claim |
|---|---|---|
| `active` | normal serving phase | yes |
| `draining` | new claims stopped; an in-flight job may still run | no |
| `quiescent` | no job in flight; the process exits `75` | no |
| `updating` | operator recorded that the binary is being replaced | no |
| `verifying` | new binary is running and is reporting its identity | no |
| `rolled_back` | running binary does not match the recorded target | no |
| `quarantined` | the journal cannot be trusted, or the lifecycle cannot resume | no |

Transitions are idempotent (repeating a phase is a no-op) and everything not
listed is refused: backward moves, forward skips, and leaving a terminal phase.
`rolled_back` and `quarantined` are cleared by **removing the journal**, not by
a transition — an absent journal reads as `active`, which is what the contract
says a host that never started an update looks like.

### Who writes which phase

| transition | written by |
|---|---|
| `active → draining → quiescent`, then exit `75` | worker, when the heartbeat directive says `drain` or a claim returns `runner_drained` / `runner_update_required` |
| `quiescent → updating` | operator script, after the swap is staged and the target is known |
| `updating → verifying → active` | worker, on restart, when the running binary matches the target |
| `updating`/`verifying → rolled_back` | worker, on restart or `--expect-*` check, when the binary does not match |
| `any → quarantined` | worker, when the journal is unreadable/unresumable |

## Exit codes

`ato-formation-worker serve` has three non-claiming outcomes. They are also
documented at the top of `apps/formation-worker/src/bin/ato-formation-worker.rs`.

| code | meaning | what to do |
|---|---|---|
| `0` | normal (`--version`, one-shot success) | — |
| `74` | rollback required, or the journal is quarantined | do not restart in a loop; reconcile binary + journal (see Rollback) |
| `75` | drained / quiescent, or SIGTERM/SIGINT without a drain request | start again after the binary swap / maintenance window |
| `1` | unexpected error (configuration, I/O) | read the log; the journal is unchanged |

`75` is a success-like stop for the supervisor. Configure systemd so it does
**not** restart `75` in a loop (`Restart=on-failure` with
`RestartPreventExitStatus=75`, or `Restart=no` and an explicit operator start
after a swap). A restart loop on `75` cannot install anything and only buries
the operator signal.

## Journal

Path: `$WORK_ROOT/worker-update.json` (default work root is whatever the unit
passes to `--work-root`).

```json
{
  "schema": "ato.formation-worker-update/1",
  "phase": "active",
  "target_version": null,
  "target_digest": null,
  "current_version": "0.1.0",
  "current_sha256": "…",
  "previous_binary": null,
  "detail": null,
  "updated_at": "2026-10-24T07:30:00Z"
}
```

- Writes are atomic and durable: temp file in the same directory, `fsync`,
  rename, then parent-directory sync where the platform supports it. A crash
  leaves either the old record or the new one, never a half-written file.
- An unreadable or invalid journal is **not** treated as absent. The worker
  moves the bytes to `worker-update.json.corrupt`, writes a `quarantined`
  record, reports it, and exits `74`.
- Do not hand-edit the journal. Use the operator script; it validates the
  current phase before writing the one legal forward edge.

## Crash recovery (runs before the first claim)

| journal at startup | outcome |
|---|---|
| absent | `active`; serve |
| `active` | serve |
| `draining` / `quiescent` | report, exit `75`; no claiming |
| `updating` / `verifying` + binary matches target | report `verifying`, then `active`; serve |
| `updating` / `verifying` + binary mismatch | `rolled_back`, report, exit `74` |
| `rolled_back` / `quarantined` | report, exit `74`; operator must clear |
| corrupt | `quarantined` (original preserved), report, exit `74` |

A "match" means `target_version == current version` and, when the target digest
is set, `target_digest == sha256(current_exe)`.

`--expect-version` / `--expect-digest` are the operator's statement of what the
unit should have started. They are checked before recovery's serve path; a
mismatch is fail-closed (journal becomes `rolled_back` when an update was in
progress, `quarantined` otherwise, exit `74`). Add them to the systemd drop-in
when switching binaries so a wrong file at the path cannot serve.

## Automated vs operator action

| step | automated | operator action |
|---|---|---|
| build the candidate binary | — | build/CI; record commit + dirty flag |
| record `sha256sum` and `--version` | script verifies them and compares to the target | keep the pair with the deployment record |
| request the update | script calls `POST /v1/admin/runners/:id/update` with `--execute` | decide the target version/digest |
| stop new claims | worker (heartbeat directive / `409`) | — |
| wait for quiescent | script polls the journal | confirm the in-flight job finished (log) |
| record `updating` | script (atomic journal write) | — |
| place the binary | script `install` to the target path with `--execute` (keeps `.prev`) | choose the path |
| systemd drop-in / `daemon-reload` | — | edit `ExecStart` and `--expect-*`, then `systemctl daemon-reload` |
| restart | — | `systemctl restart` after the drop-in edit |
| verify version/digest/phase | worker reports; script compares | read `journalctl`, confirm heartbeat |
| undrain | — | `POST /v1/admin/runners/:id/undrain` after verification |
| smoke | — | submit a job and confirm it completes |
| rollback decision | script restores `.prev` and clears the journal with `--execute` | decide to roll back; verify binary first |
| binary distribution / signing / attestation | none (not implemented) | out of scope for this procedure |

The script (`scripts/ops/formation-worker-update.sh`) is **dry-run by default**:
it prints every command, journal change and systemd edit without touching
anything unless `--execute` is passed. It never downloads a binary, never
builds one, never edits a unit, never restarts the service and never undrains.
Those stay operator actions, printed as `OPERATOR ACTION` blocks.

```text
# read-only
scripts/ops/formation-worker-update.sh status --work-root <work_root>

# verify the candidate, request the update, wait for quiescence, record
# `updating`, install the binary (prints the remaining operator steps)
scripts/ops/formation-worker-update.sh update   --candidate /path/to/ato-formation-worker-0.1.1   --target-version 0.1.1 --target-digest <64 hex>   --api-base https://api.ato.run --runner-id <runner_id>   --work-root <work_root> [--execute]

# restore the previous binary and clear a terminal journal
scripts/ops/formation-worker-update.sh rollback --work-root <work_root> [--execute]

# after undrain, return a quiescent host to active
scripts/ops/formation-worker-update.sh clear-journal --work-root <work_root> [--execute]
```

## Procedure: update

Preconditions:

- The candidate was built from a committed tree: `--version` reports the
  expected commit and `clean` (a `dirty` or `unknown` binary is a release
  blocker).
- `sha256sum <candidate>` and `<candidate> --version` are recorded together.
- `target_version` is a stable `x.y.z` and differs from the running
  `agent_version`.

1. **Stage the candidate on the host** (scp/copy from the build record). Do not
   run it yet.

2. **Record its identity**:

   ```sh
   sha256sum /path/to/ato-formation-worker-0.1.1
   /path/to/ato-formation-worker-0.1.1 --version
   ```

3. **Request the update** through the admin API (the script does this with
   `--execute`):

   ```text
   POST /v1/admin/runners/<runner_id>/update
   { "target_version": "0.1.1", "target_digest": "<64 hex>" }
   ```

   This sets `drained_at`, so the worker's next heartbeat reports `drain` and
   it stops claiming.

4. **Wait for quiescent**: the worker finishes the in-flight job, reports the
   result, records `draining → quiescent`, reports `quiescent`, and exits `75`.
   The script polls `$WORK_ROOT/worker-update.json` until `phase == quiescent`
   (or the service is inactive). If the job does not finish before the
   timeout, stop and investigate; do not swap past a running build.

5. **Record `updating`** (script, atomic write): the journal moves
   `quiescent → updating` and gains `target_version` / `target_digest` /
   `previous_binary`. This is the record the next process uses to decide
   whether the swap landed.

6. **Place the binary** (script `--execute`): install to
   `/usr/local/bin/ato-formation-worker-<sha>` (or the path the unit uses),
   keeping the current file as `.prev` for rollback.

7. **Point the unit at it** (operator): update the drop-in `ExecStart` and add
   the expectation flags, then reload:

   ```ini
   # /etc/systemd/system/ato-formation-worker.service.d/zzz-update.conf
   [Service]
   ExecStart=
   ExecStart=/usr/local/bin/ato-formation-worker-0.1.1 serve \
     --api-base <url> --work-root <work_root> --worker-id <id> \
     --expect-version 0.1.1 --expect-digest <sha256>
   ```

   ```sh
   sudo systemctl daemon-reload
   ```

8. **Restart** (operator): `sudo systemctl restart ato-formation-worker`.

9. **Verify**: the worker starts with the journal `updating`, matches the
   target, reports `verifying` and then `active`, and starts serving. Check:

   ```sh
   scripts/ops/formation-worker-update.sh status --work-root "$WORK_ROOT"  # phase: active
   sha256sum /usr/local/bin/ato-formation-worker-0.1.1   # matches the record
   journalctl -u ato-formation-worker -o cat | head -5   # identity + sha256 lines
   ```

   The control plane's worker state/heartbeat must show the target
   `agent_version`; reporting `active` with a matching target clears the
   update request (`drained_at` stays set — undrain is still operator-owned).

10. **Undrain** (operator): `POST /v1/admin/runners/<id>/undrain`, then confirm
    the worker claims. Because the worker may exit `75` on the first claim
    while `drained_at` was still set, the sequence after a swap can be:
    worker verifies → exits `75` (residual drain) → operator undrains →
    clears the quiescent journal (step 11) → starts the service.

11. **Clear a finished lifecycle** (operator, only when no update is in
    progress): if the journal is `quiescent` and the control plane is
    undrained with no pending update, removing `worker-update.json` returns
    the host to `active` on the next start (the contract's "absent journal"
    case). Use `clear-journal` in the script so it checks `quiescent` first.
    Do not remove a journal in `updating`/`verifying`: it is the only record
    that a swap is in progress.

12. **Smoke**: submit one Formation job and confirm it completes with a
    terminal result.

## Procedure: rollback

Triggered by exit `74` (journal `rolled_back`) or by the operator deciding the
new binary is wrong.

1. The worker already refuses to claim: `rolled_back`/`quarantined` exit `74`.
   Read the `detail` field for what it compared.
2. Restore the previous binary (the script's `rollback` action verifies and
   installs `<install-to>.prev`); confirm its `sha256sum` and `--version`
   against the deployment record.
3. Remove the journal (operator action; `clear-journal --force-terminal`),
   because `rolled_back` is terminal by design. An absent journal means
   `active`, so the restored binary can serve again.
4. Remove the `--expect-*` flags that named the failed target (or set them to
   the restored binary's version/digest) and `systemctl daemon-reload`.
5. Start the service, confirm `--version`/sha256 in the log and the control
   plane state, then undrain and smoke as above.
6. Record the failed target, the mismatch (version/digest), and the restored
   binary in the deployment record.

Do not delete a `quarantined` journal without preserving
`worker-update.json.corrupt`: it is the evidence of what the worker could not
read.

## What is not automated

- Building, distributing, signing, or attesting the binary. There is no
  artifact pipeline for this worker (`[package.metadata.dist] dist = false`).
- Editing the systemd unit/drop-in and `daemon-reload`. The script prints the
  snippet; applying it is an operator action.
- Deciding to update or roll back, and undraining.
- Recovery of jobs left `running` in the control plane by a hard kill
  (SIGKILL). The worker reports terminal outcomes for jobs it finishes; a
  killed process cannot report, and the control plane's reconciliation of that
  row is outside this document.
- Actual smoke verification.
