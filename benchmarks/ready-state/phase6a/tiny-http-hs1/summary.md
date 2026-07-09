# Ready-State latency — tiny-http-hs1

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=true

Sealed: rootfs 128.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 640.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 6164.3516629999995 | 6217.476151999999 | 6694.240822000001 | 6694.240822000001 | 6694.240822000001 |
| restore cold-cache | 793.739589 | 807.968171 | 816.667523 | 821.0850710000001 | 821.09483 |
| restore warm-cache | 73.281087 | 117.18792 | 118.720478 | 120.36095300000001 | 121.27916599999999 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
