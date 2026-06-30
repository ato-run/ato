# CRIU container checkpoint — Linux spike (#839)

> Status: **spike (Linux-only, not wired)**. This establishes the CRIU
> checkpoint/restore *compatibility contract* and a runnable Linux smoke. It is
> **not** wired into `ato run` / `ato build`, has **no** product path, and is
> **not** brought into the macOS Apple Containerization guest yet — that is a
> later milestone (Mac M4) gated on this spike.

CRIU (Checkpoint/Restore In Userspace) is a candidate **inner Ready-State
mechanism**: it checkpoints a *running* container's process tree to disk and
restores it later, so a warm session can resume without a cold boot. It is the
counterpart, at the container layer, to the Firecracker VM-memory snapshot.

Code: `crates/cli/src/application/criu_spike.rs` (Linux-only, unwired).
Related vocabulary: `desktop_runner::facts::ReadyStateKind::CriuCheckpoint`.

## Why Linux-first (before macOS)

Apple Containerization runs containers inside an optimized Linux VM with a
minimal rootfs and a per-container kernel config. CRIU has hard kernel-feature
dependencies (specific namespaces, `/proc` knobs, cgroup layout). The
checkpoint/restore *compatibility class* must therefore be pinned and proven on
a known Linux substrate first; only then is it sensible to test CRIU feasibility
inside the Apple Containerization guest (Mac M4).

## CRIU is NOT a security boundary

CRIU is a **resume mechanism**, not isolation. The isolation boundary remains
the VM / container (`vm_wrapped_container` on Apple Containerization, `container`
on a Linux host). A CRIU image must never be treated as a trust boundary or a
substitute for sandboxing.

## Checkpoint invariants (pre-bind, secret-free)

The same contract as a Ready-State sealed artifact (see
[`desktop-runner.md`](./desktop-runner.md) and the Phase 5.5 binding guard):

- A checkpoint is **pre-bind only**: it is taken **before** any runtime binding
  (`BindingLease`) is injected.
- **No** secret / OAuth / credential / user-file values are present in the
  checkpoint (no `[secrets.*]` / `[bindings.*]` / `[external.*]` values written
  to the CRIU image, rootfs, memory pages, or logs).
- **No** binding-attached checkpoint: a process that has already received its
  bindings must not be checkpointed for reuse across sessions.
- CRIU is not the security boundary (restated — it is load-bearing).

A binding-required capsule is therefore checkpointed only in its pre-bind state;
bindings are injected **after** restore, per session, exactly as the Ready-State
binding guard requires.

## Capability vocabulary

`CriuBackendCapability`:

| field | value (spike) |
|---|---|
| `ready_state_kind` | `criu_checkpoint` |
| `isolation_boundary` | `vm_wrapped_container` \| `container` |
| `requires_linux_kernel` | `true` |
| `requires_criu` | `true` |
| `container_runtime` | `runc` \| `crun` \| `podman` \| `containerd` |
| `criu_available` | probed (`criu --version`) |
| `maturity` | `experimental` |

## CRIU restore-compatibility class

`CriuRunnerClassFacts` — the facts that must match (exactly) for a checkpoint
taken on one host to restore on another. CRIU restore is brittle across these,
so the class is deliberately tight:

| field | why it matters for restore |
|---|---|
| `guest_os` / `guest_arch` | a checkpoint is arch- and OS-specific |
| `kernel_release` | CRIU restore is sensitive to kernel ABI/features |
| `criu_version` | image format / feature compatibility |
| `runtime_id` / `runtime_version` | runc/crun/podman manage the checkpoint bundle |
| `rootfs_image_digest` | the restored process expects its original rootfs |
| `cgroup_version` | v1/v2 layout differences break restore |
| `namespace_model` | the set of namespaces must match |

`first_divergent_field` / `ensure_compatible` report the first mismatch
(coarsest → finest), mirroring `RunnerClassFacts` for VM snapshots — so a wrong
host fails closed with the actionable field named, never a silent bad restore.

## Manual Linux smoke

`#[ignore]`d, Linux + a CRIU-capable container runtime required. It uses
`podman container checkpoint` / `restore` (which drive CRIU):

```sh
# On a Linux host with podman (or runc/crun) + criu installed:
cargo test -p cli criu_spike::criu_checkpoint_restore -- --ignored --nocapture
```

Flow: run a tiny HTTP container → TCP healthcheck → `criu dump` (checkpoint) →
`criu restore` → healthcheck the restored process → teardown. It emits a
`CriuSpikeReceipt` (runtime, criu/kernel versions, checkpoint/restore latency,
restored-reachable, cleanup). It runs **no** binding-required workload and
publishes nothing beyond the loopback healthcheck.

## Not in this spike

No `ato run` / `ato build` wiring, no Ready-State restore selection, no
`BindingLease` injection, no checkpoint-after-bindings, no macOS Apple
Containerization CRIU (Mac M4, gated on these Linux results).
