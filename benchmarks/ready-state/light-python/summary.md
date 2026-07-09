# Ready-State latency — light-python

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=false

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 78241.125706 | 78669.314587 | 82512.577882 | 82512.577882 | 82512.577882 |
| restore cold-cache | 2194.544149 | 2237.015306 | 2260.90568 | 2266.2680100000002 | 2300.9977049999998 |
| restore warm-cache | 5462.679443 | 5757.948324 | 5805.847314000001 | 6162.294285 | 6348.606353 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
