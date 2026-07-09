# Ready-State backend matrix

How each host/provider maps to an isolation substrate and a Ready-State
mechanism. The Desktop Runner rows (M0) are produced by
`crates/cli/src/application/desktop_runner/`; the Firecracker/KVM rows are the
existing snapshot path and are listed here only for contrast — the Desktop
Runner module never fabricates them.

Legend — `ready_state_kind`: `cold_oci` (cold container start, no warm restore),
`criu_checkpoint` (in-VM CRIU restore, future), `vm_snapshot` (full VM memory
snapshot). `isolation_boundary`: `vm_wrapped_container` (OCI container in a
per-session lightweight VM), `micro_vm` (bare Firecracker microVM).

## Desktop Runner — macOS

| Host | Substrate | Isolation | `substrate_scope` | M0 `ready_state_kind` | Future | Accelerator | Cross-arch |
|---|---|---|---|---|---|---|---|
| macOS aarch64 + Apple Containerization | `apple_containerization` | `vm_wrapped_container` | `per_session_vm` | `cold_oci` | `criu_checkpoint` | `apple_vz` | explicit/debug only, **never default** |
| macOS aarch64/x86_64 + Podman (`ato-podman` running) | `podman` | `vm_wrapped_container` | `shared_machine` | `cold_oci` | — | `none` | explicit/debug only, **never default** |
| macOS aarch64, **no** `container` / macOS < 26, **no** Podman | — (unavailable) | — | — | — | — | — | recommend managed runner |
| macOS x86_64 (Intel), no Podman | — (Apple Containerization unsupported) | — | — | — | — | — | recommend managed runner |

Notes:

- The macOS aarch64 + Apple Containerization backend is advertised **only** when
  the host is Apple silicon **and** macOS 26+ **and** `container` is installed.
- The macOS + Podman backend is advertised when the `podman` binary is resolved
  **and** the `ato-podman` machine is running. It is available on both Apple
  silicon and Intel Macs, and on macOS < 26 — broadening host coverage beyond
  Apple Containerization. The probe is read-only and never starts the machine.
- When both substrates are available, Apple Containerization is preferred
  (lighter-weight, per-session VM); Podman is the fallback.
- **Blocker rendering policy:** if any substrate is available, the unavailable
  substrates' blockers do NOT appear in the placement failure path (a backend IS
  available, so placement succeeds with no blockers). They may appear in
  `ato doctor desktop-runner` substrate details.
- VM snapshot / Firecracker are **not supported locally** on macOS.
- A macOS aarch64 host never restores a `linux`/`x86_64`/`firecracker`
  Ready-State artifact, and never silently uses QEMU TCG or Rosetta for
  Ready-State (exact `RunnerClass` match required).

## Desktop Runner — Linux / Windows

| Host | Substrate | Isolation | `ready_state_kind` | Recommendation |
|---|---|---|---|---|
| Linux x86_64 / aarch64 | — (Desktop Runner provides none) | — | — | local Ready-State uses the **separate** Firecracker/KVM runner path |
| Windows | `wsl2` (placeholder, **not implemented**) | — | — | managed runner |

The Desktop Runner module deliberately does **not** advertise the Linux
Firecracker/KVM capability — that path is owned by the snapshot crate and stays
separate.

## Firecracker / KVM (existing snapshot path, for contrast)

| Host | Substrate | Isolation | `ready_state_kind` | Class facets |
|---|---|---|---|---|
| Linux x86_64 + `/dev/kvm` + Firecracker | firecracker | `micro_vm` | `vm_snapshot` | `RunnerClassFacts` (vmm/cpu_template/guest_kernel/rootfs) |

This is the domain of `crates/snapshot` and the
`RunnerClassFacts`/`RunnerClassId` restore-compatibility contract
(`capsule::foundation::install_lifecycle::runner_class`). The Desktop Runner's
[`matching`](../../crates/cli/src/application/desktop_runner/matching.rs) reuses
that exact-class contract for its restore gate.

## Selection policy (fail-closed)

1. Ready-State restore requires an **exact** `RunnerClass` match.
2. A macOS aarch64 host must not restore `linux`/`x86_64`/`firecracker`
   artifacts, nor use QEMU TCG / Rosetta for Ready-State.
3. No compatible local artifact:
   - Ready-State **enabled** → clear error / **explicit** managed-runner suggestion.
   - Ready-State **disabled** → local cold OCI may be offered.
4. Managed Cloud handoff is always **explicit** in logs/receipt.

See [`desktop-runner.md`](./desktop-runner.md) for the provider model and
security invariants.
