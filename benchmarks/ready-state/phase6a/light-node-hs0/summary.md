# Ready-State latency — light-node-hs0

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=true

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 8016.878258000001 | 9253.472443 | 13392.446118 | 13392.446118 | 13392.446118 |
| restore cold-cache | 2148.879882 | 2198.883462 | 2217.43651 | 2224.372 | 2259.212261 |
| restore warm-cache | 146.022023 | 189.281712 | 192.29476 | 193.929485 | 199.446023 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
