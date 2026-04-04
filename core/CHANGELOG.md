# Changelog

All notable changes to `ato-cli` will be documented in this file.

## [0.9.2] - 2026-04-04

### What Changed

#### CI

- Bump the patch release after `v0.9.1` was already tagged and published

## [0.9.1] - 2026-04-04

### What Changed

#### Features

- Add shared Python runtime selector helpers so lock-derived runtime generation and source execution derive the same managed Python transport from `runtime_version`

## [0.9.0] - 2026-04-03

### What Changed

#### Breaking Changes

- Add public `source_layout` metadata to manifest and resolved runtime targets so anchored single-script source execution can preserve caller cwd semantics across materialized runs

## [0.8.0] - 2026-04-02

### What Changed

#### Features

- Add public `CapsuleType::Job` enum variant for one-shot workload classification (#219)
- Add `path_contains_workspace_state_dir` and `path_contains_workspace_internal_subtree` path helpers in `common::paths` (#219)
- Propagate capsule type through `LockContractMetadata` and router synthesis (#219)
- Add fail-closed rejection for explicit authoritative inputs under workspace-local `.ato` internal state in `input_resolver` (#219)
- Add explicit `.ato` payload root fail-closed guard and `.ato` directory skip in packer (#219)

## [0.7.1] - 2026-04-02

### What Changed

#### Features

- Add public delivery bootstrap and service environment lock metadata for app control aware installs

#### Bug Fixes

- Validate persisted delivery metadata before install consumers use bootstrap, healthcheck, and repair hints

## [0.7.0] - 2026-03-29

### What Changed

#### Breaking Changes

- Rename public packer option fields from manifest-path-first to workspace/compat-input-first wiring used by lock-first packaging flows

#### Bug Fixes

- Keep generated compatibility artifacts under .ato/derived instead of scattering capsule.lock.json, config.json, and lock input snapshots at the workspace root

## [0.6.0] - 2026-03-27

### What Changed

#### Breaking Changes

- Add public single-file input resolution and lock-first execution metadata required by the new init and run flows
- Make top-level `ato init` durable-only; compatibility `capsule.toml` scaffolding remains on `ato build --init` and recovery paths

#### Bug Fixes

- Compute registry `closure_digest` from canonical normalized `resolution.closure` instead of hashing raw serialized JSON

#### Documentation

- Align manifest examples and lock-first migration notes with current `capsule.toml` and `ato init` behavior

## [0.5.6] - 2026-03-26

### What Changed

#### Bug Fixes

- Unblock clippy on dev

- Satisfy release clippy gates for diagnostics

- Stabilize inferred github source apps

- Support inferred deno github repos

- Harden github install and build fallback flows

- Correct argument passing in infer_source_driver function

#### Features

- Enhance inspect command with additional subcommands for lock, preview, diagnostics, and remediation

- Implement durable workspace materialization and legacy manifest generation

- Introduce input resolver and enhance manifest handling

- Implement ato_lock module with canonicalization, hashing, and validation functionality

- Implement GitHub auto-fix modes and related functionality

- Introduce PipelineAttemptContext for enhanced phase management

- Refine error taxonomy diagnostics

- Polish progressive runtime release flow

- Add preview session release candidate

- Support chml manifests and v0.3 runtime flows

- Add v0.3 runtime and capsule lock support

- Enhance Deno command handling and environment setup

- Add service binding scope initialization in test cases

- Implement host-side service binding management and API

- Implement persistent state management and API

- Add persistent state bindings and thin registry

- Add state-first manifest poc schema

- Add platform-specific symlink creation for native delivery

- Implement payload v3 manifest generation and related utilities

- Enhance build process and clean up code across multiple modules

- Refactor and clean up code across multiple modules

- Migrate web deno orchestration to services supervisor mode

- Lockfile-aware SBOM with JCS-canonical metadata and non-blocking generation path ([#40](https://github.com/Koh0920/ato-cli/pull/40))

- Add dynamic app capsule recipe and update symlink handling in packaging

- Add execution_required_envs method and preflight check for required environment variables

- Bump version to 0.3.8 and enhance Deno artifact handling

- Add static file server implementation in Deno

- Add conditional compilation for Unix-specific imports in runtime and IPC modules

- Add config schema for Nacelle runtime configuration

- Finalize local registry, runtime guard, and silent runner UX

- Implement manifest validation for build process and add smoke testing functionality

- Expand runtime and capsule type foundations

- Implement process management functionality

- Update README and add tests for Python, Node, Deno, Bun, and custom app config generation

#### Other Changes

- Release

- Normalize inferred node scripts and yarn locks

- Support authoritative ato.lock.json execution

- Fix dev CI clippy and parity regressions

- Harden lock-first workspace state and add CLI regressions

- Release

- Release

- Merge pull request #181 from Koh0920/dev

- Bump ato-cli to 0.4.28

- Bump capsule-core to 0.3.0

- Release 0.4.25

- Release ([#155](https://github.com/Koh0920/ato-cli/pull/155))

- Release

- Refactor path handling and validation in manifest and execution plan

- Polish persistent state docs and retries

- Finalize state-first poc validation

- Dev ([#49](https://github.com/Koh0920/ato-cli/pull/49))

- Fix clippy for v0.4.7 release

- Improve build lockfile performance and timings

- Skip unsupported universal lock targets

- Skip unsupported Deno platforms in universal locks

- Regenerate stale universal lockfiles

- Improve lockfile runtime platform detection and expand related tests

- Expand web runtime lockfile platforms

- Feat/multi cas ([#43](https://github.com/Koh0920/ato-cli/pull/43))

- Feat/multi cas ([#42](https://github.com/Koh0920/ato-cli/pull/42))

- Remove sensitive and unnecessary files before public release

- Update to 0.3.8

- Refactor CI workflows and clean up dependencies

- Refactor consent handling and enhance permission management

- Reliability update v0.2.1 final polish

- Update tracked files

- Add untracked files

- Refactor error handling to use CapsuleError across various modules

- Implement CLI reporters for metrics reporting: StdoutReporter and JsonReporter

#### Refactoring

- Streamline ato-cli modules and remove dead code ([#171](https://github.com/Koh0920/ato-cli/pull/171))

- Clean up error handling and improve script documentation

#### Tests

- Clarify capsule SBOM verification assertions

- Verify SBOM embedding in capsule packer

## [0.5.5] - 2026-03-26

### What Changed

#### Breaking Changes

- Add public single-file input resolution and lock-first execution metadata required by the new init and run flows

## [0.5.4] - 2026-03-24

### What Changed

#### Bug Fixes

- Unblock clippy on dev

- Satisfy release clippy gates for diagnostics

- Stabilize inferred github source apps

- Support inferred deno github repos

- Harden github install and build fallback flows

- Correct argument passing in infer_source_driver function

#### Features

- Introduce PipelineAttemptContext for enhanced phase management

- Refine error taxonomy diagnostics

- Polish progressive runtime release flow

- Add preview session release candidate

- Support chml manifests and v0.3 runtime flows

- Add v0.3 runtime and capsule lock support

- Enhance Deno command handling and environment setup

- Add service binding scope initialization in test cases

- Implement host-side service binding management and API

- Implement persistent state management and API

- Add persistent state bindings and thin registry

- Add state-first manifest poc schema

- Add platform-specific symlink creation for native delivery

- Implement payload v3 manifest generation and related utilities

- Enhance build process and clean up code across multiple modules

- Refactor and clean up code across multiple modules

- Migrate web deno orchestration to services supervisor mode

- Lockfile-aware SBOM with JCS-canonical metadata and non-blocking generation path ([#40](https://github.com/Koh0920/ato-cli/pull/40))

- Add dynamic app capsule recipe and update symlink handling in packaging

- Add execution_required_envs method and preflight check for required environment variables

- Bump version to 0.3.8 and enhance Deno artifact handling

- Add static file server implementation in Deno

- Add conditional compilation for Unix-specific imports in runtime and IPC modules

- Add config schema for Nacelle runtime configuration

- Finalize local registry, runtime guard, and silent runner UX

- Implement manifest validation for build process and add smoke testing functionality

- Expand runtime and capsule type foundations

- Implement process management functionality

- Update README and add tests for Python, Node, Deno, Bun, and custom app config generation

#### Other Changes

- Release

- Merge pull request #181 from Koh0920/dev

- Bump ato-cli to 0.4.28

- Bump capsule-core to 0.3.0

- Release 0.4.25

- Release ([#155](https://github.com/Koh0920/ato-cli/pull/155))

- Release

- Refactor path handling and validation in manifest and execution plan

- Polish persistent state docs and retries

- Finalize state-first poc validation

- Dev ([#49](https://github.com/Koh0920/ato-cli/pull/49))

- Fix clippy for v0.4.7 release

- Improve build lockfile performance and timings

- Skip unsupported universal lock targets

- Skip unsupported Deno platforms in universal locks

- Regenerate stale universal lockfiles

- Improve lockfile runtime platform detection and expand related tests

- Expand web runtime lockfile platforms

- Feat/multi cas ([#43](https://github.com/Koh0920/ato-cli/pull/43))

- Feat/multi cas ([#42](https://github.com/Koh0920/ato-cli/pull/42))

- Remove sensitive and unnecessary files before public release

- Update to 0.3.8

- Refactor CI workflows and clean up dependencies

- Refactor consent handling and enhance permission management

- Reliability update v0.2.1 final polish

- Update tracked files

- Add untracked files

- Refactor error handling to use CapsuleError across various modules

- Implement CLI reporters for metrics reporting: StdoutReporter and JsonReporter

#### Refactoring

- Streamline ato-cli modules and remove dead code ([#171](https://github.com/Koh0920/ato-cli/pull/171))

- Clean up error handling and improve script documentation

#### Tests

- Clarify capsule SBOM verification assertions

- Verify SBOM embedding in capsule packer

## [0.5.3] - 2026-03-24

### What Changed

#### Bug Fixes

- Unblock clippy on dev

- Satisfy release clippy gates for diagnostics

- Stabilize inferred github source apps

- Support inferred deno github repos

- Harden github install and build fallback flows

- Correct argument passing in infer_source_driver function

#### Features

- Introduce PipelineAttemptContext for enhanced phase management

- Refine error taxonomy diagnostics

- Polish progressive runtime release flow

- Add preview session release candidate

- Support chml manifests and v0.3 runtime flows

- Add v0.3 runtime and capsule lock support

- Enhance Deno command handling and environment setup

- Add service binding scope initialization in test cases

- Implement host-side service binding management and API

- Implement persistent state management and API

- Add persistent state bindings and thin registry

- Add state-first manifest poc schema

- Add platform-specific symlink creation for native delivery

- Implement payload v3 manifest generation and related utilities

- Enhance build process and clean up code across multiple modules

- Refactor and clean up code across multiple modules

- Migrate web deno orchestration to services supervisor mode

- Lockfile-aware SBOM with JCS-canonical metadata and non-blocking generation path ([#40](https://github.com/Koh0920/ato-cli/pull/40))

- Add dynamic app capsule recipe and update symlink handling in packaging

- Add execution_required_envs method and preflight check for required environment variables

- Bump version to 0.3.8 and enhance Deno artifact handling

- Add static file server implementation in Deno

- Add conditional compilation for Unix-specific imports in runtime and IPC modules

- Add config schema for Nacelle runtime configuration

- Finalize local registry, runtime guard, and silent runner UX

- Implement manifest validation for build process and add smoke testing functionality

- Expand runtime and capsule type foundations

- Implement process management functionality

- Update README and add tests for Python, Node, Deno, Bun, and custom app config generation

#### Other Changes

- Merge pull request #181 from Koh0920/dev

- Bump ato-cli to 0.4.28

- Bump capsule-core to 0.3.0

- Release 0.4.25

- Release ([#155](https://github.com/Koh0920/ato-cli/pull/155))

- Release

- Refactor path handling and validation in manifest and execution plan

- Polish persistent state docs and retries

- Finalize state-first poc validation

- Dev ([#49](https://github.com/Koh0920/ato-cli/pull/49))

- Fix clippy for v0.4.7 release

- Improve build lockfile performance and timings

- Skip unsupported universal lock targets

- Skip unsupported Deno platforms in universal locks

- Regenerate stale universal lockfiles

- Improve lockfile runtime platform detection and expand related tests

- Expand web runtime lockfile platforms

- Feat/multi cas ([#43](https://github.com/Koh0920/ato-cli/pull/43))

- Feat/multi cas ([#42](https://github.com/Koh0920/ato-cli/pull/42))

- Remove sensitive and unnecessary files before public release

- Update to 0.3.8

- Refactor CI workflows and clean up dependencies

- Refactor consent handling and enhance permission management

- Reliability update v0.2.1 final polish

- Update tracked files

- Add untracked files

- Refactor error handling to use CapsuleError across various modules

- Implement CLI reporters for metrics reporting: StdoutReporter and JsonReporter

#### Refactoring

- Streamline ato-cli modules and remove dead code ([#171](https://github.com/Koh0920/ato-cli/pull/171))

- Clean up error handling and improve script documentation

#### Tests

- Clarify capsule SBOM verification assertions

- Verify SBOM embedding in capsule packer

## [0.5.2] - 2026-03-24

### What Changed

#### Bug Fixes

- Unblock clippy on dev

- Satisfy release clippy gates for diagnostics

- Stabilize inferred github source apps

- Support inferred deno github repos

- Harden github install and build fallback flows

- Correct argument passing in infer_source_driver function

#### Features

- Refine error taxonomy diagnostics

- Polish progressive runtime release flow

- Add preview session release candidate

- Support chml manifests and v0.3 runtime flows

- Add v0.3 runtime and capsule lock support

- Enhance Deno command handling and environment setup

- Add service binding scope initialization in test cases

- Implement host-side service binding management and API

- Implement persistent state management and API

- Add persistent state bindings and thin registry

- Add state-first manifest poc schema

- Add platform-specific symlink creation for native delivery

- Implement payload v3 manifest generation and related utilities

- Enhance build process and clean up code across multiple modules

- Refactor and clean up code across multiple modules

- Migrate web deno orchestration to services supervisor mode

- Lockfile-aware SBOM with JCS-canonical metadata and non-blocking generation path ([#40](https://github.com/Koh0920/ato-cli/pull/40))

- Add dynamic app capsule recipe and update symlink handling in packaging

- Add execution_required_envs method and preflight check for required environment variables

- Bump version to 0.3.8 and enhance Deno artifact handling

- Add static file server implementation in Deno

- Add conditional compilation for Unix-specific imports in runtime and IPC modules

- Add config schema for Nacelle runtime configuration

- Finalize local registry, runtime guard, and silent runner UX

- Implement manifest validation for build process and add smoke testing functionality

- Expand runtime and capsule type foundations

- Implement process management functionality

- Update README and add tests for Python, Node, Deno, Bun, and custom app config generation

#### Other Changes

- Release

- Bump ato-cli to 0.4.28

- Bump capsule-core to 0.3.0

- Release 0.4.25

- Release ([#155](https://github.com/Koh0920/ato-cli/pull/155))

- Release

- Refactor path handling and validation in manifest and execution plan

- Polish persistent state docs and retries

- Finalize state-first poc validation

- Dev ([#49](https://github.com/Koh0920/ato-cli/pull/49))

- Fix clippy for v0.4.7 release

- Improve build lockfile performance and timings

- Skip unsupported universal lock targets

- Skip unsupported Deno platforms in universal locks

- Regenerate stale universal lockfiles

- Improve lockfile runtime platform detection and expand related tests

- Expand web runtime lockfile platforms

- Feat/multi cas ([#43](https://github.com/Koh0920/ato-cli/pull/43))

- Feat/multi cas ([#42](https://github.com/Koh0920/ato-cli/pull/42))

- Remove sensitive and unnecessary files before public release

- Update to 0.3.8

- Refactor CI workflows and clean up dependencies

- Refactor consent handling and enhance permission management

- Reliability update v0.2.1 final polish

- Update tracked files

- Add untracked files

- Refactor error handling to use CapsuleError across various modules

- Implement CLI reporters for metrics reporting: StdoutReporter and JsonReporter

#### Refactoring

- Streamline ato-cli modules and remove dead code ([#171](https://github.com/Koh0920/ato-cli/pull/171))

- Clean up error handling and improve script documentation

#### Tests

- Clarify capsule SBOM verification assertions

- Verify SBOM embedding in capsule packer

## [0.5.1] - 2026-03-24

### What Changed

#### Bug Fixes

- Unblock clippy on dev

- Satisfy release clippy gates for diagnostics

- Stabilize inferred github source apps

- Support inferred deno github repos

- Harden github install and build fallback flows

- Correct argument passing in infer_source_driver function

#### Features

- Refine error taxonomy diagnostics

- Polish progressive runtime release flow

- Add preview session release candidate

- Support chml manifests and v0.3 runtime flows

- Add v0.3 runtime and capsule lock support

- Enhance Deno command handling and environment setup

- Add service binding scope initialization in test cases

- Implement host-side service binding management and API

- Implement persistent state management and API

- Add persistent state bindings and thin registry

- Add state-first manifest poc schema

- Add platform-specific symlink creation for native delivery

- Implement payload v3 manifest generation and related utilities

- Enhance build process and clean up code across multiple modules

- Refactor and clean up code across multiple modules

- Migrate web deno orchestration to services supervisor mode

- Lockfile-aware SBOM with JCS-canonical metadata and non-blocking generation path ([#40](https://github.com/Koh0920/ato-cli/pull/40))

- Add dynamic app capsule recipe and update symlink handling in packaging

- Add execution_required_envs method and preflight check for required environment variables

- Bump version to 0.3.8 and enhance Deno artifact handling

- Add static file server implementation in Deno

- Add conditional compilation for Unix-specific imports in runtime and IPC modules

- Add config schema for Nacelle runtime configuration

- Finalize local registry, runtime guard, and silent runner UX

- Implement manifest validation for build process and add smoke testing functionality

- Expand runtime and capsule type foundations

- Implement process management functionality

- Update README and add tests for Python, Node, Deno, Bun, and custom app config generation

#### Other Changes

- Bump ato-cli to 0.4.28

- Bump capsule-core to 0.3.0

- Release 0.4.25

- Release ([#155](https://github.com/Koh0920/ato-cli/pull/155))

- Release

- Refactor path handling and validation in manifest and execution plan

- Polish persistent state docs and retries

- Finalize state-first poc validation

- Dev ([#49](https://github.com/Koh0920/ato-cli/pull/49))

- Fix clippy for v0.4.7 release

- Improve build lockfile performance and timings

- Skip unsupported universal lock targets

- Skip unsupported Deno platforms in universal locks

- Regenerate stale universal lockfiles

- Improve lockfile runtime platform detection and expand related tests

- Expand web runtime lockfile platforms

- Feat/multi cas ([#43](https://github.com/Koh0920/ato-cli/pull/43))

- Feat/multi cas ([#42](https://github.com/Koh0920/ato-cli/pull/42))

- Remove sensitive and unnecessary files before public release

- Update to 0.3.8

- Refactor CI workflows and clean up dependencies

- Refactor consent handling and enhance permission management

- Reliability update v0.2.1 final polish

- Update tracked files

- Add untracked files

- Refactor error handling to use CapsuleError across various modules

- Implement CLI reporters for metrics reporting: StdoutReporter and JsonReporter

#### Refactoring

- Streamline ato-cli modules and remove dead code ([#171](https://github.com/Koh0920/ato-cli/pull/171))

- Clean up error handling and improve script documentation

#### Tests

- Clarify capsule SBOM verification assertions

- Verify SBOM embedding in capsule packer

## [0.5.0] - 2026-03-20

### What Changed

#### Features

- Add typed AtoError and AtoExecutionError metadata for phase-aware error transport across execution plan flows

#### Bug Fixes

- Preserve structured details for consent, lockfile, environment, runtime, and manual-intervention failures

## [0.4.0] - 2026-03-18

### What Changed

#### Features

- Add reusable launch spec, lifecycle event, and host isolation primitives for source execution and smoke validation

#### Bug Fixes

- Allow versionless v0.3 manifests while preserving validation for explicit semver values

- Prepare isolated dependency installs and readiness signaling for host-executed source smoke tests

## [0.3.1] - 2026-03-18

### What Changed

#### Features

- Add preview validation and runtime guard modes so preview manifests can be inspected before full lockfile and sandbox enforcement

#### Bug Fixes

- Preserve execution working directory semantics when deriving runtime provisioning commands

## [0.3.0] - 2026-03-17

### What Changed

#### Features

- Preserve `run_command`, `outputs`, and `build_env` on public normalized target types for v0.3 runtime/build metadata

## [0.1.3] - 2026-03-12

### What Changed

#### Bug Fixes

- Correct argument passing in infer_source_driver function

#### Features

- Enhance Deno command handling and environment setup

- Add service binding scope initialization in test cases

- Implement host-side service binding management and API

- Implement persistent state management and API

- Add persistent state bindings and thin registry

- Add state-first manifest poc schema

- Add platform-specific symlink creation for native delivery

- Implement payload v3 manifest generation and related utilities

- Enhance build process and clean up code across multiple modules

- Refactor and clean up code across multiple modules

- Migrate web deno orchestration to services supervisor mode

- Lockfile-aware SBOM with JCS-canonical metadata and non-blocking generation path ([#40](https://github.com/Koh0920/ato-cli/pull/40))

- Add dynamic app capsule recipe and update symlink handling in packaging

- Add execution_required_envs method and preflight check for required environment variables

- Bump version to 0.3.8 and enhance Deno artifact handling

- Add static file server implementation in Deno

- Add conditional compilation for Unix-specific imports in runtime and IPC modules

- Add config schema for Nacelle runtime configuration

- Finalize local registry, runtime guard, and silent runner UX

- Implement manifest validation for build process and add smoke testing functionality

- Expand runtime and capsule type foundations

- Implement process management functionality

- Update README and add tests for Python, Node, Deno, Bun, and custom app config generation

#### Other Changes

- Release ([#155](https://github.com/Koh0920/ato-cli/pull/155))

- Release

- Refactor path handling and validation in manifest and execution plan

- Polish persistent state docs and retries

- Finalize state-first poc validation

- Dev ([#49](https://github.com/Koh0920/ato-cli/pull/49))

- Fix clippy for v0.4.7 release

- Improve build lockfile performance and timings

- Skip unsupported universal lock targets

- Skip unsupported Deno platforms in universal locks

- Regenerate stale universal lockfiles

- Improve lockfile runtime platform detection and expand related tests

- Expand web runtime lockfile platforms

- Feat/multi cas ([#43](https://github.com/Koh0920/ato-cli/pull/43))

- Feat/multi cas ([#42](https://github.com/Koh0920/ato-cli/pull/42))

- Remove sensitive and unnecessary files before public release

- Update to 0.3.8

- Refactor CI workflows and clean up dependencies

- Refactor consent handling and enhance permission management

- Reliability update v0.2.1 final polish

- Update tracked files

- Add untracked files

- Refactor error handling to use CapsuleError across various modules

- Implement CLI reporters for metrics reporting: StdoutReporter and JsonReporter

#### Refactoring

- Clean up error handling and improve script documentation

#### Tests

- Clarify capsule SBOM verification assertions

- Verify SBOM embedding in capsule packer

## [0.1.2] - 2026-03-12

### What Changed

#### Bug Fixes

- Correct argument passing in infer_source_driver function

#### Features

- Enhance Deno command handling and environment setup

- Add service binding scope initialization in test cases

- Implement host-side service binding management and API

- Implement persistent state management and API

- Add persistent state bindings and thin registry

- Add state-first manifest poc schema

- Add platform-specific symlink creation for native delivery

- Implement payload v3 manifest generation and related utilities

- Enhance build process and clean up code across multiple modules

- Refactor and clean up code across multiple modules

- Migrate web deno orchestration to services supervisor mode

- Lockfile-aware SBOM with JCS-canonical metadata and non-blocking generation path ([#40](https://github.com/Koh0920/ato-cli/pull/40))

- Add dynamic app capsule recipe and update symlink handling in packaging

- Add execution_required_envs method and preflight check for required environment variables

- Bump version to 0.3.8 and enhance Deno artifact handling

- Add static file server implementation in Deno

- Add conditional compilation for Unix-specific imports in runtime and IPC modules

- Add config schema for Nacelle runtime configuration

- Finalize local registry, runtime guard, and silent runner UX

- Implement manifest validation for build process and add smoke testing functionality

- Expand runtime and capsule type foundations

- Implement process management functionality

- Update README and add tests for Python, Node, Deno, Bun, and custom app config generation

#### Other Changes

- Release

- Refactor path handling and validation in manifest and execution plan

- Polish persistent state docs and retries

- Finalize state-first poc validation

- Dev ([#49](https://github.com/Koh0920/ato-cli/pull/49))

- Fix clippy for v0.4.7 release

- Improve build lockfile performance and timings

- Skip unsupported universal lock targets

- Skip unsupported Deno platforms in universal locks

- Regenerate stale universal lockfiles

- Improve lockfile runtime platform detection and expand related tests

- Expand web runtime lockfile platforms

- Feat/multi cas ([#43](https://github.com/Koh0920/ato-cli/pull/43))

- Feat/multi cas ([#42](https://github.com/Koh0920/ato-cli/pull/42))

- Remove sensitive and unnecessary files before public release

- Update to 0.3.8

- Refactor CI workflows and clean up dependencies

- Refactor consent handling and enhance permission management

- Reliability update v0.2.1 final polish

- Update tracked files

- Add untracked files

- Refactor error handling to use CapsuleError across various modules

- Implement CLI reporters for metrics reporting: StdoutReporter and JsonReporter

#### Refactoring

- Clean up error handling and improve script documentation

#### Tests

- Clarify capsule SBOM verification assertions

- Verify SBOM embedding in capsule packer

## [0.1.1] - 2026-03-12

### What Changed

#### Bug Fixes

- Correct argument passing in infer_source_driver function

#### Features

- Enhance Deno command handling and environment setup

- Add service binding scope initialization in test cases

- Implement host-side service binding management and API

- Implement persistent state management and API

- Add persistent state bindings and thin registry

- Add state-first manifest poc schema

- Add platform-specific symlink creation for native delivery

- Implement payload v3 manifest generation and related utilities

- Enhance build process and clean up code across multiple modules

- Refactor and clean up code across multiple modules

- Migrate web deno orchestration to services supervisor mode

- Lockfile-aware SBOM with JCS-canonical metadata and non-blocking generation path ([#40](https://github.com/Koh0920/ato-cli/pull/40))

- Add dynamic app capsule recipe and update symlink handling in packaging

- Add execution_required_envs method and preflight check for required environment variables

- Bump version to 0.3.8 and enhance Deno artifact handling

- Add static file server implementation in Deno

- Add conditional compilation for Unix-specific imports in runtime and IPC modules

- Add config schema for Nacelle runtime configuration

- Finalize local registry, runtime guard, and silent runner UX

- Implement manifest validation for build process and add smoke testing functionality

- Expand runtime and capsule type foundations

- Implement process management functionality

- Update README and add tests for Python, Node, Deno, Bun, and custom app config generation

#### Other Changes

- Refactor path handling and validation in manifest and execution plan

- Polish persistent state docs and retries

- Finalize state-first poc validation

- Dev ([#49](https://github.com/Koh0920/ato-cli/pull/49))

- Fix clippy for v0.4.7 release

- Improve build lockfile performance and timings

- Skip unsupported universal lock targets

- Skip unsupported Deno platforms in universal locks

- Regenerate stale universal lockfiles

- Improve lockfile runtime platform detection and expand related tests

- Expand web runtime lockfile platforms

- Feat/multi cas ([#43](https://github.com/Koh0920/ato-cli/pull/43))

- Feat/multi cas ([#42](https://github.com/Koh0920/ato-cli/pull/42))

- Remove sensitive and unnecessary files before public release

- Update to 0.3.8

- Refactor CI workflows and clean up dependencies

- Refactor consent handling and enhance permission management

- Reliability update v0.2.1 final polish

- Update tracked files

- Add untracked files

- Refactor error handling to use CapsuleError across various modules

- Implement CLI reporters for metrics reporting: StdoutReporter and JsonReporter

#### Refactoring

- Clean up error handling and improve script documentation

#### Tests

- Clarify capsule SBOM verification assertions

- Verify SBOM embedding in capsule packer
