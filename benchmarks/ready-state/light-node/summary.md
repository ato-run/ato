# Ready-State latency — light-node

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=false

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 153778.83817200002 | 159359.888122 | 160762.275984 | 160762.275984 | 160762.275984 |
| restore cold-cache | 2190.7082400000004 | 2249.6222040000002 | 2270.41324 | 2274.228869 | 2277.8494610000002 |
| restore warm-cache | 5647.965407 | 5743.682978 | 5814.377032 | 6189.268019 | 6231.174443000001 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
