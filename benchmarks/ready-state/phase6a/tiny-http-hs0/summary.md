# Ready-State latency — tiny-http-hs0

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=true

Sealed: rootfs 128.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 640.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 6195.644468 | 6223.601875 | 6739.517353 | 6739.517353 | 6739.517353 |
| restore cold-cache | 904.42035 | 961.220004 | 974.688488 | 999.8302540000001 | 1012.8275769999999 |
| restore warm-cache | 114.907719 | 117.121116 | 118.28370100000001 | 118.63211899999999 | 119.121967 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
