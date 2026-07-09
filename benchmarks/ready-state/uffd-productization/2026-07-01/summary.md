# U9 Ready-State File-vs-UFFD benchmark (2026-07-01)

Real-app benchmark for the UFFD productization track (#876, umbrella #873). Harness:
`snapshot`'s `uffd_bench` bin. Host: GCP `n2-standard-4` (4 vCPU / 15.6 GB), Firecracker
v1.16.0, guest kernel `vmlinux-5.10.223`, 512 MiB guest memory, ro-shared rootfs.
5 iterations/mode; medians below. Raw per-app JSON in `raw/`.

**Apps (all no-binding):** `tiny-http` (alpine + darkhttpd, 128 MB rootfs) ·
`light-python` (python:3.11 + FastAPI/uvicorn, 1 GB rootfs) · `light-node` (node:20 +
express, 1 GB rootfs). Each serves `/health`.

**Modes:** `file-cold` (mem cache cleared → eager rehydrate) · `file-warm` (mem cached
on disk) · `uffd-local` (demand from local CAS) · `uffd-hotset` (demand + hotset
prefetch) · `uffd-remote` (read-through a simulated remote CAS). Only the bench sets
`ATO_FC_UFFD*`; `ato run` is never touched.

## Results (median restore ms → lower is better)

| app | file-cold | file-warm | uffd-local | **uffd-hotset** | uffd-remote |
|---|---:|---:|---:|---:|---:|
| tiny-http | 803 | **118** | 180 | 137 | 165 |
| light-python | 903 | **218** | 433 | 260 | 428 |
| light-node | 873 | **193** | 443 | 233 | 465 |

Time-to-health (ms) and demand faults show *why*:

| app | uffd-local h / faults | uffd-hotset h / faults / prefetched | uffd-remote remote-chunks |
|---|---|---|---|
| tiny-http | 70 / 914 | 34 / **2** / 915 | 0 |
| light-python | 317 / 2999 | 132 / **4** / 2997 | 0 |
| light-node | 316 / 3036 | 107 / **2** / 3033 | 0 |

## Conclusions (the U9 required set)

**1. Where UFFD beats File — a cold cache.** Against `file-cold`, UFFD wins decisively:
`uffd-hotset` is **3.3–6×** faster (137–260 ms vs 803–903 ms), and even demand-only
`uffd-local` beats cold. On the first restore on a host (or after cache eviction), File
must eagerly rehydrate the whole 512 MiB before `LoadSnapshot`; UFFD faults in only the
working set. This is UFFD's product value: **cold cache, and by extension large/remote
memory images** where the eager rehydrate cost scales with image size while the working
set stays small.

**2. Where File wins / ties — a warm cache.** `file-warm` is the fastest mode
(118–218 ms): once the `.mem` is materialized on disk it is content-addressed and
reused, so a warm restore is Firecracker-bound with no rehydrate. `uffd-hotset` is
**competitive but ~40 ms behind** on the 1 GB apps (tiny-http is within ~20 ms). The
trade: `file-warm` requires the full 512 MiB materialized on disk *per capsule*;
`uffd-hotset` needs **0 bytes materialized** (served from CAS) and still lands in the
warm-File range.

**3. Is hotset needed? Yes.** Demand-only `uffd-local` is 180–443 ms; the hotset profile
cuts demand faults **~3000 → 2–4** and brings restore to 137–260 ms — closing most of
the gap to `file-warm`. Without hotset, UFFD is ~2× slower than warm File; with it, it's
within tens of ms. **Hotset is the mechanism that makes UFFD viable when File is warm.**

**4. Is remote read-through worth a product preview? Not by default.** `remote_chunks_
fetched = 0` across all apps: **content-addressed dedup already puts 52 of the 55 unique
memory chunks locally** (shared with the rootfs / zero-pages), so the "remote" store
truly holds only ~3 mem-unique chunks and the boot working set is served locally →
`uffd-remote ≈ uffd-local`. That is a *strong* result for CAS (dedup shrinks any remote
transfer to the mem-unique working set), but it also means remote read-through buys
little unless a capsule's memory image has substantial unique content absent locally.
The read-through path itself is proven mechanically by the U6 KVM smoke (50/256 chunks
when memory is remote-only). **Keep remote off by default; it graduates only in P4 with a
real network CAS and a locality/failure policy.**

## Takeaway for productization (feeds U16)

- Enable UFFD (with hotset) for **cold-cache / large-image / no-binding** restores — the
  clear win.
- When File is warm and the image is small, File is already competitive; UFFD-hotset is
  close and avoids the on-disk materialization, but there's no urgency to switch.
- Remote is a P4 concern; local CAS + hotset is the product-preview target (U11/U15).

This matches the conditions written down in `../../ready-state/uffd-mem-backend.md`
(#871). Selector wiring is the U10/U14 dry-run → U15 opt-in preview.
