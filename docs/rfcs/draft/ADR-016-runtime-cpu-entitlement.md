# ADR-016: Runtime CPU Entitlement (host-side, snapshot-shape-preserving)

Status: Accepted (runner core + integration implemented; staged rollout in progress)
Date: 2026-08-02

## Context

A connected runner serves several restored snapshots concurrently, one per
execution slot (production 4, staging 3 — each slot owns its own netns, proxy
port and public ingress identity; unchanged by this ADR). Every snapshot is
pinned to a fixed guest Machine Shape — 2 guest-visible vCPU / 3072 MiB — that
is baked into the Firecracker checkpoint at seal time. A snapshot's vCPU count
and memory size are properties of the checkpoint: they cannot be altered at
restore, and altering them would change the artifact's identity and break
`ato.snapshot-manifest/v1` / `ato.snapshot-compatibility/v1` compatibility.

We nonetheless want a launch to be able to run "faster" or "cheaper" without
touching that shape or the snapshot bytes. On Linux this is exactly what the
host cgroup `cpu.max` quota expresses: the guest still SEES 2 vCPUs, but the
host caps how much CPU time those vCPU threads actually receive. That knob sits
entirely OUTSIDE snapshot identity.

## Decision

Introduce a **Runtime CPU Entitlement** that is resolved per session and
enforced only as a host cgroup `cpu.max` quota, never as a guest machine-shape
change.

1. **Machine Shape stays fixed and snapshot-owned.** Restore never passes or
   overrides `vcpu_count` / `mem_size_mib`; they are inherited from the
   checkpoint. Memory entitlement is NOT dynamic. This ADR adds no field to the
   snapshot manifest or compatibility contract.

2. **CPU request is a server-resolved class, not a client number.** The API
   accepts only a coarse `performance_preference` (`economy` | `standard`,
   default `standard`) and resolves it server-side into
   `ato.runtime-cpu-request/v1 { class, min_millis, max_millis }`, which is
   carried in the runner lease command outside the identity-bearing restore
   fields. Clients never send raw millicores.

   | class    | min    | max    |
   |----------|--------|--------|
   | economy  | 1000m  | 1000m  |
   | standard | 1000m  | 2000m  |

3. **Allocation is deterministic integer floor-first capped equal-share** over a
   per-runner millicore budget (production 8000m, staging 6000m). Every active
   session is guaranteed its minimum; spare budget fills evenly up to each
   session's maximum; a session that saturates below the even share returns the
   remainder to the others; integer residue is distributed in `slot_index`
   order. No floats (host-to-host determinism), no priority weights in v1. v1
   further requires every request to share ONE floor (both classes use 1000m),
   and under a shared floor this policy is exactly max-min fairness; a mixed
   floor is rejected rather than silently mis-shared. If the sum of minimums
   would exceed the budget, the NEW claim is rejected rather than shrinking a
   running session below its floor.

   With `min = 1000m`, `max_slots = 4` and `budget >= 4000m`, every admissible
   slot count keeps its floor, so **the API stays CPU-unaware in v1**: capacity
   gating remains `open_lease_count < effectiveMaxSlots`. The runner enforces
   fairness locally.

4. **Enforcement is host cgroup only.** The runner creates a delegated
   cgroup per slot and writes `cpu.max` = `<quota> <period=100000>`. On
   reallocation, quotas are lowered before they are raised so the sum never
   transiently exceeds the budget. The Firecracker PID is attached to its slot
   cgroup **before the first guest instruction executes** — which is NOT
   `InstanceStart` on the restore path: a restore spawns Firecracker, then
   resumes the guest with `PUT /snapshot/load {resume_vm:true}`, so the attach
   must land in the spawn→load window. Concretely: cold boot attaches before
   `InstanceStart`; restore attaches before `/snapshot/load`. Either way no
   guest instruction runs unthrottled. Failure to apply a quota rolls back to
   the previous allocation and refuses the new launch; a failed rollback marks
   the allocator unhealthy and stops new lease polling while existing VMs
   continue on their last-applied quota.

## Invariants

1. Restore never changes a snapshot's vCPU or memory.
2. No fallback to a different-shape snapshot.
3. `ato.snapshot-manifest/v1` and `ato.snapshot-compatibility/v1` are unchanged.
4. Memory entitlement is never dynamically changed.
5. Slots keep their netns / proxy / ingress identity.
6. Builder and static-origin workloads are NOT in the runner slot allocator.
7. Clients never supply raw CPU numbers; the API confirms class/min/max.
8. A CPU reservation is released only after VM teardown is confirmed
   (fail-closed, mirroring slot release).
9. A cgroup apply failure never launches a new VM.

## Rollout

Gated behind `ATO_RUNNER_CPU_ENTITLEMENT=off|enforce` (default `off`), advertised
as the runner capability `runtime-cpu-entitlement-v1`. Staging (3 slots / 6000m)
before production (4 slots / 8000m). The managed-ingress capacity-expansion API
(separate work) is a prerequisite for raising `max_slots` beyond its provisioned
value, because heartbeat only updates `runner_devices.max_slots` and does not add
ingress allowlist rows.

## Slices

- **PR 1 (this):** the pure `runner_cpu_allocator` module + this ADR. Integer
  max-min fairness with exhaustive unit tests. No production behavior.
- **PR 2:** `runner_cgroup` + `CpuEntitlementManager` single-owner actor,
  systemd `Delegate=yes`, PID attach before first guest instruction (before
  `InstanceStart` on cold boot, before `/snapshot/load` on restore), capability
  advertisement, `ATO_RUNNER_CPU_ENTITLEMENT` flag (default off).
- **PR 3 (ato-api):** `performance_preference` → server-resolved request, shared
  lease-command composer, `runner_leases` columns, capability gate, admin read
  model.
- **PR 4 (ato-pwa):** Standard/Economy two-way choice on the Run surface.
- **PR 5:** staged rollout + E2E.

## Implementation facts (as landed)

- Runner PRs: ato#1225 (allocator), ato#1226 (std-thread manager + cgroup v2
  backend), ato#1228 (integration). API: ato-api#454 (policy + capability gate +
  lease contract + migration 0150). PWA: ato-pwa#247 (Standard/Economy,
  `VITE_RUNTIME_CPU_PREFERENCE_ENABLED`). Ingress prerequisite: ato-api#455
  (grow-only capacity expansion).
- The manager is a dedicated std::thread actor (`ato-cpu-entitlement`) with
  sync_channel request/reply: safe to call synchronously from the pre-resume
  hook on any runtime; admissions refuse on a full queue (ManagerBusy);
  releases use a blocking send (a refused release would leak the slot).
- Admission completes only after a cgroup membership READ-BACK proves the pid
  landed; release verifies lease+slot+pid and reclaims (empty + removed)
  BEFORE freeing budget; post-reclaim survivor-rebalance failures never leak
  the slot (CpuReleaseOutcome).
- **enforce requires systemd >= 254**: `DelegateSubgroup=main` keeps the
  delegated unit cgroup free of interior processes (the cgroup v2 rule that
  otherwise makes children `domain invalid` on `+cpu`). Older systemd fails
  preflight → the runtime FAULTS (claims stop, heartbeat continues) — never a
  silent unthrottled fallback. `resolve_delegated_root` steps up from the
  DelegateSubgroup child to the unit cgroup.
- Environment: `ATO_RUNNER_CPU_ENTITLEMENT=off|enforce` (default off),
  `ATO_RUNNER_CPU_BUDGET_MILLIS` (default 8000), `ATO_RUNNER_CPU_CGROUP_ROOT`
  (override; tests), `ATO_RUNNER_CGROUP_MOUNT` (default /sys/fs/cgroup).
  systemd drop-in `<unit>.d/50-cpu-delegation.conf` written by
  `ato runner setup` ONLY when the env file sets enforce.
- States: Off (legacy), Active (capability `runtime-cpu-entitlement-v1`
  advertised while Healthy), Faulted (enforce requested, host can't deliver —
  claims stop). Active→Unhealthy: capability dropped next heartbeat, new lease
  polling stops, existing VMs keep their last-applied quota, teardown/release
  still processed.
- Acceptance evidence (ubuntu-sugamo, 2026-08-02, recorded on ato#1228):
  real-kernel cgroup acceptance + Firecracker E2E under enforce — pre-resume
  admission, fc_vcpu=2 invariant, decrease-first reallocation (2000→1500),
  quota burn capped at exactly 2.0 CPU (nr_throttled=49), survivor raise
  (1500→2000), full reclaim, feature-off parity.

## Rollback

1. Runner: set `ATO_RUNNER_CPU_ENTITLEMENT=off` (or remove it) in
   /etc/ato/runner.env, delete `<unit>.d/50-cpu-delegation.conf`,
   `systemctl daemon-reload`, restart the runner. Off is byte-identical legacy
   behavior; running VMs are unaffected by a restart-time flag flip apart from
   the restart itself.
2. PWA: unset `VITE_RUNTIME_CPU_PREFERENCE_ENABLED` and redeploy — the field
   disappears from requests; the API treats absence as Standard.
3. API: nothing to roll back — the composer only embeds for capability-
   advertising runners, and the 0150 columns are nullable/forward-compatible
   (no down migration by design).
4. Slot expansion is NOT shrunk: lower `ATO_RUNNER_MAX_SLOTS` on the box and
   let heartbeat capacity drop; extra ingress slot rows stay inert.
