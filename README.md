# ato

[![Rust CI](https://github.com/ato-run/ato/actions/workflows/rust-ci.yml/badge.svg?branch=main)](https://github.com/ato-run/ato/actions/workflows/rust-ci.yml)

`ato` runs any project instantly without setup. Point it at a Python script,
a Node app, a Rust binary, or a GitHub repo — `ato` resolves the runtime,
bootstraps only what's needed, and runs inside a sandboxed environment.

This repository is the **monorepo** for the [Capsule Protocol](https://ato.run)
meta-runtime. It consolidates the historical `ato-run/ato-cli` and
`ato-run/ato-desktop` repos into a single Cargo workspace.

```bash
curl -fsSL https://ato.run/install.sh | sh

ato run .                             # local project
ato run github.com/owner/repo         # any GitHub repo
ato run https://ato.run/s/demo@r1     # share URL
```

## Status

> **Migration in progress.** The workspace builds, but cross-crate
> consolidation (`capsule-core` extraction, M4–M5) and full release-CI
> integration (M6) are still pending. See
> [`docs/monorepo-consolidation-plan.md`](docs/monorepo-consolidation-plan.md)
> for the migration design and milestones.

| Milestone | Status |
|-----------|--------|
| **M1** — subtree merge of `ato-cli` + `ato-desktop` (history preserved) | ✅ landed |
| **M2** — Cargo workspace + structural files | ✅ landed |
| **M3** — vendored deps cleanup (gpui-component, .ato/ leak fix) | ✅ landed |
| **M4** — `capsule-core` extraction Phase 1 (CCP envelope wire shape) | ✅ landed |
| **M5** — `capsule-core` extraction Phase 2 (Manifest + Error + Config wire-shape unification) | ✅ landed |
| **M6** — release CI integration (cargo-dist + xtask + Homebrew Cask) | ✅ landed (RC tag verification pending) |
| **N1** — relocate `capsule-core` to `crates/capsule-core/` (top-level) | ✅ landed |
| **N2** — extract `crates/capsule-wire/` (the IPC surface, DAG root) | ✅ landed |
| **N3** — switch `ato-desktop`'s wire imports onto `capsule-wire` | ✅ landed |
| **N4** — dependency-direction CI lint enforcing the DAG | ✅ landed |
| **P1** — backfill legacy mirror commits into monorepo (4× ato-desktop fixes) | ✅ landed |
| **P2** — subtree merge `nacelle.git` → `crates/nacelle/` (history preserved) | ✅ landed |
| **P3** — extend dep-direction CI lint with R5/R6 (nacelle sibling boundary) | ✅ landed |
| **P6a** — remove dead workflow copies under `crates/nacelle/.github/` | ✅ landed |
| **v0.5.0 bump** — after monorepo CI/CD verified end-to-end | ⬜ pending |
| **P6b** — release-CI consolidation with per-crate tag prefixes (round 2) | ⬜ pending |
| **M7 / P7** — archive `ato-cli` / `shiny-disco` / `nacelle` repos (round 2) | ⬜ pending |

## Workspace layout

```
ato/
├── Cargo.toml                           # workspace root
├── crates/
│   ├── capsule-wire/                    # IPC surface (DAG root, N2)
│   │   ├── ccp/                         #   CCP envelope schema + tolerance
│   │   ├── handle.rs                    #   URL/handle classifier
│   │   ├── config.rs                    #   ConfigField / ConfigKind
│   │   └── error.rs                     #   slim WireError
│   ├── capsule-core/                    # runtime/orchestration library (N1)
│   ├── ato-cli/                         # the meta-runtime CLI
│   │   └── lock-draft-engine/           #   lock generation, exposed as WASM
│   ├── ato-desktop/                     # GPUI-based desktop bundle
│   │   └── xtask/                       #   bundle build / packaging
│   └── nacelle/                         # source-runtime sandbox (Landlock/eBPF), spawned by ato-cli
├── docs/
│   ├── rfcs/                            # accepted / draft architectural RFCs
│   ├── core-architecture.md
│   ├── monorepo-consolidation-plan.md
│   ├── v0.5-readiness-dashboard.md
│   └── v0.5-distribution-plan.md
├── tests/manual/                        # human-driven release verification
└── .github/workflows/                   # workspace CI
```

**Process hierarchy invariant:** `ato-desktop` is always the parent process
and spawns `ato-cli` as a child. Never the reverse. This invariant is
documented in `docs/monorepo-consolidation-plan.md` §5 and will be enforced
by CI lint when M4 lands. See also
[`crates/ato-desktop/docs/`](crates/ato-desktop/docs/) for the orchestrator
contract.

## Install

```bash
# Recommended — installs CLI + Desktop + nacelle. The shell installer
# uses curl + unzip and never produces a quarantined .dmg, so macOS
# Gatekeeper does not interrupt first launch.
curl -fsSL https://ato.run/install.sh | sh

# Homebrew (CLI only). The Cask is gone — use install.sh above for
# the Desktop bundle.
brew install ato-run/ato/ato-cli

# Windows .zip (Desktop) — extract manually for now; an install.ps1
# will land alongside the next release.
# https://github.com/ato-run/ato/releases/latest

# From source (this monorepo)
cargo build -p ato-cli --release
```

### Uninstall

```bash
# install.sh deployments
ato uninstall          # interactive; --keep-data to retain ~/.ato/desktop
# or: curl -fsSL https://raw.githubusercontent.com/ato-run/ato/main/scripts/uninstall.sh | sh

# Homebrew deployments
brew uninstall ato-cli
brew uninstall --cask ato 2>/dev/null || true   # legacy Cask, removed in v0.4.88
```

## Develop

```bash
# Workspace check (all crates)
cargo check --workspace --all-targets

# Run the CLI
cargo run -p ato-cli -- run ./your-project

# Run the Desktop bundle (builds GPUI; needs platform native toolchain)
cargo run -p ato-desktop

# Per-crate test
cargo test -p ato-cli
cargo test -p capsule-core
```

## Documentation

- [Capsule Protocol RFCs](docs/rfcs/) — `accepted/` is normative, `draft/` is in flight
- [Core architecture](docs/core-architecture.md)
- [Glossary](docs/GLOSSARY.md)
- [Release process](docs/ops/RELEASE.md)
- [Agent guidelines](AGENTS.md)
- [Known limitations](crates/ato-cli/docs/known-limitations.md)
- [Monorepo consolidation plan](docs/monorepo-consolidation-plan.md)
- [Capsuled-dev → ato migration plan](docs/capsuled-dev-migration-plan.md)

## License

Apache-2.0. See [crates/ato-cli/LICENSE](crates/ato-cli/LICENSE).
