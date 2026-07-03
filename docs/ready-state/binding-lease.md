# Ready-State BindingLease contract (Phase 8 — design only)

> Status: **Phase 8 contract (design only)**, tracks #863. This note pins — on one
> page — how a restored Ready-State microVM receives its runtime bindings **after**
> restore and **before** user traffic, **without ever putting a secret into the
> snapshot**. There is **no** code here: no `BindingGuardMode::Serve` wiring, no
> `ato run` behavior change, no guest-agent yet. It is the anchor a later **Phase
> 8a** implementation PR cites. The #837 binding-required guard stays **fail-closed**
> until this contract is approved and implemented.

## Why

`ato run` long-lived serving (#845/#850) works for **no-binding** capsules. A capsule
that declares `[secrets.*]` / `[bindings.*]` / `[external.*]` is rejected *before*
restore by the #837 guard (`ensure_no_unwired_runtime_bindings`,
`BindingGuardMode::VerifyOnly` / `Serve`) because there is no mechanism to deliver
bindings to a restored guest — a restored session would otherwise serve without its
credentials. Phase 8 defines that mechanism so binding-required capsules can
eventually run, while the sealed artifact stays **pre-bind and secret-free** (the
hard invariant from #831/#834): the seal is content-addressed and no-secret-scanned,
and that must remain true.

## Hard invariants (non-negotiable)

1. **No secret in the snapshot.** No binding/secret value is ever written into the
   manifest, CapsuleFS, rootfs, memory image, or vmstate. Bindings exist **only** at
   restore time, in a session-scoped lease. The on-disk artifact is identical whether
   or not the capsule will later be bound.
2. **Post-bind state is dirty — no re-capture.** Once bindings are attached, the
   VM/session is considered **dirty**: **no** post-bind memory snapshot, vmstate
   snapshot, checkpoint, or re-seal is allowed. The secret may live only on tmpfs,
   but bound VM memory can also carry it, so capturing any post-bind state would
   re-introduce a secret into a snapshot. A Ready-State seal is **always** produced
   from a pre-bind boot/snapshot, never from a bound running session.

## Phase 8a delivery — guest-agent over vsock (recommended)

The control plane is a **guest-agent reached over vsock**. The host never writes
directly into guest tmpfs; it hands the agent a session-scoped lease over vsock, and
the agent materializes it **inside** the guest:

```
host (ato run)  ──vsock──▶  guest-agent  ──writes──▶  /run/ato/bindings/<name>  (tmpfs, 0600)
                                         └─(optional)─▶  local metadata endpoint (vsock/link-local)
```

- **vsock guest-agent** = the control plane (lease delivery, bound-ready signal,
  revoke/scrub).
- **tmpfs secret files** at `/run/ato/bindings/<binding-name>` (mode `0600`, tmpfs so
  nothing touches a persistent/overlay disk) = the primary delivery form.
- An **agent-provided metadata endpoint** inside the guest is an *optional* auxiliary
  channel the same agent exposes — not a separate, independently-trusted mechanism.

This is deliberately **not** a three-way "vsock vs tmpfs vs metadata" choice: the
agent owns all three. The host→agent channel is the trust boundary; tmpfs file +
metadata endpoint are how the agent presents the binding to the workload.

## `env` binding semantics — env-*like*, not env rewrite

An already-snapshot process's Unix environment **cannot** be retroactively changed: a
warm snapshot froze the workload's `environ` at seal/boot, and there is no supported
way to inject env vars into a running, snapshotted PID. Therefore:

- A Phase 8a **"env binding"** is an **env-like logical binding** the workload reads
  from its binding file (`/run/ato/bindings/<name>`) or the agent metadata endpoint
  **at request time** — *not* a host-side env rewrite.
- An app that genuinely requires a real **process env** var uses **supervisor mode**
  (v1.2): the guest-agent itself owns the workload process and starts (or restarts) it
  *after* bindings are attached, with the env populated then. This is the contract's
  named successor to the impossible environ-rewrite.

### Supervisor mode (v1.2)

When a capsule declares `delivery = "env"` secrets, the builder writes
`/etc/ato/supervisor.json` into the rootfs and the guest init runs the guest-agent
**as the supervisor** instead of launching the app directly. The agent then owns the
workload lifecycle:

- `supervisor.json` holds the workload `cmd` / `cwd`, a static `base_env`, and a
  `bindings_env` map (`ENV_VAR → binding name`). It holds **no secret value** — only
  the name of the binding whose tmpfs file supplies the value.
- **Start:** once the session is **bound-ready**, the agent composes the environment
  (`base_env` + each `bindings_env` value read from `/run/ato/bindings/<name>`) and
  spawns the workload with it. A missing binding fails closed (the workload never
  starts half-bound); a spawn failure is reported to the host so it never believes the
  session is serving.
- **Build (`StopWorkload`):** the build boots with a **placeholder** binding, verifies
  health, then the host sends `StopWorkload`; the agent stops the workload and the
  session scrubs the tmpfs, so the pre-bind snapshot is captured **workload-idle and
  secret-free** (contract §7.2 placeholder-readiness).
- **Restore:** the real bindings are delivered → bound-ready → the agent starts a
  **fresh** workload with the real env → health → expose. The value lives only on
  tmpfs and in the running process's environment, never in the snapshot.

## Bound-ready gate (before user traffic)

No user traffic is exposed until **bound-ready**. The agent signals (over vsock) that:

1. every declared binding for the session has been delivered to tmpfs / the endpoint, and
2. the workload has consumed them (readiness probe passes *after* bindings are present).

Until bound-ready, the session is `restored` but not `serving`; the run gate must not
register a servable session or expose the port. (For no-binding capsules this gate is
trivially satisfied at restore, preserving #850 behavior.)

## Lease lifecycle

| event | behavior |
|---|---|
| **issue** | session-scoped lease created at restore, after the #837 guard passes; delivered to the guest-agent over vsock. |
| **TTL** | each lease has a TTL; the host renews while the session is healthy. |
| **renew** | host pushes a renewed lease over vsock before expiry; agent atomically replaces the tmpfs file. |
| **expiry (no renew)** | agent scrubs the binding (tmpfs wipe / endpoint 403); the bound-ready gate drops → traffic stops. |
| **revoke** | host can revoke a lease at any time; agent scrubs immediately. |
| **`ato stop`** | host requests the agent **revoke the lease + scrub the tmpfs bindings**, then tears the VM/tap/overlay down (the #845/#850 cross-process teardown). **Never re-seals.** |

## No-secret-in-snapshot proof (Phase 8a acceptance)

A Phase 8a implementation must **demonstrate**, not just assert, the invariants:

- after a **bound** run + `ato stop`, the on-disk Ready-State artifact (CAS / manifest
  / overlay) still **scans clean** (no-secret scanner over all layers) — the lease
  never leaked back into CAS, the overlay, or a re-seal;
- a guest-side check that `/run/ato/bindings/*` is tmpfs (not a persistent mount) and
  is empty after stop/scrub;
- a test that any attempt to seal/snapshot a **bound** session is refused (the
  "post-bind state is dirty" invariant is enforced, not just documented).

## Scope (Phase 8a minimal)

**In scope:** **file/env-like secret bindings only** — delivered as tmpfs files via the
vsock guest-agent, read by the workload as logical env/file bindings.

**Out of scope (later phases):** OAuth, user files / drive, LLM bindings, runner /
context bindings, real process-env injection (supervisor/restart mode), Desktop
Runner, CRIU. The #837 guard continues to reject these fail-closed.

## Non-goals (this contract)

No implementation PR, no code, no `BindingGuardMode::Serve` wiring, no `ato run`
behavior change, no guest-agent build, no KVM. This document is the design anchor;
Phase 8a is a separate PR that cites it.

## Acceptance (for #863)

This note, reviewed and approved, **is** the deliverable: a single contract a Phase 8a
implementation PR can cite. Until then, binding-required capsules remain fail-closed
by #837.

See also [`uffd-mem-backend.md`](./uffd-mem-backend.md),
[`desktop-runner.md`](./desktop-runner.md), and
[`criu-container-spike.md`](./criu-container-spike.md) for sibling Ready-State design
notes.
