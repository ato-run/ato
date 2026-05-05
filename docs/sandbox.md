# Sandbox

## Overview

The sandbox is the execution boundary that separates **what is allowed** from
**how isolation is applied**. The host decides policy, and nacelle applies the
engine path and OS-native isolation. In the current implementation, the CLI,
capsule-core, and nacelle each own a different part of that boundary.

## How it works

The responsibility split is explicit:

- `ato-cli` / `ato-desktop`: verification, permission checks, policy decisions
- `capsule-core`: isolation context shaping and nacelle discovery
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
- nacelle discovery prefers explicit path, then `NACELLE_PATH`, then manifest /
  compat engine settings, then registered default engine, then portable mode
  next to the binary; PATH lookup is intentionally disabled

## Specification

- host runtimes MUST NOT inherit the raw host environment implicitly; Ato
  rebuilds an isolated host context first.
- filesystem access beyond the default runtime view MUST be granted explicitly
  through sandbox grants.
- sandbox grants that traverse symlinks MUST be rejected.
- execution MUST allow only explicitly approved env, filesystem, and network surfaces.
- nacelle MUST act as sandbox enforcer, not as the policy decision layer.
- engine resolution MUST NOT fall back to PATH search.

References:

- [`rfcs/accepted/SECURITY_AND_ISOLATION_MODEL.md`](rfcs/accepted/SECURITY_AND_ISOLATION_MODEL.md)
- [`rfcs/accepted/NACELLE_SPEC.md`](rfcs/accepted/NACELLE_SPEC.md)
- [`rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md`](rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md)

## Design Notes

Sandboxing stays in the engine to preserve Smart Build / Dumb Runtime. The host
decides the boundary; the engine applies the boundary. If that split collapses,
both safety and regenerability degrade. The current code is also more pragmatic
than a pure “zero env” model: environment handling is a reconstructed isolated
baseline with a small explicit passthrough set, not raw inheritance and not a
totally empty process environment.
