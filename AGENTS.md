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

## Core Conventions
- `core/` should avoid direct stdout/stderr writes.
- Keep packers/executors pure and return data + errors.
- Use `capsule_core::engine` for nacelle discovery.

## Testing Notes
- Some tests are gated by `provisioning-tests`.
- Network tests use local mock servers; avoid real network calls.
- Use `tempfile` for filesystem isolation.

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

## Release Process
- For versioned releases, update `Cargo.toml` first on the working branch.
- Send the version bump through a PR into `main`; do not treat a branch-only version bump as releasable.
- After the PR is merged, confirm that `main` contains the intended version string before creating or pushing the release tag.
- Push the release tag only after `main` and the tagged commit are aligned.

## Gotchas
- `ato` delegates to the external `nacelle` binary.
- `ato pack` routes to Source/OCI/WASM via `router` logic.
- `--skip-l1` and `--skip-validation` are dangerous; use sparingly.

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
