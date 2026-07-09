# Snapshot (Ready-State)

## Overview

A **Ready-State snapshot** is a sealed, content-addressed capture of a capsule
that has *already been built, booted, and verified ready*. Restoring the
snapshot brings the app back to its probe-confirmed serving state in roughly a
second — without re-running installs, builds, or startup.

Where `ato run` answers "run this source now", the snapshot path answers
"serve this capsule instantly, from warm state, on any capable runner".

```text
BUILD (once, on a KVM builder host)              RUN (per request, on a runner)
source / server-approved recipe                  sealed artifact
  │  Docker → ext4 rootfs                          │  restore under Firecracker
  ▼                                                ▼
boot under Firecracker                           deliver real bindings via vsock
  │  readiness probe must pass                     │  (only if the capsule has any)
  ▼                                                ▼
snapshot → seal → no-secret scan → register      ready → proxy traffic
```

The **supported application surface is a contract, not a best effort** — see
the [Snapshot v1 Compatibility Contract](snapshot-v1-compatibility.md) for
exactly which capsule classes seal, restore, and serve (guaranteed by E2E
fixtures), and which fail closed with which reason.

## How it works

### Build: seal before bind

The builder daemon (`crates/snapshot-builder`) claims capsule-snapshot jobs
from the control plane, materializes source from the *server-approved* recipe,
derives a rootfs (Docker → ext4), boots the app under Firecracker, verifies it
against its readiness probe, snapshots it, seals the result, scans every layer
for secrets, and registers the artifact.

Secrets and per-user state are **never baked in**, and the seal moment depends
on whether the capsule declares required restore-time bindings:

- **No-binding capsules** (the Snapshot v1 contract surface) seal with the
  workload **running** — the snapshot captures the probe-verified serving
  state directly, which is what makes restore instant.
- **Capsules with required bindings** (supervisor builds) boot with
  *placeholder* bindings delivered over vsock, verify health, then the host
  sends `StopWorkload` and revokes every placeholder so the snapshot is taken
  **workload-idle** with the tmpfs binding files scrubbed. Placeholder values
  are generated inside the backend and never stored.

In both cases a no-secret gate runs over every sealed layer (fail-closed on
any finding).

### Artifact: content-addressed layers

A sealed artifact is a `ReadyStateManifest` over content-addressed layers —
`rootfs / runtime / deps / app / vmstate / memory` — stored and deduplicated
through `crates/capsulefs` (a chunked, lazily-read local CAS). The rootfs is
the bootable disk, mounted read-only at restore so disk mutations can never
leak between sessions.

### Restore: bind at restore time

On a runner, restore rehydrates the microVM from `vmstate` + `memory`. A
no-binding capsule resumes serving directly. A capsule with required bindings
first receives the *real* bindings — secrets, durable state, per-session
configuration — via the in-guest agent (`crates/guest-agent`) over a vsock
control channel, and only then restarts the workload (bound-ready). Anything
session- or user-specific is a **restore-time binding, never build-time
state**.

The runner side is the same Connected Runner agent described in
[Connected Runner](runner.md): hosts prepared with `ato runner setup --fix`
(Firecracker + guest kernel, sha256-pinned) serve `restore_snapshot` leases,
and `ato runner smoke` proves a host can build **and** serve snapshots before
it is enrolled.

### Backends

`crates/snapshot` defines the backend-agnostic `SnapshotBackend` seam:

| Backend | Status |
|---|---|
| `FirecrackerBackend` | Real implementation (x86_64 KVM; file memory backend; REST over unix socket) |
| `FakeSnapshotBackend` | KVM-free backend driving the full build→seal→restore pipeline for tests |
| `QemuBackend` / `KataBackend` | Deliberate stubs reserving the virtio-fs/GPU and OCI-alignment paths |

## Specification

- a snapshot MUST be taken only after the app passes its readiness probe
  ("verified ready", never "probably ready")
- **seal before bind**: sealed layers MUST contain no secrets; the no-secret
  scan fails the build closed on any finding
- restore MUST reject a host whose `runner_class_id` differs from the class
  the snapshot was built for (fail-closed portability)
- GPU state MUST NOT be captured into a snapshot (fail-closed guard)
- durable state and secrets MUST be delivered as restore-time bindings over
  the vsock channel, never baked into layers
- builds with required bindings MUST snapshot workload-idle after
  `StopWorkload` + placeholder revocation; no-binding builds seal with the
  workload running (v1.0 no-binding contract)
- eligibility is decided at build time, fail-closed, with an actionable
  rejection reason — the full rule table (R1–R8) lives in the
  [compatibility contract](snapshot-v1-compatibility.md)

References:

- [Snapshot v1 Compatibility Contract](snapshot-v1-compatibility.md) — the
  supported-surface contract and fixture matrix
- `crates/snapshot` (backend seam, seal, no-secret scan, rootfs builder,
  Docker import), `crates/capsulefs`, `crates/snapshot-builder`,
  `crates/guest-agent`
- [`api/snapshot-run-control.md`](api/snapshot-run-control.md) — control-plane
  API contract for the build/run pipeline
- [`ready-state/`](https://github.com/ato-run/ato/tree/main/docs/ready-state)
  — internal working docs (backend matrix, binding leases, UFFD plans,
  benchmarks)

## Design Notes

Snapshot/restore is a separate trait behind `SnapshotBackend`, never grafted
onto the ordinary run path — a cold `ato run` never touches it. The split
keeps the promise symmetrical: the classic path stays simple, and the warm
path can enforce its own stricter invariants (seal-before-bind, runner-class
pinning, no-GPU) without complicating everyday runs.

The hard-won rule of this subsystem is that **anything a user would miss if it
vanished must be a restore-time binding**. Build-time state is shared by
construction; per-user durable state baked into a snapshot is a correctness
bug and a privacy bug at once. Secrets, data volumes, and session identity all
ride the same vsock binding lease instead.
