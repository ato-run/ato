# Ready-State latency — light-node-hs1

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=true

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 8014.913088 | 8070.227325999999 | 12590.365058 | 12590.365058 | 12590.365058 |
| restore cold-cache | 1492.562387 | 1547.142142 | 1564.2171199999998 | 1577.650407 | 2730.537309 |
| restore warm-cache | 184.396057 | 188.21245100000002 | 191.372136 | 192.13719300000002 | 194.87084099999998 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
