# M0 Go / No-Go — acceptance criteria + receipt

Fill this in after running the spike on a KVM host. M0 is the feasibility gate for the whole
Ready-State line; a NO-GO on any hard criterion stops the project until resolved.

## Hard criteria (all must PASS)

- [ ] **Restore latency** — `restore→ready` p50 ≤ ~3000 ms (File-backed, hello app). (`receipt.json`)
- [ ] **Zero cross-restore state leak** — `./leak-test.sh 20` reports `leaks=0`.
- [ ] **runner_class mismatch detectable** — `./mismatch-test.sh`: either the VMM rejects the
      load under a different CPU template/FC version, OR it loads but proves the host-side gate
      is mandatory (documented). Conclusion recorded either way.
- [ ] **No secret in snapshot** — `./no-secret-test.sh` PASS (sentinel + provider-key patterns absent).

## Soft observations (record, don't gate)

- [ ] cold-boot→healthy ms (motivates whether warm restore is worth it)
- [ ] snapshot sizes (mem + vmstate) — motivates CapsuleFS chunking/UFFD
- [ ] network reconnect after restore: automatic vs needs guest-agent nudge (the biggest unknown)
- [ ] clock/timer drift after resume
- [ ] vsock re-handshake behavior

## Decision

```
HOST:            <arch / shape, e.g. BM.Standard.A1.160 aarch64>
FC_VERSION:      <...>
CPU_TEMPLATE:    <...>
restore p50/p95: <... / ...> ms
leak (20x):      <0 / N>
mismatch:        <vmm-rejected | loads-but-gate-required>
no-secret:       <pass | fail>

VERDICT: [ ] GO   [ ] NO-GO
Rationale:
Next: GO -> wire real FirecrackerBackend (issue F/E real-backend); NO-GO -> evaluate Cloud Hypervisor (plan §6 fallback).
```

## Backend choice note (feeds Workstream A)

Record which `BackendCapabilities` facets the real host actually supports
(`snapshot_kind`, `memory_snapshot`, `filesystem_model`, `device_profile`, `gpu_mode`,
`supports_seal_before_bind`, `supports_disposable_overlay`) so the FirecrackerBackend `probe()`
reports them truthfully.
