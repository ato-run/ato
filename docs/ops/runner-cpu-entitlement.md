# Runner CPU entitlement (ADR-016) — operations runbook

Host-side, per-session CPU quotas (cgroup v2 `cpu.max`) over a fixed guest
Machine Shape (2 vCPU / 3072 MiB — snapshots are never resized). Classes are
server-resolved: **economy 1000m/1000m**, **standard 1000m/2000m** (default).
Design + invariants: `docs/rfcs/draft/ADR-016-runtime-cpu-entitlement.md`.

## Requirements

- Linux, cgroup v2 unified hierarchy, **systemd ≥ 254** (`DelegateSubgroup=`).
  Older systemd → preflight fails → the runtime **Faults** (by design).
- The runner service must run with the delegation drop-in
  (`/etc/systemd/system/ato-runner-agent.service.d/50-cpu-delegation.conf`:
  `Delegate=yes` + `DelegateSubgroup=main`). `ato runner setup --fix` writes it
  **only** when the env file selects enforce; feature-off hosts get zero unit
  diff.

## Environment (`/etc/ato/runner.env`)

| var | values | meaning |
|---|---|---|
| `ATO_RUNNER_CPU_ENTITLEMENT` | `off` (default) / `enforce` | single feature gate |
| `ATO_RUNNER_CPU_BUDGET_MILLIS` | int > 0 (default 8000) | per-runner millicore budget (staging 6000 / prod 8000) |
| `ATO_RUNNER_CPU_CGROUP_ROOT` | path | override delegated root (tests/non-standard layouts) |
| `ATO_RUNNER_CGROUP_MOUNT` | path (default `/sys/fs/cgroup`) | unified mount override |

## Enabling (per host)

1. Ingress capacity first if also growing slots:
   `PUT /v1/admin/runners/:id/ingress/capacity {expected_max_slots, max_slots}`
   (grow-only, CAS; see ato-api#455), then raise `ATO_RUNNER_MAX_SLOTS`.
2. Set `ATO_RUNNER_CPU_ENTITLEMENT=enforce` (+ budget) in the env file.
3. `sudo ato runner setup --official-preview --public-base-url <base> \
   --max-slots <N> --fix --yes` — writes the delegation drop-in +
   regenerates the Caddyfile, daemon-reloads.
4. `sudo caddy validate --config /etc/caddy/Caddyfile && sudo systemctl reload caddy`
5. `sudo systemctl restart ato-runner-agent`
6. Verify the journal: `CPU entitlement: ENFORCE (budget Nm, K slots)` and
   heartbeat capabilities include `runtime-cpu-entitlement-v1`.

## States & what to do

- **Off** — legacy behavior, no cgroup activity, no capability. Nothing to do.
- **Active/Healthy** — capability advertised; leases carry
  `runtime_cpu_request` and the VMM is attached to
  `<unit>/ato-slots/ato-slot-<i>` before `/snapshot/load`.
- **Faulted** (`enforce requested but FAULTED (…)` in the journal) — the host
  cannot deliver enforcement (old systemd, missing drop-in, no cpu controller).
  Heartbeat continues; **workload claims stop**. Fix the host (drop-in present?
  `systemctl show ato-runner-agent -p Delegate` = yes? systemd ≥ 254?) and
  restart. Never "fix" by silently going off unless you mean to disable the
  feature.
- **Unhealthy** (`🚨 cpu-entitlement …` in the journal) — a cgroup rollback or
  reclaim failed; the capability drops at the next heartbeat and new claims
  stop, existing VMs keep their last-applied quota and teardown/release still
  work. Inspect `<unit>.service/ato-slots/` for stray cgroups, then restart the
  runner (state is rebuilt from empty; startup reconcile settles leases).

## Verifying on the host

```sh
S=/sys/fs/cgroup/system.slice/ato-runner-agent.service/ato-slots
for d in $S/ato-slot-*; do echo "$d: $(cat $d/cpu.max) procs=$(cat $d/cgroup.procs|tr '\n' ' ')"; done
# quota: "200000 100000" = 2000m; procs must be exactly the slot's firecracker pid
grep -c fc_vcpu /proc/<pid>/task/*/comm   # must be 2 (guest shape unchanged)
cat $S/ato-slot-<i>/cpu.stat              # nr_throttled grows only under real contention
```

## Rollback

1. `ATO_RUNNER_CPU_ENTITLEMENT=off` in the env file, delete the
   `50-cpu-delegation.conf` drop-in, `systemctl daemon-reload`, restart the
   runner. Off is byte-identical legacy behavior.
2. PWA: unset `VITE_RUNTIME_CPU_PREFERENCE_ENABLED` and redeploy (field
   disappears; API default Standard).
3. API needs nothing (composer only embeds for capability-advertising runners;
   the `runner_leases.runtime_cpu_*` columns are nullable, no down migration).
4. Do NOT shrink ingress slots — lower `ATO_RUNNER_MAX_SLOTS` and let
   heartbeat capacity drop; extra slot rows are inert.

## Admin visibility

`runner_leases.runtime_cpu_class/min/max` mirror the lease contract (all-NULL =
legacy). Manager health/generation and applied quotas are host-side (journal
`🧮 cpu-entitlement` lines + the cgroup files above); surfacing them in the
Admin Console read model is tracked separately.
