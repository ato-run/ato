# Changelog

All notable changes to `ato-cli` will be documented in this file.

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
