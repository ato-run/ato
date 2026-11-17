# Changelog

This file records user-visible changes to the ato CLI, newest first. Release
tags v<version> must match apps/cli/Cargo.toml; cargo-dist refuses a tag whose
version is behind an already-published one.

## 0.9.5 — 2026-11-18

- Install native GUI capsules as persistent local Instances with `ato app install`.
  Restart with `ato app start` and preserve designs in the displayed Saved data
  directory. `ato run` remains an ephemeral execution.
- Materialize verified macOS application DMGs and reuse integrity-checked
  installed trees. Observe the launched application's visible window before
  issuing a satisfied GUI Contract receipt.
- Package source-built GUI executables as local macOS apps. Bind the local app
  identity with an ad-hoc signature while continuing to require explicit
  `--allow-native-source-build --allow-unsigned-native` on each start.
- Stream source build progress, verify cached outputs before reuse, and publish
  source build cache entries atomically. Record the actual materialized GUI
  executable in receipts.

SolidCraft GUI acceptance on Apple M1 covers both the official DMG and pinned
source build: create and edit a solid, save, restart, reopen, and source build
cache reuse. See `docs/ops/solidcraft-gui-staging-095-acceptance-20261118.md`.
Native GUI acceptance is macOS arm64; other CLI build targets do not imply
native GUI support on those hosts.

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
