# Ready-State latency — light-python-hs0

Host: Intel Cascade Lake / n2-standard-4 / x86_64 / kernel 6.17.0-1020-gcp / fc Firecracker v1.16.0 / cgroup v2 / rootfs_ro=true

Sealed: rootfs 1024.0 MB · memory 512.0 MB · vmstate 0.0 MB · total 1536.0 MB

| metric (ms) | min | median | p90 | p95 | max |
|---|---|---|---|---|---|
| build total | 7961.458463999999 | 8509.218905000002 | 12722.506825 | 12722.506825 | 12722.506825 |
| restore cold-cache | 2174.935352 | 2232.823381 | 2249.640764 | 2263.121759 | 2358.168858 |
| restore warm-cache | 172.945616 | 218.851906 | 223.849176 | 226.285417 | 235.022664 |

See `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).
