# Desktop Runner (macOS) — capability model

> Status: **M0 (developer-preview)**. Implements the capability *probe* and
> placement *matching* only. No Ready-State restore, no CRIU, no binding
> injection, no user traffic. Tracked under #838; CRIU is the separate #839 track.

The **Desktop Runner** is the desktop shell acting as a *local* Ato Runner
provider, backed by a host isolation substrate. On macOS that substrate is
[Apple Containerization](https://github.com/apple/containerization) / the Apple
[`container`](https://github.com/apple/container) tool, which runs Linux
containers inside per-session lightweight VMs on Apple silicon.

It is the local analogue of a Connected Runner: instead of advertising flat
capability strings in a heartbeat
(`application/runner_agent.rs::collect_capabilities`), it advertises a
structured `DesktopRunnerFacts` so a placement decision can reason about guest
OS/arch, isolation boundary, and Ready-State maturity.

Code: `crates/cli/src/application/desktop_runner/`
(`facts.rs`, `macos.rs`, `matching.rs`, `mod.rs`).

## Architecture

```text
Desktop Runner
  -> macOS host
  -> Apple Containerization / `container`
  -> one lightweight Linux VM per session
  -> OCI container cold start              (M0)
  -> CRIU checkpoint inside the Linux VM   (future, #839)
  -> BindingLease injection after start    (future)
  -> User
```

**Key distinction.** Apple Containerization / `container` is the *isolation
substrate*. CRIU is a later *inner Ready-State mechanism*. The two are not
conflated: the substrate question ("where does a session run?") is answered in
M0; the Ready-State question ("can we restore a warm session?") is not.

## What M0 does

| Allowed | Not allowed (yet) |
|---|---|
| Probe Apple `container` (read-only) | Ready-State restore via Apple Containerization |
| Report a `DesktopRunnerFacts` capability/receipt | CRIU checkpoint/restore |
| Offer a local cold-OCI start when Ready-State is off | BindingLease injection |
| Suggest a managed runner, explicitly | Snapshot-server execution |
| (Manual smoke) cold-start a tiny OCI image, measure latency | Cross-arch / Rosetta fallback, QEMU TCG default |

The probe is **honest and side-effect free**: every `supports_*` flag starts
`false`, the Apple Containerization backend is advertised only when the host can
actually serve it (Apple silicon **and** macOS 26+ **and** `container`
installed), and the `container` system service is *detected, never started*
(`container system status`, not `container system start`).

When a precondition is missing, the substrate is reported `available: false`
with an actionable diagnostic — but the diagnostic is surfaced **only** when the
user selects local Desktop Runner execution, never during normal Desktop
startup.

## Placement (fail-closed)

`matching.rs` decides what the host may do:

- **Ready-State restore requires an exact `RunnerClass` match.** This reuses
  `RunnerClassFacts::ensure_compatible` — the same contract the snapshot restore
  Prepare gate uses — so a macOS aarch64 host can never be told to restore a
  `linux`/`x86_64`/`firecracker` artifact.
- A macOS aarch64 host must **not** silently use QEMU TCG or Rosetta for
  Ready-State, and must not cold-start a wrong-class artifact.
- If no compatible local path exists:
  - Ready-State **enabled** → a clear error or an **explicit** managed-runner
    suggestion (the user opted into Ready-State; do not silently degrade).
  - Ready-State **disabled** → local cold OCI may be offered.
- Managed Cloud handoff is always **explicit** in the reason string (intended
  for logs/receipt).

In M0 no backend sets `supports_ready_state_restore`, so a Ready-State run on a
Desktop Runner host resolves to an explicit managed-runner suggestion rather
than a restore.

## Security invariants

These hold in M0 by the *absence* of capability and are the contract for the
future path:

- A Desktop Runner is a **Runner**, not a Snapshot Server / Capsule Registry.
  The Snapshot Server / Registry never executes user-bound sessions.
- The Apple Containerization substrate runs per-container Linux VM sessions
  locally; **session isolation is per VM** (VM-wrapped container).
- Bindings are injected **only after restore/start**, never during build/seal.
- Binding values are **never** written to CapsuleFS, ReadyStateManifest, rootfs,
  memory, vmstate, CRIU images, or logs.
- Shared caches may hold **immutable pre-bind artifacts only**.

## Future path (not M0)

```text
Desktop Runner -> Apple Containerization -> Linux lightweight VM -> OCI container
  -> CRIU checkpoint (if kernel/runtime compatible)
  -> restore
  -> BindingLease injection
  -> user traffic
```

CRIU is intentionally deferred to #839: Apple Containerization uses an optimized
kernel and minimal rootfs, and CRIU has kernel-feature dependencies, so the
`criu_checkpoint` backend's compatibility contract is settled on Linux first
(a Linux-gated spike) before it is brought to macOS.

## Manual smoke

```sh
# Requires macOS 26 + Apple silicon + `container` installed and its
# system service running (or opt in to starting it).
ATO_DESKTOP_SMOKE_START_SERVICE=1 \
  cargo test -p cli desktop_runner::smoke -- --ignored --nocapture
```

It cold-starts a tiny HTTP OCI image, waits for health, stops it, and prints a
receipt with `host_os`, `host_arch`, macOS version, `container` version,
`substrate = apple_containerization`, `isolation_boundary =
vm_wrapped_container`, `ready_state_kind = cold_oci`, and elapsed
start→health. The image is overridable with `ATO_DESKTOP_SMOKE_IMAGE`.

See also [`backend-matrix.md`](./backend-matrix.md).
