# AGENTS.md

## Purpose

- This file guides agentic coding assistants in this repo.
- Applies to the workspace root and subdirectories.
- Follow existing patterns unless instructed otherwise.

## Repository Layout

- `src/` contains the CLI binary (`ato`).
- `core/` is the `capsule-core` library crate.
- `core/src/` holds packers, router, runtime, and resource logic.
- `core/src/resource/` contains artifact/cas/storage logic.
- `tests/` contains Rust integration and E2E coverage for CLI/runtime flows.
- `.github/workflows/` contains the release, security, parity, and multi-OS build pipelines that gate releases.
- `README.md` documents end-user flows and CLI usage.

## Build Commands

- `cargo build` builds the workspace.
- `cargo build -p ato-cli` builds the CLI only.
- `cargo build -p capsule-core` builds the core library.
- `cargo build --features manifest-signing` enables legacy Cap'n Proto signing (deprecated; JCS is canonical for `.capsule` v2).
- `cargo build -p capsule-core --features manifest-signing` builds core with legacy signing enabled.

## Run / Usage

- `cargo run -- <args>` runs the CLI in debug.
- `./target/debug/ato <args>` runs the built binary directly.

## Lint / Format

- `cargo fmt` formats all Rust code.
- `cargo fmt --all -- --check` checks formatting in CI mode.
- `cargo clippy --workspace --all-targets --all-features` runs full lint.
- `cargo clippy -p ato-cli --all-targets` lints the CLI only.

## Tests (Workspace)

- `cargo test` runs default tests in the workspace.
- `cargo test -p ato-cli` runs CLI tests only.
- `cargo test -p capsule-core` runs core tests only.
- `cargo test --workspace --all-features` runs all tests with features.

## E2E / Regression Test Commands

- `cargo test -p ato-cli --test native_delivery_e2e e2e_native_delivery_windows_build_publish_install_run -- --test-threads=1 --nocapture` runs the Windows native delivery E2E used by `Native Delivery E2E (Windows Only)`.
- `cargo test -p ato-cli --test validate_e2e -- --nocapture` runs manifest and validation end-to-end checks.
- `cargo test -p ato-cli --test local_registry_e2e -- --nocapture` exercises local registry publish/install flows.
- `cargo test -p ato-cli --test oci_orchestration_e2e -- --nocapture` covers OCI orchestration behavior.
- `cargo test -p ato-cli --bin ato normalize_github_install_preview_toml_includes_deno_import_map -- --nocapture` is the focused regression test for inferred GitHub Deno packaging.
- `cargo test -p ato-cli --bin ato run_command_spec_resolves_deno_task_from_source_dir_config -- --nocapture` is the focused regression test for installed source/deno `run_command` execution.
- `cargo test -p ato-cli --bin ato run_command_spec_resolves_deno_task_entrypoint -- --nocapture` checks direct `deno task` resolution.
- Shell E2E helpers live in `tests/cli_validation_e2e.sh`, `tests/e2e_zero_config.sh`, `tests/e2e-test.sh`, and `tests/pack_sign_e2e.sh`.

## Single Test Examples

- `cargo test -p capsule-core test_registry_parsing`
- `cargo test -p capsule-core --features provisioning-tests test_registry_parsing`
- `cargo test -p ato-cli <test_name>`
- `cargo test -p capsule-core --test <integration_test>` (if added)

## Feature Flags

- `manifest-signing` enables legacy Cap'n Proto signing support (deprecated; JCS is canonical for `.capsule` v2).
- `provisioning-tests` enables networked artifact tests (see `core/src/resource/artifact/tests.rs`).

## Code Style (Rust)

- Use Rust 2021 idioms and standard library types.
- Run `cargo fmt` before finishing changes.
- Keep functions small and focused on single tasks.
- Prefer explicit types in public APIs and complex logic.
- Avoid unnecessary clones; pass references when possible.

## Imports

- Group imports by `std`, external crates, then internal crates.
- Separate groups with a blank line.
- Avoid glob imports except for prelude-style modules.
- Keep imports alphabetized within a group when practical.

## Formatting

- Use rustfmt defaults; avoid manual alignment tweaks.
- Use trailing commas in multi-line structs, enums, and arrays.
- Let rustfmt manage line wrapping for long expressions.

## Naming

- Modules/files: `snake_case`.
- Types/traits/enums: `PascalCase`.
- Functions/variables: `snake_case`.
- Constants: `SCREAMING_SNAKE_CASE`.
- Error enums end with `Error` or domain-specific names.

## Types and Ownership

- Use `PathBuf` for owned paths and `&Path` for borrows.
- Pass references for shared data; clone only when required.
- Prefer `Option<T>` over sentinel values.
- Use `Arc` when sharing reporters/clients across threads.

## Error Handling

- In `core/`, return `Result<T, CapsuleError>` from `core/src/error.rs`.
- Use `thiserror` for structured errors and transparent conversions.
- Convert external errors with `?` and `From` implementations.
- In CLI, use `anyhow::Result` with `Context` for user-facing errors.
- Avoid `unwrap`/`expect` outside tests.

## Async / Tokio

- Use `tokio` runtime for async operations.
- Prefer `tokio::fs` for async filesystem work.
- Avoid blocking in async tasks; use `spawn_blocking` if needed.
- Use `#[tokio::test]` for async tests.

## Reporting / Logging

- Use `CapsuleReporter` for user-visible output.
- Prefer structured logging with `tracing` for diagnostics.
- Respect `--json` mode in CLI output.

## CLI Conventions

- Define new commands in `src/main.rs` using `clap` derives.
- Implement command logic in dedicated modules under `src/`.
- Use `PathBuf` for CLI path arguments.
- Keep parsing and business logic separated.

## Current ato-cli Behavior

- `ato run github.com/<owner>/<repo>` is the canonical GitHub shorthand accepted by `parse_github_run_ref`; do not document or normalize `https://github.com/...` inputs as valid CLI run syntax.
- GitHub store-draft installs may return schema v0.3 manifests with `run = "..."`. For driver-specific runtimes such as Deno, do not short-circuit these targets through the generic shell executor.
- Inferred Deno GitHub apps must preserve `run_command`, resolve `deno task <name>` from `deno.json`, and execute through `src/executors/deno.rs`.
- Installed source/deno capsules unpack payload files under `source/`; Deno task resolution must look for `deno.json` both at the manifest root and under `source/`.
- If `deno.json` references `importMap`, the referenced file must be included in generated `pack.include`; otherwise inferred GitHub Deno capsules will build but fail at runtime.
- `src/commands/open.rs` only routes `run_command` targets through `src/executors/shell.rs` for generic shell-native flows. Specialized drivers (`deno`, `node`, `python`) must continue into their dedicated executors.
- The hidden `--keep-failed-artifacts` flag is available on `run` and `install` for GitHub inference debugging. Use the current workspace binary, typically `cargo run -- run ... --keep-failed-artifacts`, when inspecting generated manifests and failed checkouts.

## Core Conventions

- `core/` should avoid direct stdout/stderr writes.
- Keep packers/executors pure and return data + errors.
- Use `capsule_core::engine` for nacelle discovery.

## Testing Notes

- Some tests are gated by `provisioning-tests`.
- Network tests use local mock servers; avoid real network calls.
- Use `tempfile` for filesystem isolation.
- `cargo test -p ato-cli` is the release-candidate baseline and should pass before pushing version bumps.
- Windows native delivery E2E is intentionally serialized with `--test-threads=1`.
- If a full suite failure is flaky but a focused test passes, confirm whether the failure is caused by shared process-global state such as environment variables before changing product code.

## Dependencies

- Prefer existing crates (`anyhow`, `thiserror`, `tokio`, `serde`).
- Add dependencies to the correct `Cargo.toml` only.
- Keep `reqwest` without default features (uses `rustls-tls`).

## Security / Validation

- Preserve L1 policy scan behavior; avoid bypasses without flags.
- Keep manifest validation errors actionable for users.
- Validate external binaries (nacelle) before use.

## Files to Avoid Editing

- `src/capsule_capnp.rs` is generated; update via schema tooling.

## Cursor / Copilot Rules

- No `.cursor/rules`, `.cursorrules`, or Copilot instructions found.

## Documentation

- Keep user docs in `README.md`; update only if behavior changes.
- Avoid creating new docs unless explicitly requested.

## Commit Guidance

- Keep commits focused; mention relevant feature flags.
- Run format and targeted tests when possible.

## CI / Workflow Map

- `Build (Multi OS)` runs on pushes to `dev` and `main`, plus `workflow_dispatch`. It enforces `Cargo.lock`, runs clippy, builds release binaries for Linux/macOS/Windows, and runs smoke checks.
- `Security Audit` runs `cargo-audit`, `cargo-deny`, and `cargo-semver-checks (capsule-core)`. The semver check runs on pull requests, `workflow_dispatch`, and pushes to `main`.
- `V3 Parity Matrix` covers parity checks that should stay green across release bumps.
- `Release PR` is `release-plz` on pushes to `main`; it prepares follow-up release PRs and does not publish the just-merged version.
- `Release` in pull request mode validates cargo-dist plans. `Release` in tag mode publishes release artifacts and the GitHub Release.
- `Native Delivery E2E (Windows Only)` is a manual workflow for the heavy Windows native-delivery path.

## Release Process

- Do release prep on `dev`, not directly on `main`.
- Bump `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md` in `ato-cli`, and also bump `core/Cargo.toml` and `core/CHANGELOG.md` when `capsule-core` public API changes.
- Treat `capsule-core` as semver-sensitive public API. In `0.x`, adding public struct fields or other breaking API changes requires a minor bump, not a patch bump.
- Push the release candidate to `dev` and wait for `Build (Multi OS)`, `Security Audit`, `V3 Parity Matrix`, and secret scan to go green before merging.
- Merge the release PR from `dev` into `main` only after all required checks succeed.
- After merge, wait for the `main` push workflows (`Build (Multi OS)`, `Security Audit`, `V3 Parity Matrix`, `Release PR`, secret scan) to finish green.
- Create the annotated tag on the green `main` merge commit, for example `git tag -a v0.4.25 <main-merge-sha> -m "ato-cli v0.4.25"` followed by `git push origin v0.4.25`.
- The pushed version tag is what triggers `apps/ato-cli/.github/workflows/release.yml` to publish artifacts and create/update the GitHub Release.

## Release Monitoring Commands

- `gh pr checks <pr-number>` gives the fastest summary of whether a release PR is blocked.
- `gh pr view <pr-number> --json mergeable,mergeStateStatus,statusCheckRollup,url` is the detailed PR gate inspection command.
- `gh run list --branch dev --limit 10 --json databaseId,headSha,status,conclusion,workflowName,displayTitle,url` monitors release-candidate runs on `dev`.
- `gh run list --branch main --limit 10 --json databaseId,headSha,status,conclusion,workflowName,displayTitle,url` monitors post-merge `main` runs before tagging.
- `gh run list --commit <sha> --json databaseId,status,conclusion,workflowName,url` is the cleanest way to track one pushed release candidate commit.
- `gh run view <run-id> --json status,conclusion,url,jobs` shows whether a long-running workflow is blocked in `build-local-artifacts`, `build-global-artifacts`, or `host`.
- `gh run view <run-id> --job <job-id> --log | tail -n 200` is the quickest way to inspect a failing CI job, especially `cargo-semver-checks (capsule-core)`.
- `gh release view v<version> --json name,tagName,isDraft,isPrerelease,url,assets` confirms the final published release and asset set.
- A practical release sequence is: check PR, check `dev` runs, merge to `main`, check `main` runs, tag, then watch the `Release` workflow and `gh release view` until the release is published.

## Gotchas

- `ato` delegates to the external `nacelle` binary.
- `ato pack` routes to Source/OCI/WASM via `router` logic.
- `--skip-l1` and `--skip-validation` are dangerous; use sparingly.
- `gh pr merge` may still report branch policy blocking even when all checks are green. In this repo, administrator merge may be required when auto-merge is disabled by repository settings.
- `Release PR` success on `main` does not mean the current version has been published. Publication starts only after the version tag is pushed.
- The release workflow can create the GitHub Release shell before all assets finish uploading, so keep checking both the workflow and `gh release view` until assets appear.

## Temp Files

- NEVER write to `/tmp` or `/var/tmp`.
- Always create a `.tmp/` folder in the current working directory for temporary files.
- Clean up temp files when no longer needed.

## Useful Paths

- `~/.ato/config.toml` holds engine registrations.
- `capsule.toml` is the project manifest used by the CLI.
- `.capsuleignore` controls bundle inclusion.

## When in Doubt

- Follow existing module patterns.
- Prefer explicitness over cleverness.
- Ask maintainers if behavior is ambiguous.
