# Ready-State latency — light-python-hs1

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=true

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 7960.207498 | 8396.390163 | 12538.809364 | 12538.809364 | 12538.809364 |
| restore cold-cache | 1506.024861 | 1561.437433 | 1576.1407629999999 | 1597.409411 | 1620.4255380000002 |
| restore warm-cache | 212.038397 | 218.032428 | 220.965818 | 221.770303 | 222.623884 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
