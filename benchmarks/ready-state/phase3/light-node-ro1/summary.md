# Ready-State latency — light-node-ro1

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=true

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 7921.09141 | 8266.883727 | 12577.572344 | 12577.572344 | 12577.572344 |
| restore cold-cache | 2163.715313 | 2224.1900889999997 | 2242.309869 | 2297.1140010000004 | 3406.2837560000003 |
| restore warm-cache | 142.61448900000002 | 188.98880400000002 | 191.979847 | 193.23891600000002 | 193.86213899999998 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
