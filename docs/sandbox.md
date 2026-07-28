# Sandbox

## Overview

The sandbox is the execution boundary that separates **what is allowed** from
**how isolation is applied**. The host decides policy, and nacelle applies the
engine path and OS-native isolation. In the current implementation, the CLI,
capsule, and nacelle each own a different part of that boundary.

## How it works

The responsibility split is explicit:

- `ato-cli` / `ato-desktop`: verification, permission checks, policy decisions
- `capsule`: isolation context shaping and nacelle discovery
- `nacelle`: sandbox application, launch, and supervision via mechanisms like Landlock and Seatbelt

At runtime:

- host commands rebuild `HOME`, `TMPDIR`, cache, and config directories inside a
  host-isolated namespace
- baseline passthrough env is narrow: `PATH`, locale vars, proxy / CA vars,
  Windows runtime vars, plus `CAPSULE_*`
- additional host path access is granted explicitly through `--read`,
  `--write`, and `--read-write`
- sandbox grant resolution rejects symlink traversal and resolves relative paths
  against the effective caller cwd
- canonical `discover_nacelle()` prefers explicit path, then `NACELLE_PATH`,
  then manifest / compat engine settings, then registered default engine, then
  portable mode next to the binary; this canonical path intentionally disables
  PATH lookup
- legacy / specialized nacelle resolution paths used by share execution and
  bundle packing have additional fallbacks, including PATH lookup

## Specification

- host runtimes MUST NOT inherit the raw host environment implicitly; Ato
  rebuilds an isolated host context first.
- filesystem access beyond the default runtime view MUST be granted explicitly
  through sandbox grants.
- sandbox grants that traverse symlinks MUST be rejected.
- execution MUST allow only explicitly approved env, filesystem, and network surfaces.
- nacelle MUST act as sandbox enforcer, not as the policy decision layer.
- `discover_nacelle()` in `capsule` intentionally disables PATH fallback for security; however `resolve_nacelle_binary()` (executor path) and `find_nacelle_binary()` (bundle path) MAY fall back to PATH search as a last resort.

> **Implementation status (v0.x).** The statements above describe the target
> security model (see the linked RFCs). The current implementation does not yet
> meet all of them on every platform:
>
> - **Filesystem is a strict deny-by-default allowlist on Linux only.** On
>   macOS the source sandbox is `(allow default)` plus a blocklist of ~12
>   sensitive paths (SSH keys, cloud credentials, and similar), so it is
>   defense-in-depth, not a deny-default allowlist.
> - **Network deny-all is enforced; the egress allowlist is not.**
>   `network.enabled = false` (deny-all) is enforced on both platforms, but a
>   hostname/IP `egress_allow` list is advisory only on source runtimes — there
>   is no enforcing SOCKS sidecar yet.
> - **The deny must be declared; it is not the default.** An absent
>   `[network] enabled` means *enabled* — a capsule that says nothing about
>   network gets egress. Only an explicit `[network] enabled = false` denies.
>   Whether the absent case should instead fail closed is an open decision
>   (ato#786): flipping it changes runtime behavior for every already-published
>   capsule that fetches anything. The single source of that default is
>   `capsule::types::NETWORK_ENABLED_WHEN_UNDECLARED`.
> - **`[isolation.network]` is not an authoring surface.** Network policy is
>   authored at the top-level `[network]`; `[isolation.network]` is the internal
>   wire format the CLI synthesizes for nacelle, and authoring it in
>   `capsule.toml` has no effect.
> - **The build / prepare phase is not sandboxed.** Dependency installs and
>   `build` / `prepare` lifecycle commands run as ordinary host processes with
>   the host environment, secrets, and network. Only the run phase is isolated.

References:

- [`rfcs/accepted/SECURITY_AND_ISOLATION_MODEL.md`](rfcs/accepted/SECURITY_AND_ISOLATION_MODEL.md)
- [`rfcs/accepted/NACELLE_SPEC.md`](rfcs/accepted/NACELLE_SPEC.md)
- [`rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md`](rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md)

## Recipe permissions

A recipe can request filesystem access, environment variables, network egress,
services, and host bridge capabilities. These requested surfaces are declared in
`capsule.toml` and become part of the launch graph.

Because requested permissions are part of the launch graph, they are also part of
execution identity. A launch with network egress allowed and a launch without
network egress are not the same execution — they produce different execution IDs
even if source, runtime, and entrypoint are identical.

Treat third-party recipes with the same care as third-party source code. A recipe
controls what the execution is allowed to access.

## Design Notes

Sandboxing stays in the engine to preserve Smart Build / Dumb Runtime. The host
decides the boundary; the engine applies the boundary. If that split collapses,
both safety and regenerability degrade. The current code is also more pragmatic
than a pure “zero env” model: environment handling is a reconstructed isolated
baseline with a small explicit passthrough set, not raw inheritance and not a
totally empty process environment.
