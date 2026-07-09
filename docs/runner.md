# Connected Runner

## Overview

A **Connected Runner** is a machine you enroll under your Ato account so it can
execute runs dispatched from the Ato control plane (`api.ato.run`). Once
enrolled, the runner appears in the Web Console (`app.ato.run/#route=/runners`),
sends liveness heartbeats, and polls for **run leases** — dispatched runs it
executes locally, sandboxed, and reports back honestly.

The runner agent is part of the `ato` CLI: everything lives under the
`ato runner` subcommand.

```text
Web Console (app.ato.run)          control plane (api.ato.run)
  "Run on <your machine>"  ──────▶  creates a run lease
                                        │
                                        ▼  (poll)
                              ato runner serve  (your host)
                                        │
                                        ▼
                              ato run <source> --sandbox
                                        │
                                        ▼
                     readiness probe → lease reported ready
                     traffic proxied via the slot proxy port
```

## How it works

### Host preparation and enrollment

There are two preparation tracks, each with its own diagnostic command —
don't mix them up:

**GPU / native-inference hosts** (serving local-LLM runs):

1. **`ato runner doctor`** — read-only GPU-host readiness check. Profiles:
   `nvidia-ubuntu` (default; Vulkan native-inference path for llama.cpp) and
   `nvidia-cuda` (SGLang CUDA path). Never mutates host state.
2. **`ato runner provision`** (root) — Dockerless NVIDIA/Vulkan
   native-inference provisioning: NVIDIA driver + Vulkan runtime + a
   `vulkaninfo` GPU smoke. Idempotent; `--resume` continues after a reboot,
   `--dry-run` prints the plan.

**Snapshot-serving hosts** (building and restoring Ready-State snapshots):

1. **`ato doctor runner`** (note: the *top-level* doctor, not
   `ato runner doctor`) — diagnostics-only readiness check for the all-in-one
   snapshot builder + capsule runner role: KVM, Firecracker, guest kernel,
   Docker, groups, tun/tap, artifact root, env file, runner token, the systemd
   services, and the derived Ready-State verdict (can this host
   `build_ready_state` / `restore_snapshot` today?). It installs and
   reconfigures nothing; the fixable set is applied by the next step.
2. **`ato runner setup`** (root) — prepares the host. Without `--fix` it only
   prints the derived plan; with `--fix` it installs Docker, the pinned
   (sha256-verified) Firecracker release and guest kernel, group grants, the
   artifact root, `/etc/ato/runner.env` (append-only), and two systemd units.
   `--official-preview` additionally configures Caddy per-slot ingress for an
   ato-managed `https://<slug>.runner.ato.run` hostname.
3. **`ato runner smoke`** (root + KVM + Docker) — a local, control-plane-free
   Ready-State smoke: Docker→ext4 rootfs → build (boot + healthcheck + seal) →
   restore → proxy HTTP probe → teardown → orphan diff. A green smoke means the
   host can actually build **and** serve capsule snapshots.

**Both tracks** then converge on:

- **`ato runner enroll`** — registers the host against the control plane via a
  browser device-flow sign-in (or a headless single-use `--enrollment-token`,
  used by Managed Cloud VMs), writes the systemd env file, verifies the
  control plane sees the runner as active, and with `--start` enables the
  runner service. Only the runner token is persisted — the operator session
  used for registration is discarded.
- **`ato runner status`** — shows the local systemd unit states plus the
  control plane's device view (active/online, last seen, public URL,
  supported lease kinds, slot capacity).

### The agent loop

`ato runner serve` (normally run by the systemd unit, not by hand) does two
things:

- **Heartbeats** — periodic liveness reports; the control plane derives the
  online/offline state shown in the Web Console from a recent-heartbeat window.
- **Lease polling** — claims dispatched runs and executes them locally with
  `ato run <source> --sandbox`. Readiness is **honest by construction**: a run
  is reported ready only on the local probe-confirmed signal, never optimistically.

Concurrency uses an **N-slot** model: slot `i` owns local proxy port
`base_port + i` (default base `127.0.0.1:8420`), so concurrent runs never
collide on a port. `--max-slots` / `ATO_RUNNER_MAX_SLOTS` set capacity
(clamped to `[1, 64]`); `--public-url-template` maps each slot's proxy port to
a public URL when the host's ingress supports it.

## Specification

- a runner MUST report ready only from a local probe-confirmed signal
  ("honest readiness"); there is no optimistic ready path
- dispatched runs MUST execute sandboxed (`ato run … --sandbox`)
- enrollment persists ONLY the runner token; operator sessions and enrollment
  tokens are single-use and never stored
- `/etc/ato/runner.env` is append-only for the tooling: operator-set keys are
  never overwritten, and existing files are backed up rather than clobbered
- the diagnostic commands (`ato runner doctor` for GPU readiness,
  `ato doctor runner` for snapshot-host readiness) MUST stay read-only; host
  mutation is confined to `provision` and `setup --fix` (root, with
  confirmation)
- slot ports are deterministic (`base_port + slot_index`) so the operator's
  tunnel or load balancer can be configured statically

References:

- `crates/cli/src/cli/runner.rs` (command surface)
- `crates/cli/src/application/runner_agent.rs` (agent implementation)
- [Snapshot v1 Compatibility Contract](snapshot-v1-compatibility.md) (what a
  snapshot-capable runner serves)
- [`ops/`](https://github.com/ato-run/ato/tree/main/docs/ops) runbooks
  (GPU profiles, max-slots, provisioning)

## Design Notes

The runner reuses the ordinary `ato run` pipeline instead of shipping a second
execution engine: a dispatched run is the same sandboxed launch a local user
would get, plus a lease-reporting wrapper. That keeps the safety model in one
place — and it is why "honest readiness" is cheap to guarantee: the probe that
gates a local `ato run` is the same signal the lease reports.

Hosts prepared with `setup --fix` also serve **Ready-State snapshots**
(sealed Firecracker artifacts restored per run) in addition to source runs;
see the [Snapshot v1 Compatibility Contract](snapshot-v1-compatibility.md) for
the exact supported application surface.
