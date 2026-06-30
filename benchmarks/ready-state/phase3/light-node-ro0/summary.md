# Ready-State latency — light-node-ro0

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=false

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 7977.643465 | 8288.607985 | 12632.477802000001 | 12632.477802000001 | 12632.477802000001 |
| restore cold-cache | 2181.62932 | 2238.743578 | 2253.6729339999997 | 2309.405936 | 3459.016217 |
| restore warm-cache | 5581.902725 | 5758.180775999999 | 5787.83468 | 6094.612824000001 | 6203.788878 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
