# Ready-State latency — tiny-http-ro1

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=true

Sealed: rootfs 128.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 640.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 6185.935897 | 6199.7448779999995 | 6755.130623999999 | 6755.130623999999 | 6755.130623999999 |
| restore cold-cache | 921.23894 | 972.7813130000001 | 986.3518310000001 | 992.004056 | 995.668998 |
| restore warm-cache | 114.762219 | 116.66576500000001 | 119.226317 | 121.41478599999999 | 122.54838799999999 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
