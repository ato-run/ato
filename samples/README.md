# samples

This directory stores hand-authored sample projects and fixtures used by smoke tests, import detection, and future compatibility coverage.

## Purpose

- Keep stable, reviewable fixture source trees in the repository.
- Exercise real project shapes without committing machine-local build outputs.
- Provide a predictable place to add future fixtures for web, mobile, wasm, and container flows.

## Scope

Checked-in fixtures may represent source projects for:

- native desktop apps such as Tauri, Electron, and Wails
- web apps and static sites
- mobile apps
- wasm projects
- container and docker-based apps

The fixture tree should describe the minimum source layout needed for detection, init, build bridging, publish, and install smoke coverage.

## Layout

Use family-first directories so new fixture types stay discoverable.

Example layout:

```text
samples/
  native-desktop/
    tauri/
    electron/
    wails/
  web/
  mobile/
  wasm/
  containers/
```

Each fixture directory should be a clean source tree that a contributor can inspect without generated noise.

## Required Rules

- Commit only hand-authored source files and minimal metadata needed for fixture detection.
- Keep fixtures small. Include only files that change detection, lock generation, build bridging, or publish behavior.
- Prefer minimal lockfiles or manifest metadata only when they are required to make the fixture shape valid.
- Write fixtures so tests can copy them into a temporary workspace and generate artifacts there.
- Keep names stable and explicit so test failures are easy to trace back to a fixture.

## Forbidden Files

Do not commit machine-local or generated state under samples.

- dependency installation outputs such as node_modules, vendor, .venv, or build cache directories
- generated artifacts such as dist, build outputs, packaged apps, binaries, images, or archives
- environment-specific files such as .env.local, editor state, or host-specific config
- generated ato state such as ato.lock.json, capsule.lock.json, fetched artifacts, or publish outputs
- test run leftovers such as logs, temp directories, screenshots, or copied registries

If a workflow needs these files, tests must generate them inside a temporary directory at runtime.

## Adding A Fixture

When adding a new fixture:

1. Create the smallest source tree that still triggers the intended product behavior.
2. Put it under the correct family directory.
3. Verify tests materialize the fixture into a temp workspace instead of mutating the checked-in copy.
4. Avoid adding real installed dependencies or generated ato lockfiles.
5. Document any non-obvious fixture constraint in the test that consumes it.

## Maintenance

- Update fixtures when product detection rules or build bridge assumptions change.
- Prefer editing an existing fixture over adding near-duplicates.
- If a fixture must intentionally omit files to preserve cleanliness, keep that omission deliberate and let tests synthesize the missing generated state.