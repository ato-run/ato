# Changelog

This file records user-visible changes to the ato CLI, newest first. Release
tags v<version> must match apps/cli/Cargo.toml; cargo-dist refuses a tag whose
version is behind an already-published one.

## 0.9.4 — 2026-11-17

- Run public Capsule Hub references with explicit Derivation selection and
  verified receipts. Support macOS native applications fetched prebuilt or
  built from pinned source, including persistent verified build reuse.
- Add `ato hub describe` to read and edit version-scoped publisher estimates,
  dependencies and notes. This advice does not change Contract/Derivation
  identity, runtime admission or verification.
- Correct macOS build network resolution and toolchain isolation. Local
  unsigned native applications still require the explicit allowed override.
- Keep unsupported host PTY execution fail closed while building the CLI on
  Windows. Native macOS acceptance does not imply Windows native support.

See `docs/guides/hub-derivation-info.md` for authoring details and
`docs/ops/solidcraft-staging-094-acceptance-20261117.md` for native staging
acceptance measurements.

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
