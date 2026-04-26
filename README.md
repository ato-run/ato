# ato

Monorepo for the [ato](https://ato.run) project — the Capsule Protocol meta-runtime.

This repository consolidates the historical `ato-run/ato-cli` and `ato-run/ato-desktop` projects into a single Cargo workspace. See [docs/monorepo-consolidation-plan.md](docs/monorepo-consolidation-plan.md) for the migration design.

## Status

Migration in progress (M1: subtree merge). Build is not expected to succeed until M2 lands.

## Crates

- `crates/ato-cli` — the meta-runtime CLI
- `crates/ato-desktop` — the GPUI-based desktop bundle (parent process; spawns `ato-cli` as child)
- `crates/capsule-core` — *(to be extracted in M4)* shared CCP / manifest / error types
