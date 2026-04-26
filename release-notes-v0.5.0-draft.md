# Release Notes — ato v0.5.0

> **Status:** Draft — not yet published  
> **Target:** v0.5.0 (promoting from 0.4.x series)

---

## What is ato?

`ato` runs any project instantly without setup. Point it at a Python script, a Node app, a Rust binary, or a GitHub repo — it resolves the runtime, bootstraps only what's needed, and runs inside a sandboxed environment. No Dockerfile, no setup guide, no manual environment.

```bash
curl -fsSL https://ato.run/install.sh | sh

ato run .                             # local project
ato run github.com/owner/repo         # any GitHub repo
ato run https://ato.run/s/demo@r1     # share URL
```

---

## Highlights

### `ato encap` / `ato decap` — share any project with one URL

Capture your current workspace as a portable share descriptor, upload it, and get a URL anyone can restore:

```bash
ato encap                             # → https://ato.run/s/my-project@r1
ato decap https://ato.run/s/my-project@r1 --into ./copy
ato run ./copy
```

Visibility flags: `--internal`, `--private`, `--local`.

Secrets are never uploaded. `ato encap` records *contracts* (which env vars are required) but not their values.

### `ato secrets` — encrypted secret store

Manage per-capsule secrets with masked input and a `--dry-run` scanner that flags accidentally hardcoded values:

```bash
ato secrets set OPENAI_API_KEY
ato secrets list
ato run . --dry-run     # scan for secrets before shipping
```

Secret files are written with `chmod 600`. CI environments are detected; masked input falls back gracefully.

### Sandbox-by-default (Tier 2 runtimes)

Python and native source runtimes route through [Nacelle](https://github.com/ato-run/nacelle) for OS-level isolation:

- `network.enabled = false` (deny-all) is fully enforced via `sandbox-exec` (macOS) / `bwrap` (Linux)
- `--prompt-env` collects required env variables interactively, with save-and-reuse lifecycle
- `ato run` is quiet by default; use `--verbose` or `ATO_LOG=debug` for diagnostic output

### Multi-entry selection and env file injection

```bash
ato run <url> --entry dev           # select which entry to run
ato run <url> --env-file .env       # inject env file
ato run <url> --prompt-env          # interactive env collection
```

### Desktop bundle distribution (Plan B, $0 cost)

`ato-desktop` ships as platform-native installers in v0.5 — no Apple
Developer Program or Windows EV certificate required for the initial
release. The same bundled `ato` CLI is exposed as a PATH binary across
all install paths, so versions can never drift between the GUI and
the headless tool.

| Platform | Default route | Mechanism |
|----------|---------------|-----------|
| macOS | `brew install --cask ato` | Homebrew Cask, ad-hoc signed bundle (L11), auto_updates true |
| macOS | `curl ato.run/install.sh \| sh` | downloads .dmg, stages to `~/Applications`, strips quarantine |
| Windows | direct download or `iwr ato.run/install-win.ps1 \| iex` | unsigned MSI (L10), SmartScreen "More info" → "Run anyway" |
| Linux | direct download or install.sh | `.AppImage` per-arch, drops in `~/Applications` |
| Headless / SSH / CI | install.sh auto-detects | falls back to CLI-only; pass `--with-desktop` to override |

CLI-only is always available as `--cli-only` (`-CliOnly` on Windows)
and is selected automatically when no display is detected.

### Homebrew tap

```bash
brew tap ato-run/ato

# CLI only (Plan A — same as before)
brew install ato

# Desktop + bundled CLI (new in v0.5, Plan B)
brew install --cask ato
```

### Host runtime isolation (security hardening)

PATH poisoning, shim injection, login-shell traps, and wrong-runtime leakage are now covered by a multi-platform E2E host-isolation CI suite (`e2e-host-isolation.yml`) running across Linux, macOS, and Windows.

---

## Runtime support matrix

| Runtime | Status | Notes |
|---------|--------|-------|
| `source/python` | ✅ | uv-backed, `pyproject.toml`, `uv.lock`, single-file PEP 723 |
| `source/node` | ✅ | pnpm / npm / yarn; Deno-compat fallback for bare `.js` |
| `source/deno` | ✅ | deno.json tasks |
| `wasm/wasmtime` | ✅ | `.wasm` binary execution |
| `oci/runc` | ✅ | requires Docker on host |
| `source/native` (Rust, Go) | ✅ | cargo / go toolchain |
| Shell scripts | ✅ | single-file `.sh` inference |

---

## Known limitations

See [docs/known-limitations.md](docs/known-limitations.md) for the full list. Key gaps in v0.5:

| ID | Area | Gap | Target fix |
|----|------|-----|------------|
| L1 | Network | `egress_allow` is advisory on source runtimes | v0.5.1 |
| L2 | Env | `required_env` warns but does not abort | v0.5.1 |
| L3 | Sandbox | `--sandbox` not yet supported for `source/python` | v0.6 |
| L4 | Lock | Lock file auto-generation policy not finalized | v0.5.1 |
| L5 | Services | Multi-service orchestration is experimental | v0.6 |
| L6 | Linux | `ato://` URL handler requires manual `xdg-mime` step | v0.5.1 |
| L7 | Cache | `~/.ato/cache/synthetic/` never GC'd | v0.5.1 |
| L8 | Desktop | Windows / Linux Desktop are beta-quality | v0.6 |
| L9 | Protocol | CCP is wire-shape-fixed at v1; no streaming | v0.6+ |
| L10 | Windows | MSI is unsigned in v0.5 (SmartScreen warning) | v0.5.x |
| L11 | macOS | Desktop bundle is ad-hoc signed (not Developer ID) | v0.6 |

---

## Install

```bash
# Shell installer (auto-detects display; installs Desktop+CLI on graphical
# sessions, CLI-only on headless/SSH; pass --cli-only or --with-desktop
# to override)
curl -fsSL https://ato.run/install.sh | sh

# CLI only — explicit
curl -fsSL https://ato.run/install.sh | sh -s -- --cli-only

# Homebrew (CLI only)
brew tap ato-run/ato && brew install ato

# Homebrew (Desktop + bundled CLI, ad-hoc signed)
brew install --cask ato

# Windows (PowerShell — Desktop MSI + CLI by default)
iwr https://ato.run/install-win.ps1 | iex

# Windows CLI only
powershell -Command "& { iwr https://ato.run/install-win.ps1 -OutFile install.ps1; .\install.ps1 -CliOnly }"

# From source
cargo build -p ato-cli --release
```

---

## Foundation readiness — 0 / 6

The Capsule Protocol defines open-governance transfer criteria (§11.2). v0.5 is the
foundation-building release; transfer is not a v0.5 milestone.

| KPI | Target | v0.5 status |
|-----|--------|-------------|
| External conforming runtime | ≥1 | 0 / 1 |
| Conformance suite pass rate | ≥70% | skeleton (0%) |
| External maintainers | ≥3 | 0 / 3 |
| TSC non-ato majority | required | 0 / required |
| Publishers | ≥100 | 0 / 100 |
| Adversarial security reports | ≥5 | 0 / 5 |

---

## Full changelog

See [CHANGELOG.md](CHANGELOG.md) for the complete commit-level history from v0.4.22 through v0.4.73.

Key milestones since v0.4.22:

- `ato encap` / `ato decap` share workflow with interactive primary entry selection
- `ato secrets` subcommand with SecretStore, masked input, `--dry-run` scanner
- App control surface (`bootstrap`, `status`, `repair` flows)
- Synthetic npm/PyPI package execution via `ato run npm:mintlify`
- Homebrew tap distribution
- E2E host-isolation suite (PATH poisoning, shim injection, wrong-runtime — Linux/macOS/Windows)
- Lock policy: one `ato.lock.json` per project, content-addressed cache
- `ato run` quiet-by-default with `--verbose` / `ATO_LOG` escape hatch
- Secrets-safe env handling: `chmod 600`, CI detection, injection denylist
- Background process support: `ato run . --background`, `ato ps`, `ato logs`, `ato stop`
- Readiness probes for multi-service supervisor mode
- RUSTSEC-2026-0098 dependency patch (rustls-webpki 0.103.12)

---

*Draft prepared: 2026-04-23*
