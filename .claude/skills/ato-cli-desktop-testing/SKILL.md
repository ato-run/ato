---
name: ato-cli-desktop-testing
description: "Run hermetic verification for ato-cli and ato-desktop via MCP without polluting the developer's real ~/.ato state. Covers isolated env setup, focused cargo tests, CLI run/session verification, desktop socket discovery checks, and stdio MCP smoke tests."
argument-hint: "Describe the validation target, such as run the ato-cli cache tests, verify desktop MCP discovery, or do a hermetic CLI+desktop smoke test"
user-invocable: true
---

# Ato CLI + Desktop Testing

## What This Skill Does

This skill standardises how to verify `ato-cli` and `ato-desktop` in a hermetic environment.

It is for two classes of checks:

- `ato-cli` focused verification using cargo tests or direct CLI commands
- `ato-desktop` verification through the MCP bridge and its socket discovery path

The default rule is simple: do not test against the developer's real `~/.ato` tree unless the user explicitly asks for that.

## When To Use

Use this skill when the user asks to:

- test `ato-cli`
- verify a CLI bug fix or regression
- verify `ato-desktop` automation or MCP behavior
- run a hermetic desktop smoke test
- confirm `ATO_HOME` isolation behavior
- reproduce a CLI or desktop issue without touching local state

Do not use this skill when:

- the task is purely code review with no execution
- the user explicitly wants real local state involved
- the change is unrelated to CLI or desktop runtime behavior

## Core Rule

Hermetic verification is driven by one root switch: `ATO_HOME`.

Every hermetic verification run must use a fresh environment root by default.
Do not reuse the same `ATO_HOME` across retries, MCP smoke attempts, or manual suites unless you are explicitly debugging state carry-over.

For test runs that should not leak into real user state, isolate all of:

- `ATO_HOME`
- `HOME`
- `XDG_CONFIG_HOME`
- `XDG_CACHE_HOME`

In this repo, the default way to do that is `scripts/ato-test-shell.sh`, which allocates a fresh `.tmp/ato-test-shell/env.XXXXXX` root on every invocation.
If you intentionally need to reuse an existing hermetic root, set both `ATO_TEST_REUSE_ENV_ROOT=1` and `ATO_TEST_ENV_ROOT=<existing-root>` explicitly.

## Fast Path

Open a hermetic shell:

```bash
./scripts/ato-test-shell.sh
```

Print the env block and run a single command:

```bash
./scripts/ato-test-shell.sh --print-env ./target/debug/ato --version
```

Guard against new `HOME -> .ato` regressions before or after a change:

```bash
./scripts/check-ato-home-paths.sh
```

## CLI Verification

### Focused integration tests

Use focused `cargo test` invocations for the touched slice first.

Current hermetic cache / freeze regression set:

```bash
cargo test -p ato-cli --test cache_admin -- --nocapture
cargo test -p ato-cli --test attestation_e2e -- --nocapture
cargo test -p ato-cli --test cache_warm_run_e2e -- --nocapture
cargo test -p ato-cli --test freeze -- --nocapture
cargo test -p ato-cli --test dependency_materializer -- --nocapture
```

Session-start focused regression example:

```bash
cargo test -p ato-cli --test ato_desktop_session_e2e -- --nocapture
```

### Direct CLI smoke tests

Run inside the hermetic shell whenever the command would otherwise mutate `.ato/` state.

Example one-shot run:

```bash
./scripts/ato-test-shell.sh --print-env ./target/debug/ato run --sandbox -y capsule://github.com/Koh0920/WasedaP2P
```

Example session-start check:

```bash
./scripts/ato-test-shell.sh --print-env ./target/debug/ato app session start capsule://github.com/Koh0920/WasedaP2P --json
```

If the target needs secrets or runtime helpers, inject them explicitly in the hermetic env rather than relying on anything persisted under the real home directory.

## Desktop MCP Verification

### Focused desktop tests

These are the first checks for ATO_HOME-aware desktop socket discovery:

```bash
cargo test --manifest-path crates/ato-desktop/Cargo.toml current_instance_file_respects_ato_home_override -- --nocapture
cargo test --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop-mcp discover_socket_uses_ato_home_run_dir -- --nocapture
```

### Hermetic desktop + MCP smoke flow

Desktop and MCP must share the same `ATO_HOME`, and that `ATO_HOME` should belong to the current smoke run only.

Preferred flow: create one fresh shell and launch both processes inside it.

```bash
./scripts/ato-test-shell.sh --print-env

cargo run --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop
cargo run --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop-mcp
```

If you need separate terminal invocations, first allocate one fresh root and then reuse it explicitly for the duration of that single smoke run:

```bash
env_root="$(./scripts/ato-test-shell.sh --print-env true | sed -n 's/^ATO_TEST_ENV_ROOT=//p')"

ATO_TEST_REUSE_ENV_ROOT=1 ATO_TEST_ENV_ROOT="$env_root" \
  ./scripts/ato-test-shell.sh cargo run --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop

ATO_TEST_REUSE_ENV_ROOT=1 ATO_TEST_ENV_ROOT="$env_root" \
  ./scripts/ato-test-shell.sh cargo run --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop-mcp
```

Do not call `./scripts/ato-test-shell.sh` twice without the explicit reuse flags above; each invocation creates a different fresh root by default.

`ato-desktop-mcp` discovers its socket from:

```text
${ATO_HOME}/run/ato-desktop-current.json
```

not from the developer's real `~/.ato/run/`.

### Stdio MCP smoke probe

For a quick transport-level sanity check, pipe a minimal MCP request into the binary:

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  | cargo run --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop-mcp
```

Expected result: a JSON-RPC response with `protocolVersion`, `capabilities`, and `serverInfo`.

## Manual Verification Rules

All scripts under `tests/manual/` already source `tests/manual/config.sh`, which allocates a fresh hermetic env root per suite invocation by default.

Use the default behavior unless the user explicitly wants to test against real local state.

To disable hermetic mode intentionally:

```bash
ATO_TEST_HERMETIC=0 tests/manual/<suite>/test.sh
```

Do not set this casually.

If you intentionally need to rerun against the same hermetic root, opt in explicitly:

```bash
ATO_TEST_REUSE_ENV_ROOT=1 ATO_TEST_ENV_ROOT=/path/to/existing/env tests/manual/<suite>/test.sh
```

## Validation Order

Prefer this order:

1. the narrowest focused cargo test for the touched behavior
2. a hermetic direct CLI command if the behavior is runtime-visible
3. a desktop MCP focused test if socket discovery or automation is involved
4. a hermetic desktop + MCP smoke flow if end-to-end behavior matters
5. the HOME-access guard script before closing the work

## Common Failure Modes

### MCP cannot find the desktop socket

Check:

- desktop and MCP are running with the same `ATO_HOME`
- if they were started from separate commands, the second and later commands used `ATO_TEST_REUSE_ENV_ROOT=1` with the same `ATO_TEST_ENV_ROOT`
- `${ATO_HOME}/run/ato-desktop-current.json` exists
- the desktop process started its automation listener successfully

### A command passes only outside hermetic mode

That usually means the behavior is relying on leaked state from the real machine.

Inspect whether it implicitly depends on:

- saved secrets
- cached runtimes
- persisted trust metadata
- old session files
- desktop WebView profile state

### A new code path reintroduces `HOME -> .ato`

Run:

```bash
./scripts/check-ato-home-paths.sh
```

If it fails, route the path through the canonical ATO path helpers instead of joining `.ato` off `HOME` directly.

## Reporting Back To The User

When you use this skill, report:

- whether the run was hermetic
- whether the hermetic root was fresh or explicitly reused
- which focused tests or commands you executed
- whether the desktop MCP path used `${ATO_HOME}/run`
- any remaining reliance on ad-hoc env setup or real-machine state

Keep the summary short and concrete.
