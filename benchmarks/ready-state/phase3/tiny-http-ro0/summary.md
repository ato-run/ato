# Ready-State latency — tiny-http-ro0

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=false

Sealed: rootfs 128.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 640.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 6197.825742 | 6517.787064 | 6673.455823 | 6673.455823 | 6673.455823 |
| restore cold-cache | 921.5642429999999 | 978.764557 | 994.646329 | 999.357882 | 1016.142286 |
| restore warm-cache | 663.9063130000001 | 718.5586539999999 | 751.253325 | 985.236506 | 1377.525222 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
