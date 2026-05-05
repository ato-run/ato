# Sandbox

## Overview

The sandbox is the execution boundary that separates **what is allowed** from
**how isolation is applied**. The host decides policy, and nacelle applies the
OS-native isolation.

## How it works

The responsibility split is explicit:

- `ato-cli` / `ato-desktop`: verification, permission checks, policy decisions
- `nacelle`: sandbox application, launch, and supervision via mechanisms like Landlock and Seatbelt

At runtime, the environment is rebuilt with default-deny rules, and only
approved IPC and network paths remain available.

## Specification

- host runtimes MUST NOT inherit host environment variables implicitly.
- execution MUST allow only explicitly approved env, filesystem, and network surfaces.
- nacelle MUST act as sandbox enforcer, not as the policy decision layer.
- guest-visible IPC paths MUST be allowed explicitly by the sandbox profile.

References:

- [`rfcs/accepted/SECURITY_AND_ISOLATION_MODEL.md`](rfcs/accepted/SECURITY_AND_ISOLATION_MODEL.md)
- [`rfcs/accepted/NACELLE_SPEC.md`](rfcs/accepted/NACELLE_SPEC.md)
- [`rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md`](rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md)

## Design Notes

Sandboxing stays in the engine to preserve Smart Build / Dumb Runtime. The host
decides the boundary; the engine applies the boundary. If that split collapses,
both safety and regenerability degrade.
