# Ready-State latency — first hardware results (2026-06-30)

Host: GCP **n2-standard-4**, Intel **Cascade Lake**, x86_64, kernel 6.17.0-gcp,
**Firecracker v1.16.0**, cgroup v2, pd-ssd. Mode: `ATO_FC_ROOTFS_READONLY=0`
(fresh-copy rootfs — the only hardware-validated mode). N = **5 builds**
(retry-robust) / **30 restores** per target/mode.

## Headline numbers (ms)

| target | sealed (rootfs/mem) | build median | restore cold p95 | restore warm p95 |
|---|---|---|---:|---:|---:|
| tiny-http  | 128 MB / 512 MB | **32.3 s** | 977 | **746** |
| light-python | 1 GB / 512 MB | **78.7 s** | 2266 | 6162 ⚠ |
| light-node | 1 GB / 512 MB | **159 s** | 2274 | 6189 ⚠ |

## Decomposition (per-span, representative run)

**Build** — dominated by the no-secret scan:

| span | tiny | light-python | light-node |
|---|---:|---:|---:|
| start_fc + boot_to_health | ~2.1 s | ~2.1 s | ~2.1 s |
| snapshot_create | ~7 s | ~7 s | ~7 s |
| store_rootfs + seal_store | ~1.5 s | ~1.6 s | ~1.6 s |
| **no_secret_scan** | **~22 s** | **~70 s (84%)** | **~146 s (92%)** |

**Restore** — Firecracker is ~150 ms; the rest is Ato rehydrate/copy:

| span | cold-cache | warm-cache |
|---|---:|---:|
| start_fc + load_snapshot + wait_health (**Firecracker**) | **~150 ms** | **~150 ms** |
| cache_mem (rehydrate 512 MB) | ~687 ms | **0** (cached) |
| cache_rootfs (fresh-copy 1 GB, rw mode) | ~1347 ms | **~5900 ms** ⚠ |

## Answers to the 5 questions

1. **Snapshot creation:** ~7 s for a 512 MB-mem guest (`snapshot_create`). The
   large *build* total is the scan, not the snapshot.
2. **Restore → usable:** tiny **sub-second** (warm 746 ms); 1 GB-class **~2.3 s**
   cold. Not the claimed 100 ms — that was the raw bash spike.
3. **Firecracker vs Ato:** Firecracker ≈ **150 ms (~7%)**; Ato rehydrate/copy ≈
   **93%** of restore. The VMM is not the bottleneck.
4. **Scaling:** *restore* scales with rootfs+mem size (copy/rehydrate). *build*
   scales with total **sealed bytes** because the byte-level scan is O(n) over
   every layer — 70–150 s for 1.5 GB.
5. **Class:** tiny = sub-second/warm; 1 GB-class = low-seconds; **none limited by
   Firecracker** — limited by Ato I/O, and *build* by the scan.

## ⚠ The warm-cache anomaly (warm > cold for 1 GB rootfs)

For the 1 GB-rootfs apps, **warm-cache restore is *slower* than cold** (p95
~6.2 s vs ~2.3 s); tiny-http is normal (warm 746 < cold 977). Cause: in
**fresh-copy mode the 1 GB rootfs is rewritten on every restore** (`cache_rootfs`),
and 30 consecutive 1 GB overwrites with no cache-clear build up dirty-page
write-back pressure, so each warm write blocks longer (~5.9 s) than a freshly
written cold one (~1.3 s). The mem cache *does* help (cache_mem 0 in warm), but
the rootfs rewrite dominates and degrades. **This is exactly what Phase 3
(read-only shared rootfs) eliminates** — a ro-shared immutable rootfs is never
rewritten, so warm < cold and both approach (Firecracker 150 ms + mem rehydrate).

## What this directs

- **Phase 2 (no-secret scan):** the #1 build cost. Content-address + cache the
  scan of byte-identical base layers, stream-scan during store (read once), and
  bound/defer the advisory scan of per-build mem/vmstate. Target: build scan →
  seconds.
- **Phase 3 (read-only shared rootfs):** the restore fix. Removes the per-restore
  1 GB rootfs copy; should push warm restore toward sub-second for 1 GB apps.

## Caveats / scope

- Fresh-copy mode only; **read-only-shared rootfs not yet hardware-validated**.
- tiny-http uses alpine + darkhttpd; light-python = FastAPI/uvicorn; light-node =
  Express. **medium-python / heavy-assets not yet run** (need larger rootfs
  images) — follow-up.
- Raw per-run records + host facts: `<target>/{raw.jsonl,receipt.json,summary.md}`.

---

# Phase 3 — read-only-shared rootfs (2026-06-30)

Same host class (n2-standard-4 / Cascade Lake / FC v1.16.0 / pd-ssd). Rebuilt the
rootfs images **ro-bootable** (init mounts tmpfs over `/tmp`,`/run`,`/var/tmp`, so
the app needs no writable root) and ran the benchmark in **both** modes, 5 builds /
30 restores each. Raw records under `phase3/<target>-ro{0,1}/`.

## Restore latency — fresh-copy (ro0) vs read-only-shared (ro1), p95 ms

| target | cold ro0 | cold ro1 | **warm ro0** | **warm ro1** | warm speedup |
|---|---:|---:|---:|---:|---:|
| tiny-http    | 999  | 992  | 985  | **121**  | ~8× |
| light-python | 2275 | 2272 | 5959 | **222**  | **~27×** |
| light-node   | 2309 | 2297 | 6095 | **193**  | **~31×** |

## Why (light-python restore span decomposition, ms)

| span | ro0 cold | ro0 **warm** | ro1 cold | ro1 **warm** |
|---|---:|---:|---:|---:|
| cache_rootfs | 1359 | **6140** | 1369 | **0** |
| cache_mem | 698 | 0 | 682 | 0 |
| start_fc + load_snapshot + wait_health (Firecracker) | ~110 | ~153 | ~161 | ~152 |
| **total** | 2234 | **6360** | 2281 | **222** |

- **Fresh-copy (ro0) rewrites the 1 GB rootfs every restore** (`cache_rootfs`),
  and warm degrades to ~6 s under repeated-write/writeback pressure.
- **Read-only-shared (ro1) never rewrites the rootfs** (`cache_rootfs` = **0** in
  warm) — the immutable rootfs image is shared across all restores. Warm restore
  is now **Firecracker-bound (~150 ms)** + wait_health.
- Cold-cache is the same in both modes (~2.25 s): the first-touch rehydrate of
  rootfs (~1.36 s) + memory (~0.69 s) from CapsuleFS.

## Invariants (fc_kvm suite, read-only mode, fulltest FastAPI rootfs): **7/7 PASS**

probe · build→restore (+teardown: 0 firecracker / 0 tap / overlay removed) ·
restore_latency_20x (min 175 / median 220 / p95 895 ms) ·
**rootfs_is_read_only_shared_across_restores** (now RUNS in ro mode — rootfs image
mtime unchanged across restores ⇒ **no mutation**) · runner_class_mismatch
fail-closed · **state_leak** (marker did not survive a fresh restore) ·
**no_secret** (post-restore sentinel absent from sealed mem/vmstate).

## Verdict

**Read-only-shared rootfs mode is now hardware-validated.** It boots ro, builds,
restores, does not mutate the shared rootfs across restores, preserves the
state-leak and no-secret invariants, and tears down cleanly — while cutting warm
restore for 1 GB apps from ~6 s to ~0.2 s (the per-restore rootfs rewrite is
eliminated). This makes the shared rootfs a safe, reusable immutable artifact —
the substrate for treating a Capsule as a verified distribution object.

## Phase 6 hotset recommendation: target **memory** first

Warm restore is solved (Firecracker-bound ~150–220 ms). The remaining cost is
**cold-cache** (~2.25 s, first-touch). Of that, the rootfs rehydrate (~1.36 s) is
**amortized** by ro-shared — paid once per base-image per runner, then shared by
every restore. The **memory rehydrate (~0.69 s) is per-capsule** — every distinct
capsule's first restore on a runner pays it. So **Phase 6 hotset should prefetch
the memory chunks touched before first health** (the per-new-capsule cold cost),
with rootfs prefetch as a secondary base-image/runner-warming step. The
`HotsetRecorder` already records mem + rootfs chunks; prioritize memory in the
prefetch order.

## Scope / caveats
- x86_64 / Firecracker / File memory only; no lazy rootfs, no UFFD, no product
  wiring (all out of scope here).
- ro-bootable build scripts: `build_rootfs_ro.sh` + `run_builds_ro.sh` +
  `build_fulltest.sh` (the /marker+/secret FastAPI image for the invariant suite).
- medium-python / heavy-assets still not run (follow-up).

---

# Phase 6A — memory-first restore prefetch (2026-06-30)

Same host class (n2-standard-4 / Cascade Lake / FC v1.16.0 / pd-ssd), **read-only-shared
rootfs**, 5 builds / 30 restores per target × mode. `ATO_READY_STATE_HOTSET=1` rehydrates
memory/rootfs/vmstate **in parallel (memory-first)** instead of sequentially. Raw under
`phase6a/<target>-hs{0,1}/`. **This is restore I/O scheduling — NOT UFFD / lazy memory**
(File memory still needs a complete file before LoadSnapshot).

## Cold-cache restore p95 (ms), hotset off (hs0) vs on (hs1)

| target | cold hs0 | cold hs1 | Δ | warm hs0 | warm hs1 |
|---|---:|---:|---:|---:|---:|
| tiny-http    | 1000 | **821**  | −18% | 117 | 117 |
| light-python | 2263 | **1597** | **−29%** | 219 | 218 |
| light-node   | 2224 | **1578** | **−29%** | 189 | 188 |

Cold median for the 1 GB apps ~2.2 s → ~1.55 s. **Warm is unchanged** (both modes serve from
the content-addressed cache — nothing to rehydrate).

## Why (light-python cold-cache span decomposition, ms)

| | hotset off (sequential) | hotset on (parallel) |
|---|---:|---:|
| cache_mem / prefetch.memory | 685 | 686 |
| cache_rootfs / prefetch.rootfs | 1328 | 1346 |
| **rehydrate wall-clock** | **2013 (sum)** | **1346 (`prefetch.join` = max)** |
| start_fc + load_snapshot + wait_health | ~153 | ~150 |
| **total** | **2234** | **1561** |

The per-capsule **memory rehydrate (~686 ms) is fully overlapped behind the rootfs
rehydrate (~1346 ms)** — `prefetch.join` ≈ `max(rootfs, mem)` instead of their sum. Cold
moves toward `max(rootfs, mem) + Firecracker`, exactly as predicted.

## Invariants
ro-shared mode (no rootfs mutation), atomic materialization (temp+rename — no partial files),
fail-closed on any prefetch task error (Firecracker never started). #836 missing-artifact and
#837 binding guard unaffected. Off ⇒ sequential restore unchanged.

## Honest limits / next lever
- The residual cold floor is now **rootfs-bound** (~1.35 s rehydrate + ~0.15 s Firecracker ≈
  ~1.5 s). ro-shared **amortizes** that rootfs cost across all subsequent restores of the base
  (warm = ~0.2 s), so the per-capsule cold cost that hotset removed (the ~0.69 s memory
  rehydrate) is the meaningful win here.
- For **memory-heavy** apps (memory > rootfs), memory would dominate and overlap would hide
  rootfs behind it — at that point reducing cold further needs **UFFD / lazy memory** (a later
  phase): File-memory eager materialization caps how low cold can go because LoadSnapshot
  requires a complete memory file. Phase 6A does not claim demand-paged memory restore.

---

# Phase 7 — long-lived serving + ato stop (2026-06-30)

KVM-gated validation on n2-standard-4 / Cascade Lake / FC v1.16.0, **read-only-shared**,
`fulltest` FastAPI rootfs (/health + /marker + /secret). Full `fc_kvm` suite (now **8/8**,
`--test-threads=1`):

```
fc_kvm_probe_available ......................... ok
fc_kvm_build_restore_roundtrip ................. ok
fc_kvm_rootfs_is_read_only_shared_across_restores  ok
fc_kvm_restore_latency_ ........................ ok   (min 344 / median 394 / p95 1229 ms)
fc_kvm_runner_class_mismatch_fails_closed ...... ok
fc_kvm_state_leak_regression ................... ok
fc_kvm_no_secret_invariant ..................... ok
fc_kvm_cross_process_stop_via_record ........... ok   ← Phase 7
test result: ok. 8 passed; 0 failed.  (152.59s)
```

**`fc_kvm_cross_process_stop_via_record`** is the Phase 7 proof: restore a session, **drop the
restoring backend** (the detached VM keeps serving — confirmed by a live `/health` after the
drop), then a **fresh `FirecrackerBackend`** (empty in-memory registry, like a separate `ato
stop` process) reaps it purely from `overlay_root/.fc-session.json` → VM dead, **tap deleted,
overlay removed, zero orphan firecracker**. The other 7 confirm the `stop()` change (tap+pid now
read from the record) preserves every prior invariant (ro-shared no-mutation, state-leak,
no-secret, runner-class fail-closed, latency).
