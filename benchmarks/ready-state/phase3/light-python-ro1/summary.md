# Ready-State latency — light-python-ro1

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=true

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 7881.564702000001 | 9147.716141 | 12563.79832 | 12563.79832 | 12563.79832 |
| restore cold-cache | 2183.95165 | 2249.168531 | 2259.038564 | 2271.580655 | 2281.28639 |
| restore warm-cache | 178.26386 | 218.555712 | 220.458748 | 222.44475599999998 | 223.811767 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
