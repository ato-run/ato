# Ready-State latency — tiny-http

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=false

Sealed: rootfs 128.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 640.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 32238.059561999995 | 32342.534829999997 | 32887.756453999995 | 32887.756453999995 | 32887.756453999995 |
| restore cold-cache | 905.8437240000001 | 965.158955 | 973.770276 | 977.323397 | 980.4974400000001 |
| restore warm-cache | 631.368894 | 718.106975 | 726.043194 | 746.217954 | 771.420895 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
