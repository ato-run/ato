# Store Capsule Snapshot Benchmark Harness (L6 — #912)

The empirical harness that measures how well **real Store capsules** (public, no-binding,
`capsule.toml`-only) snapshot + restore on Linux/KVM — to inform the **Track C** production
builder. **This is not the production builder** and it does not write `capsule_snapshots`
rows; it is a local measurement command.

## What it does

Given a **capsule list**, for each capsule it runs the full pipeline and records the
result — **failures are recorded, never hidden**:

```
eligibility → build → boot → verify → snapshot → seal → no-secret scan
            → restore → healthcheck → stop → File cold/warm benchmark
```

Binary: `snapshot`'s `store_bench`.

```sh
sudo -E env ATO_READY_STATE_BENCH=1 ATO_FC_BIN=~/bin/firecracker ATO_FC_KERNEL=~/bin/vmlinux \
  ATO_FC_WORK=/tmp/ato-fc-store ATO_FC_ROOTFS_READONLY=1 \
  target/release/store_bench --capsules capsules.json --iterations 5 --out out/
```

## Input — capsule list

`capsules.json` is an array of approved, **no-binding** capsule specs:

```json
[{ "capsule_id": "…", "rootfs": "/path/app.ext4", "healthcheck": "/health", "port": 8080, "target_label": "web" }]
```

- **`rootfs`** is a prebuilt bootable ext4 for the capsule (serving its healthcheck).
- **No client-controlled `source_ref` is fetched here.** Source refs are resolved
  **server-side / from an approved store record** *before* producing the rootfs; the
  harness only measures. Exclude: `secrets_required`, `[bindings.*]`, `[external.*]`,
  OAuth, user-file, large-model, GPU capsules.

See `benchmarks/ready-state/store-bench/capsules.example.json`.

## Output

`out/results.json` + `out/summary.md`. Per capsule: eligibility, success, no-secret-scan
result, `build_to_seal_ms`, artifact sizes (rootfs/mem/vmstate/cas_chunks,
artifact_manifest_hash, runner_class_id), File cold/warm restore p50/p95, and — on
failure — `failure_stage` + `failure_reason`. The File cold/warm metrics are comparable
to the U9 synthetic benchmark (`uffd-productization/`).

## Relationship to Track C

- **L6** (this): empirical harness; runs approved Store capsules locally; measures
  success/failure/performance; informs builder rules. Does **not** write production
  `capsule_snapshots`.
- **Track C**: the production/staging builder that claims queued jobs from ato-api and
  writes `capsule_snapshots`. L6 is not blocked on Track C, and vice versa.

## What L6 answers

How many toml-only Store capsules snapshot cleanly as-is; the main failure causes
(missing healthcheck, build-command ambiguity, port detection, dependency time, rootfs
size, network dependency, long boot, unsupported runtime); restore speed on real
capsules vs the synthetic benchmark; the eligibility rules the builder needs; and the
blocker reasons a Store card should surface.

> **Running against 5 real Store capsules requires the approved source refs / prebuilt
> rootfs images**, which are produced server-side (Store / ato-api) — not on the Linux
> build box. The harness + schema are ready; supply the capsule list to run it.
