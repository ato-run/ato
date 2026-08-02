# ADR-016: Runtime CPU Entitlement (host-side, snapshot-shape-preserving)

Status: Draft
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

3. **Allocation is deterministic integer max-min fairness** over a per-runner
   millicore budget (production 8000m, staging 6000m). Every active session is
   guaranteed its minimum; spare budget fills evenly up to each session's
   maximum; a session that saturates below the even share returns the remainder
   to the others; integer residue is distributed in `slot_index` order. No
   floats (host-to-host determinism), no priority weights in v1. If the sum of
   minimums would exceed the budget, the NEW claim is rejected rather than
   shrinking a running session below its floor.

   With `min = 1000m`, `max_slots = 4` and `budget >= 4000m`, every admissible
   slot count keeps its floor, so **the API stays CPU-unaware in v1**: capacity
   gating remains `open_lease_count < effectiveMaxSlots`. The runner enforces
   fairness locally.

4. **Enforcement is host cgroup only.** The runner creates a delegated
   cgroup per slot and writes `cpu.max` = `<quota> <period=100000>`. On
   reallocation, quotas are lowered before they are raised so the sum never
   transiently exceeds the budget. The Firecracker PID is attached to its slot
   cgroup BEFORE `InstanceStart`, so no boot window runs unthrottled. Failure to
   apply a quota rolls back to the previous allocation and refuses the new
   launch; a failed rollback marks the allocator unhealthy and stops new lease
   polling while existing VMs continue on their last-applied quota.

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
  systemd `Delegate=yes`, pre-`InstanceStart` PID attach, capability
  advertisement, `ATO_RUNNER_CPU_ENTITLEMENT` flag (default off).
- **PR 3 (ato-api):** `performance_preference` → server-resolved request, shared
  lease-command composer, `runner_leases` columns, capability gate, admin read
  model.
- **PR 4 (ato-pwa):** Standard/Economy two-way choice on the Run surface.
- **PR 5:** staged rollout + E2E.
