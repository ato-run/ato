# Changelog

All notable changes to `ato-cli` will be documented in this file.

## [Unreleased]

## [0.4.51] - 2026-04-04

### What Changed

#### CI

- Bump the patch release after `v0.4.50` was already tagged and published

## [0.4.49] - 2026-04-04

### What Changed

#### Bug Fixes

- Align provider-backed PyPI materialization and thin `source/python` execution on the same `runtime_version`-derived managed Python selector so C-extension packages like `markitdown[pdf]` do not hit ABI mismatches at runtime

## [0.4.48] - 2026-04-03

### What Changed

#### CI

- Bump the patch release after `v0.4.47` was already tagged and published
## [0.4.47] - 2026-04-03

### What Changed

### Bug Fixes

- Keep one-shot single-script `ato run` materialization state out of the caller workspace by moving cache and run-attempt state under `~/.ato/cache` and `~/.ato/runs`

## [0.4.46] - 2026-04-03

### What Changed

#### CI

- Bump the patch release after `v0.4.45` was already tagged and published

## [0.4.45] - 2026-04-03

### What Changed

#### Bug Fixes

- Anchor materialized single-script source entrypoints under `source/`, propagate the layout hint through routing/runtime config generation, and fail closed when relative entrypoints are combined with `effective_cwd` without the anchored layout

## [0.4.44] - 2026-04-03

### What Changed

#### Features

- Materialize ato-managed service helpers and service state during install, expose the service root in install output, and refresh service health during bootstrap and repair flows

#### Bug Fixes

- Make host fallback source execution honor the effective cwd so relative outputs land in the caller workspace instead of the materialized root

## [0.4.43] - 2026-04-03

### What Changed

#### Bug Fixes

- Keep single-script relative read/write sandbox paths aligned with the caller/effective cwd instead of drifting into the materialized workspace

- Stop inferring ingress for single-script job workloads and fail closed on job ports during lock/runtime routing

- Materialize single-script run state under the caller workspace root and add markitdown-style regressions for relative PDF input/output behavior

## [0.4.42] - 2026-04-02

### What Changed

#### Features

- Add public `CapsuleType::Job` for one-shot workloads and propagate through manifest, lock, router, and runtime synthesis (#219)
- Single-script source inference emits `job` type; exit code 0 is treated as success for one-shot foreground and background runs (#219)
- Separate authoritative workspace root from synthetic materialization/execution root; add run-context visibility note (#219)
- Add `.ato` internal-state fail-closed guards in `input_resolver` and packer; prune `.ato` during source-inference and payload walks (#219)
- Move single-script generated workspaces to deterministic durable cache roots under `.ato/source-inference/single-script-cache/` (#219)

#### Bug Fixes

- Re-anchor source host isolation under the authoritative workspace root (#219)
- Collapse nested `if` in `path_contains_workspace_internal_subtree` to satisfy clippy (#219)

## [0.4.41] - 2026-04-02

### What Changed

#### Features

- Add the app control CLI surface and snapshots for bootstrap, status, and repair flows

#### Bug Fixes

- Harden install manifest integrity and delivery metadata handling for the new app control flow

#### CI

- Bump the patch release after `v0.4.40` was already tagged and published

## [0.4.40] - 2026-04-02

### What Changed

#### CI

- Bump the patch release after `v0.4.39` was already tagged and published

## [0.4.39] - 2026-04-01

### What Changed

#### CI

- Bump the patch release after `v0.4.38` was already tagged and published

## [0.4.38] - 2026-04-01

### What Changed

#### CI

- Bump the patch release after `v0.4.37` was already tagged and published

## [0.4.37] - 2026-04-01

### What Changed

#### CI

- Bump the patch release after `v0.4.36` was already tagged and published

## [0.4.36] - 2026-04-01

### What Changed

#### Features

- Add Phase 1 exported CLI execution through `ato run @publisher/tool -- ...` for authored `exports.cli.<name>` entries backed by `python-tool`

#### Bug Fixes

- Preserve authored manifest `exports` through build and publish packaging so exported CLI metadata survives artifact round-trips

#### CI

- Bump the patch release after `v0.4.35` was already tagged and published

## [0.4.35] - 2026-03-31

### What Changed

#### CI

- Bump the patch release after `v0.4.34` was already tagged and published

## [0.4.34] - 2026-03-30

### What Changed

#### CI

- Bump the patch release after `v0.4.33` was already tagged and published

## [0.4.33] - 2026-03-30

### What Changed

#### CI

- Bump the patch release after `v0.4.32` was already tagged and published
- Unblock `Release PR` by stopping `.archives/` from being both tracked and ignored

## [0.4.32] - 2026-03-26

### What Changed

#### CI

- Bump the patch release after `v0.4.31` was already tagged and published

## [0.4.31] - 2026-03-25

### What Changed

#### CI

- Bump the patch release after v0.4.30 was already tagged and published

## [0.4.30] - 2026-03-21

### What Changed

#### Features

- Add official and private publish routing with phased publish execution

- Add payload size validation and progressive UI handling across CLI flows

- Add a terminal search UI with event-driven interaction

#### Refactoring

- Reorganize CLI modules plus state and skill handling for a cleaner internal layout

#### CI

- Retry release signing after the previous release signing path regressed

## [0.4.29] - 2026-03-20

### What Changed

#### Bug Fixes

- Unblock `Build (Multi OS)` by resolving `clippy::too_many_arguments` failures in the lockfile and install helper paths

## [0.4.28] - 2026-03-20

### What Changed

#### Features

- Refine CLI and transport error taxonomy with structured inference, provisioning, execution, and internal diagnostics

#### Bug Fixes

- Reclassify consent, preflight, and inferred GitHub draft failures into stable typed error codes with JSON envelopes

## [0.4.27] - 2026-03-18

### What Changed

#### Features

- Add progressive cliclack-based run/install flows with preview plans, manifest review, and unified GitHub auto-install confirmations

- Add compatibility host fallback execution for native and node source targets with isolated host runtime state

#### Bug Fixes

- Keep inferred GitHub source builds and generated manifests on a single interactive timeline without duplicate warnings

- Resolve source launch commands, package includes, and readiness tracking more reliably across node, python, and host-fallback execution paths

## [0.4.26] - 2026-03-18

### What Changed

#### Features

- Add preview session persistence and preview-aware manifest execution paths for store-draft and local preview workflows

#### Bug Fixes

- Resolve runtime lockfile checks from the effective execution working directory for source builds and runs

## [0.4.25] - 2026-03-17

### What Changed

#### Bug Fixes

- Fix inferred GitHub Deno apps so `ato run github.com/...` packages required import maps and executes `deno task` targets through the Deno runtime instead of the generic shell path

## [0.4.24] - 2026-03-15

### What Changed

#### Features

- Add v0.3 runtime and capsule lock support

#### Documentation

- Update v0.3 and registry guidance

## [0.4.23] - 2026-03-13

### What Changed

#### Bug Fixes

- Include the current Ato session token when reading capsule metadata and manifests

## [0.4.22] - 2026-03-13

### What Changed

#### Bug Fixes

- Use `https://api.ato.run` as the default registry fallback

## [0.4.21] - 2026-03-13

### What Changed

#### Bug Fixes

- Improve registry serve bind errors

- Satisfy clippy io-other-error lint

## [0.4.20] - 2026-03-12

### What Changed

#### Bug Fixes

- Share local input resolution across commands

- Add repository field to package metadata in Cargo.toml

- Satisfy clippy for finalize permissions

- [native-only] clear readonly finalize output

- Sign rebased windows artifact

- Hardcode windows finalize target path

- Select windows build result json

- Update README to include DeepWiki badge

- Parse windows build json output

- Stabilize windows native fixture build

- Add Windows tauri test icon

- Increase Windows stack for local registry server

- Flatten registry route construction

- Disable registry ui for windows e2e server

- Harden windows registry e2e readiness checks

- Update artifact naming to include 'ato-cli' prefix and change archive formats

- Refactor update function for improved error handling and output

- Preserve strict-v3 failure path

- Address prompt review feedback

- Correct argument passing in infer_source_driver function

- Improve error handling for UI dependency installation and streamline code

- Remove unused serde_jcs dependency from Cargo.toml

- Replace zip command with PowerShell Compress-Archive for Windows builds

- Update node-modules-dir argument to remove auto option and enhance local registry tests

- Align rebuild CLI contract and wait behavior

- Update errno function to use std::io::Error for better error handling

#### CI

- [native-only] add fast native delivery path

- Add native delivery windows workflow

- Tighten release workflow permissions

- Add release readiness workflows

#### Documentation

- Add windows native delivery e2e postmortem

- Remove note about OpenSSF Best Practices badge from README

- Add README status badges

- Clarify release tagging flow

- Update CLI docs and add artifact publish scripts

- Add Apache-2.0 LICENSE and trademark guidelines

#### Features

- Refactor token handling to use current_session_token for consistency

- Refactor app structure and enhance UI components

- Update release workflows to enhance token handling and remove R2 upload process

- Enhance Deno command handling and environment setup

- Add cargo audit configuration and update dependencies

- Implement fallback update mechanism and enhance installer URL resolution

- Detect and validate windows native executables

- Add update command to ato CLI for version management

- Expand init framework prompt detection

- Generate agent-ready prompts from ato init

- Introduce experimental native delivery support

- Enhance process cleanup by integrating service binding cleanup functionality

- Refactor service binding synchronization and add cleanup functionality

- Add sync functionality for local service bindings from a running process

- Enhance service binding registration with process ID and port options

- Add support for registering local service bindings and enhance service binding handling

- Add ingress TLS bootstrap and serving commands

- Add service binding resolution with allowed callers enforcement

- Implement host-side service binding management and API

- Implement persistent state management and API

- Add inspect requirements json output

- Update dependencies and enhance install command with artifact persistence

- Improve keyring error handling and add wait_for_single_process function in tests

- Enhance native delivery with local derivation and projection support

- Add platform-specific symlink creation for native delivery

- Add experimental project and unproject commands for symlink management

- Update README for new commands and options in ato-cli

- Enhance ConfirmActionModal with extra content support

- Update error messages to include --strict-v3 for clarity in diagnostics

- Consolidate JSON-RPC imports for improved clarity

- Refactor IPC invoke handling for improved readability and maintainability

- Refactor IPC invoke handling for improved platform compatibility and streamline code

- Enhance Windows support for npm handling and improve UI build process

- Add Windows-specific guest command handling and update dependencies

- Enhance build process and clean up code across multiple modules

- Refactor and clean up code across multiple modules

- Migrate web deno orchestration to services supervisor mode

- Add dynamic app capsule recipe and update symlink handling in packaging

- Remove obsolete allowlist for known false positives in historical test commits

- Update workflows to trigger on push events for dev and main branches, remove pull request triggers

- Add Cloudflare secrets check for R2 upload and ensure proper permissions in secret scan workflow

- Enhance R2 upload workflow with additional checks and fallback bucket options

- Add Japanese README.md for localization support

- Add CODEOWNERS file to define repository ownership

- Add support for zip archives and update scripts for multi-OS builds

- Update version to 0.4.0 and add subtle dependency; enhance process identity checks and token validation

- Add execution_required_envs method and preflight check for required environment variables

- Bump version to 0.3.8 and enhance Deno artifact handling

- Add static file server implementation in Deno

- Bump version to 0.3.4 and add private key file; enhance CI publish error handling

- Bump version to 0.3.2 in Cargo.toml and Cargo.lock

- Add retry logic for upload failures and update default values for PUT_RETRIES and PUT_RETRY_SLEEP

- Add cargo_version output and enhance R2 upload steps for semver aliases

- Add validation step for Cargo.lock in CI workflow

- Enhance CI workflow validation and OIDC token handling

- Add dry run functionality for capsule publishing

- Add cache control options for R2 object uploads

- Enhance multi-OS build workflow with configurable build targets and rustflags

- Add support for Windows target with rustflags configuration

- Enhance R2 upload workflow with configurable inputs and improve wrangler config handling

- Implement Windows process termination and enhance process management

- Add conditional compilation for Unix-specific imports in runtime and IPC modules

- Add config schema for Nacelle runtime configuration

- Implement multi-OS build workflow and enhance guest protocol handling

- Finalize local registry, runtime guard, and silent runner UX

- Improve run auto-install flow and registry handling

- Implement manifest validation for build process and add smoke testing functionality

- Add miette diagnostics with JSON error envelope

- Add auth/search/source/install and IPC command surface

- Expand runtime and capsule type foundations

- Implement process management functionality

- Update README and add tests for Python, Node, Deno, Bun, and custom app config generation

- Enhance error handling in main function with specific CapsuleError messages

- Add session file pattern to .gitignore

- Add Cap'n Proto conversion for CapsuleManifestV1

- Add R3 configuration generation and enforcement

- Phase 2 - Add validation pipeline (L1/L2/L4)

- Add scaffold command for generating Dockerfiles and .dockerignore

- Add Rust/Go binary distribution flow

#### Other Changes

- Refactor path handling and validation in manifest and execution plan

- Update border-radius and font-family across stylesheets for consistency

- Restore 0.4.19 release metadata

- Revert "chore: update dependencies and remove unused packages"

- Update dependencies and remove unused packages

- Fix native delivery smoke fixtures

- Support store-backed GitHub install drafts

- Polish GitHub repo install error messaging

- Fix GitHub repo install normalization and tarball unpacking

- Bump version to 0.4.18 ([#145](https://github.com/Koh0920/ato-cli/pull/145))

- Fix release SBOM upload repo context ([#144](https://github.com/Koh0920/ato-cli/pull/144))

- Bump version to 0.4.17 ([#143](https://github.com/Koh0920/ato-cli/pull/143))

- Merge dev into main for 0.4.17 ([#142](https://github.com/Koh0920/ato-cli/pull/142))

- Fix repo context in release signing backfill ([#141](https://github.com/Koh0920/ato-cli/pull/141))

- Add release signing backfill workflow

- Fix cosign bundle output in release workflow ([#139](https://github.com/Koh0920/ato-cli/pull/139))

- Merge origin/main into dev

- Bump version to 0.4.16

- Add Windows native-delivery E2E coverage with committed Tauri fixture ([#126](https://github.com/Koh0920/ato-cli/pull/126))

- Linux向けProjection処理を追加し、`.desktop` と `~/.local/bin` 連携を実装 ([#125](https://github.com/Koh0920/ato-cli/pull/125))

- Relax native delivery config validation for Linux targets and non-signing finalize tools ([#124](https://github.com/Koh0920/ato-cli/pull/124))

- Merge pull request #116 from Koh0920/copilot/stabilize-native-delivery-metadata

- Merge pull request #123 from Koh0920/copilot/implement-linux-artifact-detection

- Merge pull request #122 from Koh0920/copilot/implement-windows-projection

- Bump version to 0.4.14 in Cargo.toml and Cargo.lock

- Bump version to 0.4.13 in Cargo.toml and Cargo.lock

- Fix description formatting and update tag input reference in upload workflow

- Add ASSET_PREFIX variable for consistent asset naming in R2 uploads

- Fix R2 upload workflow

- Retag latest release as 0.4.12

- Fix Windows UI bootstrap checksum

- Prepare 0.4.13 release

- Prepare 0.4.12 dist release

- Clarify native delivery fail-closed follow-ups

- Fix native finalize path rebasing for 0.4.11

- Release v0.4.10 ([#85](https://github.com/Koh0920/ato-cli/pull/85))

- Refresh Cargo.lock for 0.4.9

- Bump version to 0.4.9

- Bump version to 0.4.8

- Address inspect requirements review feedback

- Merge pull request #65 from Koh0920/copilot/add-storage-mount-schema

- Fix clippy sort_by_key warning

- Add native delivery fetch/finalize PoC

- Implement target-based configuration generation and refactor engine installation logic

- Refactor code for improved readability and maintainability

- Keep nacelle release flow in nacelle repo

- Add nacelle R2 mirror release flow

- Improve build lockfile performance and timings

- Fix local registry UI env aggregation and release 0.4.6

- Skip unsupported universal lock targets

- Skip unsupported Deno platforms in universal locks

- Regenerate stale universal lockfiles

- Release 0.4.2

- Bump CLI version to 0.4.1

- Expand web runtime lockfile platforms

- Merge origin/main into dev

- Feat/multi cas ([#43](https://github.com/Koh0920/ato-cli/pull/43))

- Feat/multi cas ([#42](https://github.com/Koh0920/ato-cli/pull/42))

- Add transient step progress for pack/publish to avoid “silent wait” UX ([#39](https://github.com/Koh0920/ato-cli/pull/39))

- Refactor trademark guidelines, remove obsolete scripts, and clean up skills

- Mask staging domains and bucket defaults for public repo

- Sanitize absolute paths in skill verify prompts

- Remove sensitive and unnecessary files before public release

- Remove sensitive/temp files and harden gitignore

- Update .github/workflows/build-multi-os.yml

- Update readme

- Update to 0.3.9

- Update to 0.3.8

- Bump version to 0.3.6 and update publish command for keyless OIDC CI

- 0.3.5

- Refactor publish command tests to align with new CLI structure

- Remove branch and path ignore settings from build workflow

- Update Cargo.lock

- Update version

- Refactor CI workflows and clean up dependencies

- Refactor consent handling and enhance permission management

- Reliability update v0.2.1 final polish

- Update tracked files

- Add untracked files

- Refactor error handling to use CapsuleError across various modules

- Implement CLI reporters for metrics reporting: StdoutReporter and JsonReporter

- Bump version to 0.2.0 in Cargo files

- Unify new/init using recipe engine

- Fix Bun flag placement in node template

- Standardize Bun dev/release workflow

- エンジンの登録機能を追加し、設定ファイルの読み書きを実装。依存関係に `toml` を追加し、エンジンの発見ロジックを改善。

- Initial commit

#### Refactoring

- Refactor native delivery platform abstractions

- Streamline framework detection logic and improve readability

- Simplify variable initialization in persistent state test

- Clean up error handling and improve script documentation

#### Tests

- [native-only] fix windows authenticode verification

- Cover windows pe validation branches

- Test native delivery follow-up constraints

- Test native delivery platform refactor

- Tighten expanded init prompt coverage

- Add test for persistent state columns creation in fresh database

- Validate native delivery build path

- Cover native delivery build integration

- Accept authenticated preflight failure for source rebuild alias

- Extend e2e coverage for new IPC and auth flows

- Add E2E test for validation pipeline
