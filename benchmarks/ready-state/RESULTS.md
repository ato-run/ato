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
