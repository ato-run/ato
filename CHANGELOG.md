# Changelog

All notable changes to `ato-cli` will be documented in this file.

## [Unreleased]

## [0.4.84] - 2026-04-25

### What Changed


#### CI

- Add intel mac target and core-crate purity lint (PR-2)


#### Documentation

- Add monorepo consolidation plan for post-v0.5.0 migration

- Add L8/L9/L10/L11 for v0.5 distribution scope


#### Other Changes

- Merge pull request #326 from ato-run/dev

- Add authors + WiX template for cargo-dist MSI

- Add WiX upgrade-guid/path-guid for cargo-dist MSI

- Generate Windows MSI for CLI-only installs


## [0.4.83] - 2026-04-25

### What Changed


#### Documentation

- Document v0.5 distribution channels (Plan B)


#### Features

- Freeze schema_version to ccp/v1 (PR-1a)


## [0.4.82] - 2026-04-25

### What Changed


#### CI

- Fix gitleaks false positives and retire obsolete v3-parity workflow


#### Features

- Advance release work

- Emit missing_schema in E103 envelope for desktop config UI


#### Refactoring

- Consolidate workspace scratch under .ato/ (eliminate .tmp/)


## [0.4.81] - 2026-04-24

### What Changed


#### Bug Fixes

- Enable trusted publishing for release-plz


#### Features

- Auto-bootstrap age identity during interactive auth login


## [0.4.80] - 2026-04-24

### What Changed


#### Bug Fixes

- Surface structured store errors (cf-ray, request_id) in publish flow

- Ensure publisher signing key exists for existing publishers on login

- Remove invalid --share flag from post-run and decap suggestions


#### Documentation

- Enhance usage instructions for consuming and producing projects


#### Features

- Use ?format=id for scoped capsule id lookup

- Migrate upload HTTP to curl with exponential backoff retry


## [0.4.79] - 2026-04-24

### What Changed


#### Bug Fixes

- Fix all clippy -D warnings errors (unused var, dead code, redundant closures)

- Set submodules: false to unblock CI for private samples submodule

- Move host-isolation and shadow-workspace out of cwd, add SIGINT cleanup

- Address code review items from cwd pollution fix

- Clean up orphaned pre-watch normalization attempt

- Move directory run-attempt state to ~/.ato/runs/


#### Other Changes

- Apply cargo fmt and clean up .ato/share artifacts

- Add CHANGELOG entries for cwd pollution fixes


#### Refactoring

- Remove OS keyring from auth storage (Phase 5)

- Replace samples/ dir with sample-capsules submodule

- Migrate auth tokens onto shared credential backend (Phase 2)

- Introduce shared credential backend layer (Phase 1)

- Remove unused mag:// URI scheme module


### What Changed

#### Bug Fixes

- **Performance**: `ato run .` on directory projects no longer accumulates
  `attempt-<nanos>/` directories under `<cwd>/.ato/tmp/source-inference/`.
  All run-attempt state is now always stored in `~/.ato/runs/source-inference/`
  regardless of whether the target is a single script, directory project,
  canonical lock, or compatibility project (`USE_HOME_RUN_STATE`).
  Fixes the "cwd is heavy after repeated runs" regression introduced in v0.4.42.
  (Commit f44b1b8 partially migrated single-scripts to home but left directory
  projects writing to cwd.)

- **Watch mode**: The pre-watch normalization in `execute_watch_mode_with_install`
  previously created an orphaned run-attempt directory (no cleanup scope).
  It is now cleaned up immediately after the preliminary normalization completes.

- **cwd pollution — host isolation**: `apply_host_isolation` previously created
  `<cwd>/.ato/tmp/.ato-run-host/{home,tmp,cache,config}` on every `ato run .`.
  These dirs are now created under `~/.ato/cache/run-host/` so the user's project
  directory is never written to.

- **cwd pollution — auto-provision shadow workspace**: `prepare_shadow_workspace`
  previously created `<cwd>/.ato/tmp/ato-auto-provision/run-<nanos>/` when a
  project needed auto-provisioning. These dirs are now created under
  `~/.ato/cache/auto-provision/`. Additionally, `.ato/` is now excluded from
  snapshot copies (alongside `.git`, `node_modules`, etc.) to prevent recursive
  growth across repeated runs.

- **SIGINT cleanup**: Pressing Ctrl+C during `ato run` now triggers cleanup of
  in-flight run artifacts (registered via `CleanupScope::register_remove_dir`)
  before the process exits. Previously, SIGINT would leave temporary directories
  on disk. Both non-watch mode (exit 130) and watch mode (exit 0) are covered.

## [0.4.78] - 2026-04-23

### What Changed


#### Bug Fixes

- Prune excluded dirs with filter_entry; add artifacts/locks/dist to skip list

- Remove all keychain support from secrets subsystem


#### Features

- Wire session commands and add priority-config support

- Migrate secrets backend to age file encryption


#### Refactoring

- Remove migrate-from-keychain command

## [0.4.77] - 2026-04-23

### What Changed


#### Bug Fixes

- Inject real secrets from secret store in auto-provisioning


## [0.4.76] - 2026-04-23

### What Changed


#### Bug Fixes

- Apply multi-bin selection fix to synthetic npm path


## [0.4.75] - 2026-04-23

### What Changed


#### Bug Fixes

- Select package-name-matching bin when npm package has multiple entrypoints


## [0.4.74] - 2026-04-23

### What Changed


#### Bug Fixes

- Allow npm packages with prepare-only lifecycle scripts


#### Documentation

- Foundation Readiness 0/6, L7 cache GC, TESTING.md


## [0.4.73] - 2026-04-23

### What Changed

#### Bug Fixes

- **#294 runtime PATH leak (pinpoint fix)**: `build_host_node_package_command` and `build_host_node_entrypoint_command` now prepend the managed Node binary directory to `PATH` before spawning the child process. Previously only `build_package_manager_command` did so, meaning direct Node script executions could pick up a host `node`/`npm` and leak out of the managed runtime. A shared `prepend_managed_node_to_path` helper now keeps all three code paths consistent. The broader `RuntimeProvisioner` + `ManagedRuntimePath` abstraction that supersedes this pinpoint fix lands in v0.5.x minor 1 (see RFC `docs/rfcs/draft/UNIFIED_EXECUTION_MODEL.md` §4.2).

#### Known Limitations

- **Synthetic workspace cache is not GC'd in v0.5**: `~/.ato/cache/synthetic/` accumulates one directory per `(provider, package, version)` tuple and is never automatically cleaned in this release. Heavy users (e.g. daily `npm:mintlify` invocations over weeks) will accumulate on the order of hundreds of MB to several GB. Use `du -sh ~/.ato/cache/synthetic/` to inspect, and remove stale directories manually if disk pressure arises. Automatic LRU-based GC with a `ato gc --synthetic` command ships in v0.5.x minor 1 (tracking: RFC `UNIFIED_EXECUTION_MODEL.md` §4.3 / §7.2).

#### Docs

- Added RFC draft `docs/rfcs/draft/UNIFIED_EXECUTION_MODEL.md` outlining the unified Pipeline Spine (`HourglassFlow` variants) and Runtime Spine (`RuntimeProvisioner` + `ManagedRuntimePath`) model targeted for v0.5.x minor 1 / minor 2. Updated `docs/rfcs/accepted/ATO_CLI_SPEC.md` §3.1 to clarify the narrative vs. pipeline separation.

#### CI

- Multi-target E2E host isolation test suite (`e2e-host-isolation.yml`) covering PATH poisoning, wrong-runtime, shim injection, cwd-untouched, child-spawn, login-shell trap, macOS path_helper, Windows PATH case-sensitivity, and symlink-shim across Linux / macOS / Windows.

## [0.4.72] - 2026-04-22

### What Changed


#### Features

- Replace --share/--save-only with --internal/--private/--local flags


## [0.4.71] - 2026-04-21

### What Changed


#### Bug Fixes

- Resolve clippy warnings in presigned, inject, ipc, share, reconstruct

- Include image and component fields in import_target_hints

- Update store domain store.ato.run → ato.run

- Add wasm/wasmtime executor support

- Correct docs URL to docs.ato.run/errors

- Stop readiness port polling when process has already exited

- Move IPC socket dir from /tmp to ~/.ato/run

- Normalize capsule://store/ to capsule://ato.run/ and add reserved publishers


#### Documentation

- Add capsule.toml reference section to README

- P0-1 README english optimization — punchy hero, embedded demo SVG


#### Other Changes

- Cargo fmt


#### Refactoring

- Remove dead CapsuleSigner / legacy_signer.rs (YAGNI)

- Rename capsule_v3 → capsule, drop V2 compat (YAGNI)


## [0.4.70] - 2026-04-20

### What Changed


#### Bug Fixes

- Honor --compatibility-fallback host for share URL runs


#### Documentation

- Optimize README for english developers


#### Features

- Add NL→filter gold corpus (NL2Filter Phase 1)

- Add canonical capability schema (NL2Filter Phase 0)


#### Other Changes

- Bump to v0.11.0 for breaking field additions (capabilities, compat_host)

## [0.4.69] - 2026-04-19

### What Changed

#### Bug Fixes

- Refresh release lint and tests
- Update native delivery Tauri fixture to the current v0.3 form
- Default missing native build working directory when parsing release data

#### Tests

- Align install normalization expectations with current runtime inference and start-script fallback behavior

#### Other Changes

- Cargo fmt

## [0.4.68] - 2026-04-17

### What Changed


#### Bug Fixes

- Move default_git_mode_str import into cfg(test) scope

- Fix remaining clippy errors (sort_by_key, manual_pattern_char_comparison)

- Use exact package name match for AI agent SDK detection

- Handle both nacelle event formats and open_url routing

- Address review findings C-1, C-2, M-1, M-4, m-1

- Add archive source kind for non-git directories


#### Features

- Add Vite port detection and dev script inference

- Add AI agent pattern detection and uv entrypoint inference

- Prefix 'Try now' hint with [hint] tag in grey

- Dim 'Try now' hint so execution output stands out

- Resolve bare slugs from local ~/.ato/store for ato run

- Route ato run <share-url> through nacelle via ShareExecutor


#### Other Changes

- Cargo fmt


#### Refactoring

- Extract share types to capsule-core


## [0.4.67] - 2026-04-16

### What Changed


#### Security

- Harden env validation


## [0.4.66] - 2026-04-15

### What Changed


#### Bug Fixes

- Update rustls-webpki 0.103.11 → 0.103.12 (RUSTSEC-2026-0098)

- B2 — add yarn.lock and packageManager yarn@ detection


#### Features

- D2 — auto-copy .env.example → .env in GitHub checkout

- Phase 2 — ato secrets subcommand with SecretStore module

- Phase 1e — --dry-run flag with secret pattern scanner

- Phase 1d — rpassword masked input for secret-like env keys

- Phase 1c — SecretStorage abstraction, chmod 600 on env files, CI detection

- Add automatic port assignment for capsules


#### Other Changes

- Bump capsule-core 0.9.2 → 0.10.0 for semver-compatible release

- Cargo fmt fixes for env_security tests and Cargo.lock update


#### Security

- A1+A2 — env* exclusions and injection denylist


## [0.4.64] - 2026-04-14

### What Changed

- Bump the patch release after `v0.4.63` was already tagged and published

## [0.4.62] - 2026-04-14

### What Changed

#### Distribution

- Add Homebrew tap support: `brew tap ato-run/ato && brew install ato` now works after the first release
- Auto-generate and push `Formula/ato.rb` to `ato-run/homebrew-ato` on each release via cargo-dist
- Exclude `capsule-core` from cargo-dist release artifacts (prevents `tar_pack_bench` binary from polluting release builds)

#### Installer

- Add `wget` fallback to `install.sh`: prefers `curl` but falls back to `wget` if curl is unavailable
- `wget -qO- https://ato.run/install.sh | sh` now works



### What Changed

#### Tests

- Strengthen E2E-9 (trailing args): assert each argument appears as its own output line, catching splits that `echo ok` would hide
- Strengthen E2E-1 (prompt-env save → reuse): assert the saved env file exists on disk after `Save=yes` before verifying non-TTY reuse
- Strengthen E2E-2 (prompt-env use-once): assert the first PTY run actually produced `DEMO_TOKEN=once-only-value` output, preventing false passes when the run silently fails
- Fix `write_no_env_fixture` entry command: replaced `echo ok` with a shell loop that echoes each `$@` argument on its own line, making argv passthrough meaningful to test
- Add E2E-6 (multi-entry chooser): PTY test that verifies `ato run` shows a numbered chooser when no single primary exists, and runs only the selected entry

## [0.4.60] - 2026-04-12

### What Changed

#### Tests

- Add 10 E2E / interactive integration tests for `encap`, `run`, and `decap` share workflows:
  - `share_run_e2e`: `--watch` reject, `--background` reject, non-TTY missing required env fails closed, failed share-run then rerun (no stale-dir error)
  - `share_interactive_e2e` (PTY): `--prompt-env` save → reuse, use-once, cancel, trailing args passthrough via `--entry ... -- --arg`
  - `share_encap_interactive_e2e` (PTY): primary entry switch persists to `share.spec.json`, zero-primary chooser selection persists
- Fix pre-existing test bug: `capture_workspace_detects_sources_steps_services_and_env` now uses the correct entry ID (`dashboard-dev`) and adds a `.env` fixture to the dashboard repo so the env-file linkage assertion is exercised end-to-end
- Add `expectrl` PTY harness (`expectrl = "0.8"`) as a dev dependency for interactive test automation

## [0.4.59] - 2026-04-11

### What Changed

#### Bug Fixes

- Fix spec/lock digest validation: `load_share_input` now computes the spec digest using canonical serialization (`serde_json::to_vec`) to match `build_share_lock`, preventing false-positive digest mismatch warnings on valid local `share.spec.json` / `share.lock.json` files

#### Tests

- T8b: valid digest verified when loading from local spec path
- T8c: valid digest verified when loading from local lock path
- Fix T9 test to use canonical digest computation

## [0.4.58] - 2026-04-11

### What Changed

#### Bug Fixes

- Fix `encap` interactive primary entry edit: setting a new primary entry now clears all prior primaries immediately, so the saved spec always reflects user intent instead of silently reverting to the first entry
- Add recipient-side tool detection in `decap`: verification now distinguishes between "missing tool in lock" (spec vs lock gap) and "missing tool on this machine" (tool present in lock but absent locally)
- Add spec/lock digest validation: `decap` emits a verification warning when the spec file has changed since the lock was created (sha256 digest mismatch)

#### Tests

- T6: `run <share-url> --watch` is explicitly rejected
- T7: `run <share-url> --background` is explicitly rejected
- T8: digest mismatch surfaces as verification issue
- T9: source present in spec but absent in lock errors at materialize
- T10: tool present in spec but absent in lock is flagged by verify_tools
- T11: verify_local_tools detects tools not installed on current machine
- T12: `--into` path with spaces is accepted by `ensure_target_root_ready`
- Primary entry edit loop clears prior primaries in kept_entries
- `ensure_single_primary_entry` leaves a single primary untouched

## [0.4.57] - 2026-04-10

### What Changed

#### Bug Fixes

- Strip `sha256:` prefix from ephemeral run root directory name to prevent uv/path-separator crash on production share URLs (Fix1)
- Remove stale temp root before re-materializing on share-run rerun, eliminating non-empty directory error after interrupted runs (Fix2)
- Fix zero-primary entry ordering bug in interactive encap; prompt user to select primary entry instead of silently reverting to first entry (Fix3)
- Cross-check spec `tool_requirements` against lock `resolved_tools` in `verify_tools` to surface missing-tool issues at decap time (Fix4)

#### New Features

- Add `--entry`, `--env-file`, `--prompt-env` flags to `ato run` for multi-entry share selection, env file injection, and interactive env prompting with save/reuse lifecycle



### What Changed

#### Bug Fixes

- Accept Store workspace share responses that expose `id` instead of `share_id`, so `ato encap --share` and share revision fetches continue to work against the current Store API payloads

## [0.4.55] - 2026-04-08

### What Changed

#### CI

- Bump the patch release after `v0.4.54` was already tagged and published

## [0.4.54] - 2026-04-04

### What Changed

#### Bug Fixes

- Accept fragment-bearing PyPI simple-index wheel links so packages like `markitdown` resolve correctly

## [0.4.53] - 2026-04-04

### What Changed

#### CI

- Bump the patch release after `v0.4.52` was already tagged and published

## [0.4.52] - 2026-04-04

### What Changed

#### Bug Fixes

- Make `ato run` quiet by default, keep run metadata on stderr, and expose `--verbose` plus `ATO_LOG` for detailed output
- Remove the verbose run logo animation so `ato run --verbose` stays focused on context details

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

#### Bug Fixes

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
