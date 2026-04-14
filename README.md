# Ato

[![CI](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=dev)](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/releases)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org/)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-6f42c1)
[![Issues](https://img.shields.io/github/issues/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/issues)
[![Last Commit](https://img.shields.io/github/last-commit/ato-run/ato-cli/dev)](https://github.com/ato-run/ato-cli/commits/dev)

English | [日本語](README_JA.md)

> Capture any multi-repo workspace as a capsule. Share a URL. Anyone rebuilds the exact same environment in one command.

Ato introduces the **capsule** — a portable, self-describing workspace descriptor that records your git sources, toolchains, install steps, run entries, and env-file contracts without capturing any secrets. `ato encap` creates one from your local workspace. `ato decap` materializes it anywhere.

---

## 30-second demo

```bash
# On your machine: capture the workspace and get a shareable URL
ato encap . --share
# → https://ato.run/s/myproject@r1

# On any other machine: rebuild the full workspace from that URL
ato decap https://ato.run/s/myproject@r1 --into ./myproject
```

That is the entire workflow. No Dockerfile. No manual README steps. No "works on my machine."

---

## What is a capsule?

A capsule is a pair of JSON files written to `.ato/share/` inside your workspace root:

| File | Purpose |
|------|---------|
| `share.spec.json` | Human-readable declaration: sources, tools, install steps, run entries, env file contracts |
| `share.lock.json` | Resolved snapshot: exact commit SHAs, detected tool versions, computed digests |
| `guide.md` | Auto-generated quick-reference for recipients |

`ato encap` writes both files. `ato decap` reads the spec (or lock) and materializes the full environment. The lock is what makes reconstruction byte-for-byte reproducible.

### What a capsule records

| Layer | Example |
|-------|---------|
| **Sources** | `git@github.com:org/api.git` at commit `abc123` |
| **Tools** | `node`, `python`, `uv`, `bun` — versions detected from lockfiles |
| **Install steps** | `npm ci` in `frontend/`, `uv sync` in `server/` |
| **Run entries** | `npm run dev`, `uv run bot.py`, `bun run dev` |
| **Env contracts** | paths to `.env.example` files; no secret values are ever stored |

### What a capsule does NOT record

- Secret values (`.env` files, tokens, passwords)
- Build artifacts or binaries
- The contents of `node_modules`, `.venv`, or any generated directory

---

## `ato encap` — capture a workspace

```bash
# Preview detected items without writing anything
ato encap . --print-plan

# Capture locally (writes .ato/share/)
ato encap .

# Capture and upload to get a shareable URL
ato encap . --share

# CI / non-interactive: accept all detected items automatically
ato encap . --yes --share

# Save detected settings into capsule.toml [share] for future runs
ato encap . --yes --save-config
```

### Encap flags

| Flag | Description |
|------|-------------|
| `--yes` / `-y` | Accept all detected items without prompting. Required in CI. |
| `--share` | Upload the capsule after writing local files and print the share URL. |
| `--save-only` | Write local files only; do not upload even if authenticated. |
| `--print-plan` | Print the detected spec as JSON and exit; writes nothing. |
| `--git-mode` | `same-commit` (default) — lock to the current HEAD SHA. `latest-at-encap` — record the branch and resolve HEAD at encap time. |
| `--tool-runtime` | `auto` (default) — ato manages toolchains. `system` — use whatever is on `PATH`. |
| `--allow-dirty` | Allow encap when a repo has uncommitted changes (warns by default). |
| `--save-config` | Write `git_mode`, `tool_runtime`, and any excludes into `capsule.toml [share]`. |

### Interactive summary screen

When a TTY is present and `--yes` is not set, `ato encap` shows a single summary screen:

```
Detected workspace `myproject`:

  sources   api  frontend  dashboard
  tools     node  python  uv  bun
  install   frontend-install  server-install  dashboard-install
  entries   frontend-dev  server-bot  dashboard-dev
  env files 2  (frontend/.env.example  server/.env.example)

Accept all? [Enter]  or  skip <ids>:
```

Press **Enter** to accept everything, or type `skip api server-bot` to exclude specific items by ID.

### Reachability check

`ato encap` checks that every git source's current HEAD commit is reachable on the remote before locking it. If a commit has not been pushed, encap stops with an actionable message:

```
error: source `api` has unpushed commits (HEAD abc1234 not found on remote)
hint: push first, or use --git-mode latest-at-encap to record the branch instead
```

### `capsule.toml [share]` — project-level config

Run `ato encap --save-config` once to persist settings for your workspace. After that, plain `ato encap` picks them up automatically:

```toml
# capsule.toml
[share]
git_mode     = "same-commit"   # or "latest-at-encap"
tool_runtime = "auto"          # or "system"
yes          = false           # set true to always skip the summary screen

[share.exclude]
sources       = []             # IDs to always skip
tools         = []
install_steps = []
entries       = []
```

CLI flags always override `capsule.toml`.

---

## `ato decap` — materialize a workspace

```bash
# From a share URL
ato decap https://ato.run/s/myproject@r1 --into ./myproject

# From a local spec or lock file
ato decap .ato/share/share.spec.json --into ./myproject

# Preview the materialization plan without executing
ato decap https://ato.run/s/myproject@r1 --into ./myproject --plan

# Treat any verification issue as fatal
ato decap https://ato.run/s/myproject@r1 --into ./myproject --strict
```

`ato decap` works in four phases:

1. **Resolve** — fetch and validate the spec/lock (digest check)
2. **Checkout** — `git clone` + `git checkout <sha>` for each source
3. **Verify** — check tool availability, env file presence
4. **Install** — run each install step (e.g. `npm ci`, `uv sync`)

Verification issues are reported as a summary after all steps complete. Use `--strict` to exit with code 1 if any issue is found.

### Decap flags

| Flag | Description |
|------|-------------|
| `--into <dir>` | Target directory for materialization. Must be empty or not yet exist. |
| `--plan` | Print the materialization plan as JSON and exit. |
| `--tool-runtime` | `auto` (default) or `system`. Overrides the value recorded in the spec. |
| `--strict` | Exit with code 1 if any verification issue is found. |

### After `ato decap`

The materialized directory contains:

```
myproject/
  api/                  ← git checkout at locked SHA
  frontend/             ← git checkout at locked SHA
  dashboard/            ← git checkout at locked SHA
  .ato/share/
    share.spec.json
    share.lock.json
    guide.md            ← quick-reference for the workspace
```

Run entries are printed at the end of `ato decap`. To launch one:

```bash
ato run https://ato.run/s/myproject@r1 --entry frontend-dev
```

---

## `ato run` with share URLs

`ato run` accepts any share URL and executes the primary entry directly, without a full `decap`:

```bash
ato run https://ato.run/s/myproject@r1
ato run https://ato.run/s/myproject@r1 --entry dashboard-dev
ato run https://ato.run/s/myproject@r1 --env-file ./local.env
ato run https://ato.run/s/myproject@r1 --prompt-env
```

---

## Install

```bash
curl -fsSL https://ato.run/install.sh | sh
```

Or download a pre-built binary from the [GitHub Releases page](https://github.com/ato-run/ato-cli/releases/latest) and place it on your `PATH`.

### Build from source

```bash
cargo build -p ato-cli

# Install the Nacelle sandbox engine (required for Python / native targets)
./target/debug/ato config engine install --engine nacelle
```

---

## Process management

```bash
ato ps
ato logs --id <id> [--follow]
ato stop --id <id>
```

---

## Security

Ato is fail-closed by default.

- **No secrets in capsules** — env file paths are recorded; values are never captured.
- **Reachability enforcement** — `ato encap` refuses to lock a commit that has not been pushed.
- **Digest verification** — `ato decap` checks SHA-256 digests before materializing.
- **Sandbox isolation** — `ato run` executes inside [Nacelle](https://github.com/ato-run/nacelle) for Python and native targets.
- **Strict env handling** — missing required env vars stop execution before launch.

---

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `ATO_TOKEN` | Auth token for publishing and CI | — |
| `CAPSULE_WATCH_DEBOUNCE_MS` | Debounce for `run --watch` in ms | `300` |
| `CAPSULE_ALLOW_UNSAFE` | Set `1` to allow `--dangerously-skip-permissions` | — |

Credentials are stored in `~/.config/ato/credentials.toml` and resolved in this order: `ATO_TOKEN` env var → OS keyring → credentials file.

---

## Contributing

Bug reports and feature requests are welcome in [GitHub Issues](https://github.com/ato-run/ato-cli/issues).

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
