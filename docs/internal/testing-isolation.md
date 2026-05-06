# Testing Isolation

Hermetic verification in this repo is driven by one root switch: `ATO_HOME`.
All global ato state under `.ato/` should follow that root. For CLI and desktop flows that still need OS-level config locations, pair it with isolated `HOME`, `XDG_CONFIG_HOME`, and `XDG_CACHE_HOME`.

## Fixture model

- `ATO_HOME` moves the canonical ato state tree: `run`, `logs`, `store`, `cache`, `trust`, `apps`, desktop config, desktop tabs, secrets, and MCP discovery.
- `HOME` is still relevant for APIs that are intentionally outside `.ato/`, such as login keychains or default XDG expansion.
- `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` isolate canonical auth metadata and cache roots that are not stored under `.ato/`.

## Fast path

Use [scripts/ato-test-shell.sh](../../scripts/ato-test-shell.sh) to enter an isolated shell:

```bash
./scripts/ato-test-shell.sh
```

Run a single command in isolation:

```bash
./scripts/ato-test-shell.sh --print-env ./target/debug/ato run --sandbox -y capsule://github.com/Koh0920/WasedaP2P
```

## MCP verification

Desktop and MCP must share the same `ATO_HOME`.

```bash
./scripts/ato-test-shell.sh --print-env

ATO_HOME="$ATO_HOME" cargo run --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop
ATO_HOME="$ATO_HOME" cargo run --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop-mcp
```

The discovery socket and `ato-desktop-current.json` will be created under `$ATO_HOME/run/`, not the developer's real `~/.ato/run/`.

## Manual suite

Every script under [tests/manual](../../tests/manual) sources [tests/manual/config.sh](../../tests/manual/config.sh).
That helper now creates a fresh per-suite environment root by default and exports:

- `ATO_HOME`
- `HOME`
- `XDG_CONFIG_HOME`
- `XDG_CACHE_HOME`

Set `ATO_TEST_HERMETIC=0` only if you intentionally need to run against your real local state.

## Guard

Use [scripts/check-ato-home-paths.sh](../../scripts/check-ato-home-paths.sh) to catch new direct `HOME -> .ato` path derivations in product source.