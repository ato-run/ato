# Ready-State latency — light-python-ro0

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=false

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 7932.81228 | 8443.973748999999 | 12524.816603 | 12524.816603 | 12524.816603 |
| restore cold-cache | 2205.263968 | 2257.929032 | 2271.004495 | 2274.550192 | 2284.920804 |
| restore warm-cache | 5384.090967 | 5784.79306 | 5814.482172 | 5958.592328 | 6359.5006060000005 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
