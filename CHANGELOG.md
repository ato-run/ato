# Changelog

This file records user-visible changes to the ato CLI, newest first. Release
tags v<version> must match apps/cli/Cargo.toml; cargo-dist refuses a tag whose
version is behind an already-published one.

## 0.1.0 — unreleased

The 0.1.0 line restarts the product version of the renewed tree. It follows
0.8.2 on the same channels (GitHub Releases, Homebrew `ato-run/homebrew-ato`,
shell installer, MSI). The pre-renewal repository is preserved in
https://github.com/ato-run/ato-archive-2026-11.

### Distribution
- `ato-cli`, `ato-provider-nacelle`, `netd` and the nacelle eBPF crate are
  versioned 0.1.0, matching the other workspace crates.
- The Windows MSI uses a new upgrade code, so it does not upgrade or block on
  an installed 0.8.x MSI; uninstall 0.8.x first if it is present.
- Homebrew and installer behaviour when moving from 0.8.2 to 0.1.0 is not yet
  verified (tracked in TODO #223).

### Removed
- In-tree `apps/desktop` and `apps/desktop-tauri` (the Desktop is released from
  `ato-run/ato-desktop`), `benchmarks/ready-state`, `tools/legacy-vm-inspector`,
  `spikes/`, desky mock samples, `docs/aodd` and `docs/archive`.

## 0.8.1 — unreleased

### Added
- ato login [--api-url URL] [--headless]: browser-approved device-code login
  (PKCE) against the Ato API. The credential is stored per API origin in
  $ATO_HOME/credentials.json (0600 on Unix) and is never printed.
- ato upload --auth-token now falls back to the stored credential for the
  target API origin, so 'ato login && ato upload' works without copying a
  token into shell history or CI variables.

### Fixed
- ato-runtime-attempt test fixtures now carry the optional
  StateAttachmentV1.backing field (the workspace test suite did not compile
  on main).

### Distribution
- cargo-dist now also builds an Intel macOS (x86_64-apple-darwin) target and
  publishes a shell installer in addition to Homebrew and MSI.

## 0.8.0 — 2026-07-28

Baseline release line for the current CLI command surface (init, run, pack,
form, app, upload, runtime-network, ...). Older entries are kept in the
release pages of the repository.
