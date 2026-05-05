# Sandbox

## Overview

Sandbox は「何を許可するか」と「どう隔離するか」を分けるための実行境界である。
ホスト側が policy を決め、nacelle が OS ネイティブの隔離を適用する。

## How it works

役割分担は明確に分かれる。

- `ato-cli` / `ato-desktop`: 検証、権限チェック、policy decision
- `nacelle`: Landlock / Seatbelt などの sandbox 適用、起動、監視

実行時には env を default-deny で組み直し、許可された IPC / network 経路だけを残す。

## Specification

- host runtimes MUST NOT inherit host environment variables implicitly.
- execution MUST allow only explicitly approved env, filesystem, and network surfaces.
- nacelle MUST act as sandbox enforcer, not as the policy decision layer.
- guest-visible IPC paths MUST be allowed explicitly by the sandbox profile.

根拠:

- [`rfcs/accepted/SECURITY_AND_ISOLATION_MODEL.md`](rfcs/accepted/SECURITY_AND_ISOLATION_MODEL.md)
- [`rfcs/accepted/NACELLE_SPEC.md`](rfcs/accepted/NACELLE_SPEC.md)
- [`rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md`](rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md)

## Design Notes

Sandbox を engine 側に閉じ込めるのは、Smart Build / Dumb Runtime を守るため。
ホストは「境界を決める」、engine は「境界を適用する」。この分離が壊れると、
安全性と再構成性の両方が崩れる。
