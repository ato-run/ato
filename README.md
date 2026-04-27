# ato

[![Rust CI](https://github.com/ato-run/ato/actions/workflows/rust-ci.yml/badge.svg?branch=main)](https://github.com/ato-run/ato/actions/workflows/rust-ci.yml)

`ato` runs any project instantly without setup. Point it at a Python script,
a Node app, a Rust binary, or a GitHub repo — `ato` resolves the runtime,
bootstraps only what's needed, and runs inside a sandboxed environment.

This repository is the monorepo for the [Capsule Protocol](https://ato.run)
meta-runtime: the `ato` CLI, the GPUI desktop shell, the shared
`capsule-core` library, the `nacelle` source-runtime sandbox, and the
Tailscale tsnet sidecar.

```bash
curl -fsSL https://ato.run/install.sh | sh

ato run .                             # local project
ato run github.com/owner/repo         # any GitHub repo
ato run https://ato.run/s/demo@r1     # share URL
```

## Workspace layout

```
ato/
├── Cargo.toml                           # workspace root
├── crates/
│   ├── capsule-wire/                    # IPC surface (DAG root, no internal deps)
│   │   ├── ccp/                         #   CCP envelope schema + tolerance
│   │   ├── handle.rs                    #   URL/handle classifier
│   │   ├── config.rs                    #   ConfigField / ConfigKind
│   │   └── error.rs                     #   slim WireError
│   ├── capsule-core/                    # runtime / orchestration library
│   ├── ato-cli/                         # the meta-runtime CLI
│   │   └── lock-draft-engine/           #   lock generation, exposed as WASM
│   ├── ato-desktop/                     # GPUI-based desktop bundle
│   │   └── xtask/                       #   bundle build / packaging
│   └── nacelle/                         # source-runtime sandbox (Seatbelt / bubblewrap / Landlock)
├── sidecars/
│   └── ato-tsnetd/                      # Go: Tailscale tsnet + gRPC + SOCKS5
├── docs/
│   ├── rfcs/                            # accepted / draft architectural RFCs
│   ├── core-architecture.md
│   ├── GLOSSARY.md
│   └── …
├── tests/manual/                        # human-driven release verification
└── .github/workflows/                   # workspace CI (incl. dep-direction lint)
```

## Components

| Crate / module | Role |
|---|---|
| **`ato-cli`** | Meta-runtime entry point. Detects project type, resolves runtime, builds sandbox, runs workload. |
| **`ato-desktop`** | GPUI shell hosting capsules in embedded Wry WebViews. Spawns `ato-cli` as a child for guest sessions. |
| **`capsule-core`** | Runtime / orchestration library shared by CLI and Desktop. |
| **`capsule-wire`** | Pure wire-shape definitions (CCP envelope, handles, config, errors). DAG root with no internal deps. |
| **`nacelle`** | Source-runtime sandbox using OS isolation. Spawned as a child of `ato-cli`. |
| **`ato-tsnetd`** (Go sidecar) | Embedded Tailscale tsnet daemon for capsule egress filtering. Spawned at runtime when `ATO_TSNET_*` env vars are set. Auto-attach pending v0.5.1. |

**Process hierarchy invariant:** `ato-desktop` is always the parent process
and spawns `ato-cli` as a child. Never the reverse.

**Dependency DAG (enforced by `.github/workflows/dep-direction.yml`):**
`ato-desktop` → `capsule-core` → `capsule-wire`, and
`ato-cli` → `capsule-core` → `capsule-wire`.
`nacelle` is a runtime sibling, not a build-time dep.

## Install

```bash
# Recommended — installs CLI + Desktop + nacelle. Uses curl + unzip and
# never produces a quarantined .dmg, so macOS Gatekeeper does not
# interrupt first launch.
curl -fsSL https://ato.run/install.sh | sh

# Homebrew (CLI only). The Cask is gone — use install.sh for the Desktop.
brew install ato-run/ato/ato-cli

# Windows .zip / .msi — extract / install from the latest release.
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

# Run the Desktop shell (builds GPUI; needs platform native toolchain)
cargo run -p ato-desktop

# Per-crate test
cargo test -p ato-cli
cargo test -p capsule-core
```

### Bundle the desktop app

```bash
cargo xtask bundle darwin-arm64    # macOS:   dist/darwin-arm64/Ato Desktop.app
cargo xtask bundle windows-x86_64  # Windows: .msi via WiX
cargo xtask bundle linux-x86_64    # Linux:   .AppImage
```

### Manual test suite

```bash
# Wipes prior results, runs all 15 release-verification suites.
# Set ATO_TEST_KEEP_RESULTS=1 to preserve artifacts across runs.
tests/manual/run-all.sh
```

## Documentation

- [Capsule Protocol RFCs](docs/rfcs/) — `accepted/` is normative, `draft/` is in flight
- [Core architecture](docs/core-architecture.md)
- [Glossary](docs/GLOSSARY.md)
- [Known limitations](crates/ato-cli/docs/known-limitations.md)
- [Agent guidelines](AGENTS.md)
- [Monorepo consolidation history](docs/monorepo-consolidation-plan.md)

## License

Apache-2.0. See [crates/ato-cli/LICENSE](crates/ato-cli/LICENSE).
