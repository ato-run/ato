# Connected Runner concurrency: `--max-slots` / `ATO_RUNNER_MAX_SLOTS`

How to control how many capsule runs a single `ato runner serve` process will
execute **concurrently** (the "N-slot" executor, #632).

## What it is

By default a Connected Runner serves **one** run at a time. When a lease is
dispatched to it while it is already running something, the runner does not
claim a second lease until the first slot frees up.

`--max-slots` (and its environment-variable equivalent, `ATO_RUNNER_MAX_SLOTS`)
raise that ceiling so the runner claims and runs up to `N` leases at once. Each
slot is fully independent:

- slot `i` gets its own reverse-proxy port, `base_port + i` (`--proxy-listen`
  sets the base; default base is `127.0.0.1:8420`), so concurrent runs never
  collide on a port;
- slots claim leases and stop independently — one slot finishing or failing
  does not affect the others;
- with more than one slot, only slot 0 gets a public URL from
  `--public-base-url` by default. To expose every slot, also set
  `--public-url-template` / `ATO_RUNNER_PUBLIC_URL_TEMPLATE` so your
  ingress/tunnel can route `{port}` or `{slot}` to the right place (see
  `ato runner serve --help`).

## Default and resolution order

| Precedence | Source | Notes |
|---|---|---|
| 1 (highest) | `--max-slots N` flag | explicit, wins over everything |
| 2 | `ATO_RUNNER_MAX_SLOTS` env var | used when the flag is absent — this is how the systemd unit (`EnvironmentFile=/etc/ato/runner.env`, which runs `ato runner serve` with no flags) configures it |
| 3 (default) | `1` | single-slot behavior; matches every runner before #632 |

Whatever value comes out of that resolution is clamped to **`[1, 64]`** — an
operator cannot request 0 slots (that would silently wedge the runner) or an
unbounded number.

This default of **1** is intentional for user-owned Connected Runners and is
**not** changed by this doc. It is a deliberately conservative default:
raising concurrency is something an operator opts into once they know their
hardware can take it, not something that happens implicitly.

> **Managed Cloud is a different system.** Ato's own hosted (Managed Cloud)
> runners are provisioned and configured independently of this CLI flag —
> a separate, hardcoded slot count on that side is being addressed in
> `ato-api`, not here. This doc and the `--max-slots`/`ATO_RUNNER_MAX_SLOTS`
> settings only apply to Connected Runners you run and serve yourself.

## Raising it

```sh
# One-off / foreground:
ato runner serve --max-slots 4

# Or via env (what the systemd unit reads from /etc/ato/runner.env):
echo 'ATO_RUNNER_MAX_SLOTS=4' | sudo tee -a /etc/ato/runner.env
sudo systemctl restart ato-runner-serve
```

Only raise this on a Connected Runner that can genuinely sustain that many
capsule runs **at the same time** — see the caution below before picking a
number.

On startup, `ato runner serve` logs the slot count it resolved (and, when it's
still the default, a reminder of how to change it), so you can confirm what's
actually in effect without re-deriving it from flags/env:

```
🛰  Connected Runner heartbeat
   ...
   Slots:  1 concurrent run(s); per-slot proxy from 127.0.0.1:8420
           (default; override with --max-slots or ATO_RUNNER_MAX_SLOTS, max 64)
```

## Caution: resource implications

Each slot is a full, independent capsule run sharing the same host. Before
raising `--max-slots` above 1, make sure the machine actually has the
headroom for that many concurrent runs:

- **CPU/RAM**: every slot's process (and any sandbox/VM backend it uses) is a
  separate resource consumer; N slots ≈ N× a single run's footprint, not free
  parallelism.
- **GPU workloads**: native-inference / GPU-bound capsules generally should
  **not** be run at `--max-slots > 1` unless you have already confirmed the
  GPU (VRAM, compute) can serve multiple concurrent workloads — otherwise
  slots will contend for the same device and all of them will degrade.
  Multi-GPU or GPU-partitioning setups need explicit capsule-level placement,
  which this flag does not provide.
- **Disk / network**: concurrent runs multiply local disk I/O and outbound
  network usage; watch for the host's tunnel/proxy bandwidth becoming the
  bottleneck before compute does.
- **Blast radius**: a resource-starved host can degrade *every* slot at once
  (noisy-neighbor effect) rather than failing just the offending run.

When in doubt, raise `--max-slots` incrementally and watch host resource
usage under real concurrent load rather than jumping straight to a large
number.
