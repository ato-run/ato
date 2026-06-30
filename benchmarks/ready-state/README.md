# Ready-State Snapshot latency benchmark

Decomposes **build-time** and **run-time** latency for the Ready-State path and
separates **raw Firecracker** time (boot / snapshot / load / health) from **Ato
overhead** (CapsuleFS store / no-secret scan / seal / rehydrate / cache), across
cache modes and app weights.

It answers:

1. How long does snapshot **creation** take?
2. How long does **restore → usable** take?
3. How much of restore time is **Firecracker vs Ato** rehydrate/cache overhead?
4. How does latency **scale** with rootfs size, memory size, and app type?
5. Is restore **millisecond-class, second-class, or worse** per app class?

## Tooling

Instrumentation is opt-in and **zero-cost when off**: spans are recorded only when
`ATO_READY_STATE_BENCH=1` (see `snapshot::bench`). The harness is a dev binary,
`ready-state-bench` (not part of the product CLI). It measures one rootfs image
per invocation; the app-class matrix is driven by running it once per image.

```sh
sudo -E env \
  ATO_READY_STATE_BENCH=1 \
  ATO_FC_BIN=$PWD/firecracker \
  ATO_FC_KERNEL=$PWD/vmlinux \
  ATO_FC_ROOTFS_READONLY=0 \
  ATO_FC_WORK=/tmp/ato-fc-bench \
  cargo run -p snapshot --release --bin ready-state-bench -- \
    --rootfs $PWD/rootfs.ext4 --target tiny-http \
    --build-runs 5 --restore-runs 30 \
    --out benchmarks/ready-state
```

Output, under `benchmarks/ready-state/<target>/`:

- `raw.jsonl` — one record per run with per-span decomposition (ms) + sealed/restored bytes
- `receipt.json` — host facts + aggregated min/median/p90/p95/max
- `summary.md` — human-readable table

### Modes

- **cold-cache** — the content-addressed layer cache (`$ATO_FC_WORK/{mem,vmstate,rootfs}`)
  is cleared before each restore: this is the first-touch cost on a runner.
- **warm-cache** — cache primed once, then reused: the steady-state user-facing cost.
- **fresh-copy rootfs** (`ATO_FC_ROOTFS_READONLY=0`) — validated mode; rewrites the
  rootfs every restore (leak-safe by fresh copy).
- **read-only shared rootfs** (`ATO_FC_ROOTFS_READONLY=1`) — only if the rootfs boots
  read-only; **not yet hardware-validated** (see #831).

## App-class matrix

| class | example | rootfs | memory | restore target | status |
|---|---|---|---|---|---|
| A tiny-http | hello `/health` server | 200–500 MB | 128–512 MB | p95 < 0.5–1 s | runnable with the M0 hello rootfs |
| B light-python | FastAPI/Flask, small deps | 0.5–1 GB | 0.5–1 GB | p95 < 1–2 s | needs rootfs image (follow-up) |
| C light-node | Express/Vite preview | 0.5–1 GB | 0.5–1 GB | p95 < 1–2 s | needs rootfs image (follow-up) |
| D medium-python | PDF/text processing, larger deps | 1–3 GB | 1–2 GB | p95 < 3–5 s | needs rootfs image (follow-up) |
| E heavy-assets | large static/model files, CPU-only | 3–8 GB | 2–4 GB | measured only | needs rootfs image (follow-up) |

Each app-class rootfs (B–E) is built on the KVM host and fed to the harness via
`--rootfs`; only the tiny-http class is reproducible from the existing M0 rootfs.

## Host

Validation class (match where possible): GCP `n2-standard-4`, Intel Cascade Lake,
x86_64, Firecracker v1.16.0. Restore is storage-sensitive — `receipt.json` records
the disk rotational flag and cgroup version. Use local SSD/NVMe if available.

## Current hypothesis (from #831, to be confirmed)

- Raw Firecracker restore for a tiny app: ~100 ms (M0 bash spike).
- Ato integrated restore in fresh-copy mode: ~2–3 s for the hello rootfs (the
  per-restore rootfs copy dominates).
- Warm-cache + read-only/shared rootfs should approach sub-second — must be measured.
- Heavy apps will be dominated by rootfs/memory/cache IO unless lazy-restore /
  hotset / external model cache is implemented.

Out of scope here: GPU, UFFD, QEMU, Kata, aarch64. No product behavior changes.
