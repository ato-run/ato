# AGENTS.md - Capsule Development Guidelines

Guidelines for agentic coding assistants working on the Capsule project.

## Computation / Capsule invariants

These invariants govern design, implementation, documentation, and review:

1. **Computation is the semantic center.** It is the evolving residual
   computation, not a repository, manifest, state snapshot, or trace.
2. **Capsule is immutable.** A Capsule is a sealed, addressable Computation
   point—a persistent open continuation.
3. **Run is mutable.** A Run evaluates a Capsule and advances through immutable
   successor Computations. Do not use Run and Capsule interchangeably.
4. **Record is evidence.** Records and Traces describe observed Evolution; they
   are not the current Computation and do not define its identity.
5. **Materialization does not define Capsule identity.** Replay, filesystem or
   source reconstruction, checkpoints, snapshots, containers, and VMs are
   possible realization strategies.
6. **Composition is closed over Computation.** Wiring Computations through
   compatible Ports produces another Computation, not a new semantic root.
7. **Distribution, placement, sandboxing, providers, processes, and VMs are
   realization concerns.** Keep their policy and evidence outside the Semantic
   Core.
8. **Do not introduce a top-level semantic noun** unless it cannot be expressed
   through Computation, Port, Evolution, Composition, Contract, or realization
   concerns.

Practical consequences:

- `capsule.toml` is authoring input, not Capsule identity.
- A `.capsule` file is transport rooted at a `ComputationRef`, not the Capsule
  itself.
- State is a purpose-specific projection of a Computation.
- PortRef is logical and persistent; Binding owns its mapping to a physical
  Endpoint.
- Ready State is a Contract/realization concern, not a universal primitive.
- Prefer one extensible Adapter and Materializer model over workload-specific
  special cases, and remain safe by default at every physical boundary.

## Repository Structure

This directory is the `ato-run/ato` Git repository and a Rust workspace. The
desktop app under `apps/desktop` and the Nacelle provider under
`extensions/providers/nacelle` have separate build boundaries and are excluded
from the root Cargo workspace.

### Git Commit Rules

- Commit per logical change, not per file touched.
- Commit in small, coherent chunks during implementation so progress is saved incrementally.
- Do not hardcode a commit author identity. Use the currently authenticated `gh` user for GitHub operations, and use the current repository/global `git config user.name` and `git config user.email` for local commits. Do not add any `Co-Authored-By` lines.
- Message format: `<scope>(<app>): <what changed>` — e.g., `fix(ato-desktop): guard evaluate_script after PageLoadEvent::Finished`

## Repository layout

```text
apps/
├── cli/                         # lifecycle CLI and supervisor
└── desktop/                     # GPUI + Wry shell; separate workspace
lib/
├── computation/                 # Semantic Core values and identity
├── kernel/                      # Evolution
├── compose/                     # operational composition
├── objects/                     # CAS, Records, lineage, bundle transport
└── ipc/                         # process-boundary DTOs
extensions/
├── adapters/                    # process, PTY, workspace, binding, HTTP
├── materializers/               # replay, snapshot, public API
└── providers/nacelle/           # source runtime and sandbox; separate boundary
services/
├── netd/                        # session network broker
└── ato-tsnetd/                  # Go tailnet sidecar
tools/
├── arch-check/                  # dependency boundary validator
└── snapshot-builder/            # Ready-State implementation tooling
```

## Build/Test/Lint Commands

Hermetic CLI, desktop, MCP, and manual smoke verification must use a fresh env root on every run by default. In this repo, `scripts/ato-test-shell.sh` and `tests/manual/config.sh` should allocate a unique root per invocation; reuse is allowed only as an explicit opt-in with `ATO_TEST_REUSE_ENV_ROOT=1` plus `ATO_TEST_ENV_ROOT=<existing-root>` when debugging state carry-over.

### Root Rust workspace

```bash
# Build
 cargo build --workspace                    # All crates
 cargo build -p ato-cli                     # CLI package

# Test
 cargo test --workspace                     # All tests
 cargo test -p ato-cli test_name            # Single CLI test
 cargo test -p ato-computation --lib test_fn
 cargo test -- --nocapture                 # Show output

# Lint/Format
 cargo fmt --all
 cargo clippy --all-targets --all-features -- -D warnings

# CI Check
 cargo check --workspace && cargo test --workspace --no-fail-fast && cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings
```

### Desktop (GPUI + Wry)

```bash
cd apps/desktop

# Dev
cargo run --bin ato-desktop

# Test
cargo test

# Bundle (macOS)
cargo run --manifest-path xtask/Cargo.toml -- bundle --target darwin-arm64
```

## Branching Model

### Long-lived branches

```
main          — latest stable release only (every commit = a published vX.Y.Z tag)
dev           — normal development integration; next stable candidate
nightly       — 0.7.0 MVP / experimental integration
release/0.6   — 0.6.x maintenance branch
release/0.5   — 0.5.x maintenance branch (limited / security-only support)
```

Future: `release/0.7` is created when 0.7.0 ships; `nightly` then advances to 0.8 work.

### Feature branch naming

Short-lived, one issue per branch: `feat/*`, `fix/*`, `hotfix/*`.

### Development flows

```
0.7 new features:
  feat/0.7-* ──PR──▶ nightly ──▶ dev ──▶ main

0.6 patch:
  fix/0.6-* ──PR──▶ release/0.6 ──▶ main
                               ↘ cherry-pick / forward-port ──▶ dev ──▶ nightly

0.5 patch:
  fix/0.5-* ──PR──▶ release/0.5 ──▶ main
                               ↘ forward-port ──▶ release/0.6 ──▶ dev ──▶ nightly

Urgent hotfix:
  hotfix/* from main ──▶ main
                      ↘ cherry-pick ──▶ affected release branches / dev / nightly
```

**Forward-port rule — always flow fixes oldest → newest:**
`release/0.5` → `release/0.6` → `dev` → `nightly`

Never backport from `nightly` to `release/*`.

### Base branch per work type

| Work type | Base branch |
|-----------|-------------|
| 0.7 new feature / experiment | `nightly` |
| 0.6.x patch or regression fix | `release/0.6` |
| 0.5.x critical / security fix | `release/0.5` |
| Normal dev (non-version-specific) | `dev` |
| Urgent hotfix from stable | `main` |

### Branch protection

| Branch | Policy |
|--------|--------|
| `main` | No direct push. Release PR only. Full CI + manual smoke. |
| `release/0.6` | No direct push. Patch / security / regression only. No new features. |
| `release/0.5` | No direct push. Critical / security only. Limited support. |
| `dev` | Normal integration. CI required. |
| `nightly` | 0.7 experimental. Compile + test required. AODD degraded OK — log reason. |

### Versioning

| Branch | Version scheme |
|--------|----------------|
| `release/0.5` | `v0.5.x` |
| `release/0.6` | `v0.6.x` |
| `nightly` | `v0.7.0-nightly.YYYYMMDD+sha` |
| `dev` | `v0.7.0-dev` (unreleased) |
| `main` | stable tags only (`vX.Y.Z`) |

---

## Development Workflow

### Spec-Driven Development

1. **Always check specs first**: See `docs/rfcs/` before implementing
   - `docs/rfcs/accepted/` — 確定仕様（現行実装の根拠）
   - `docs/rfcs/draft/` — ドラフト仕様（議論中・未確定）
2. **Key specs**:
   - `COMPUTATION_ARCHITECTURE.md` — semantic identity and Evolution
   - `COMPOSITION.md` — closed Computation composition
   - `LOCAL_CAPSULE_REPOSITORY.md` — branches, Runs, Records, and lineage
   - `PROTOCOL_ADAPTER.md` — logical Protocol / physical Adapter boundary
   - `MATERIALIZATION.md` — physical realization boundary
   - `OBJECT_BUNDLE.md` and `CAPSULE_BUNDLE.md` — portable closure format
   - `CAPSULE_CLI_LIFECYCLE.md` — public CLI behavior
3. **Missing specs**: If implementing important logic not in specs, document it as a new RFC in `docs/rfcs/draft/`

### Component Responsibilities

- **lib/computation**: canonical Computation values, Ports, identity, and pure wiring
- **lib/kernel**: payload-opaque Evolution through registered semantics and Protocols
- **lib/compose**: operational composition and closure traversal
- **lib/objects**: verified CAS, Records, lineage, signatures, and bundles
- **lib/ipc**: adjacent process-boundary DTOs, not Semantic Core
- **extensions/adapters**: physical interactions mapped to logical Protocols
- **extensions/materializers**: physical encoding and restoration strategies
- **apps/cli**: lifecycle supervision and product assembly
- **apps/desktop**: separate GPUI + Wry shell (NOT Tauri)
- **extensions/providers/nacelle**: OS-native sandbox and source execution provider
- **services/netd**: session network broker
- **tools/snapshot-builder**: Ready-State implementation tooling, not Capsule identity

### Agent Instructions by App

- Treat this root file as the workspace-wide baseline.
- For `apps/cli`, verify public behavior against
  `docs/rfcs/accepted/CAPSULE_CLI_LIFECYCLE.md` and CLI tests.
- For `extensions/providers/nacelle`, consult its local documentation before
  changing provider or sandbox behavior.
- For `apps/desktop` (GPUI + Wry shell), follow Rust + GPUI patterns; do NOT
  apply Tauri/TypeScript patterns.
- Keep app-specific release flow, semver policy, and test commands in the nearest app-level `AGENTS.md` instead of duplicating operational detail here.

### Semantic core, explicit realization

- Seal immutable Computation identity before treating a point as a Capsule.
- Keep runtime orchestration, physical endpoints, placement, and policy outside
  the Semantic Core.
- Validate at boundaries and persist evidence without folding history into the
  current Computation.

## Code Style (Rust)

### Imports

```rust
use std::collections::HashMap;           // std first
use anyhow::Result;                      // external
use capsule_core::manifest;              // internal last
```

### Naming

- Types: `PascalCase` (e.g., `RuntimeDecision`)
- Functions/vars: `snake_case` (e.g., `route_manifest`)
- Constants: `SCREAMING_SNAKE_CASE`
- Error types: End with `Error`

### Error Handling

```rust
// CLI: anyhow with context
pub fn load(path: &Path) -> Result<Manifest> {
    std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?
}

// Library: thiserror
#[derive(Error, Debug)]
pub enum CapsuleError { ... }
```

### Async

- Use `tokio` consistently
- Prefer `async fn` over manual futures
- Use `tokio::spawn` for concurrency
- Use `#[tokio::test]` for async tests

## Code Style (TypeScript/React)

### Imports

```typescript
import { useState } from "react"; // React/core
import { invoke } from "@tauri-apps/api"; // External
import { useOSState } from "@/hooks/useOSState"; // Internal absolute
```

### Naming

- Components: `PascalCase` (e.g., `HostBridgeFrame`)
- Hooks: `camelCase` with `use` prefix (e.g., `useGuestIpc`)
- Types/Interfaces: `PascalCase` (e.g., `TabState`)

### Error Handling

```typescript
try {
  await invoke("command", { args });
} catch (error) {
  console.error("Failed to execute:", error);
  toast.error(error.message);
}
```

## Architecture Principles

### Semantic classification

Every public concept must be classified as a property of the current
Computation, history/evidence, Protocol interaction, Adapter, Materialization,
or runtime orchestration. Do not add application-, provider-, snapshot-, or
placement-specific semantic roots.

### Security

- No secrets in code/logs
- Use `capsule_core::signing` for verification
- Validate inputs at boundaries
- Principle of least privilege

### Type Safety

- Rust: Strong types over `String`/`Vec<u8>` (e.g., `RuntimeKind`)
- TypeScript: Use Zod for runtime validation
- Prefer `Option<T>` over sentinel values

## Before Committing

```bash
# Root Rust workspace
 cargo fmt --all
 cargo clippy --all-targets --all-features -- -D warnings
 cargo test --workspace
```

## Key Paths

- `~/.ato/config.toml`: CLI configuration
- `.capsule/objects/`: immutable Computation and content objects
- `.capsule/refs/heads/`: mutable branch pointers
- `.capsule/records/`: Evolution evidence
- `.capsule/runs/`: active physical Run metadata
- `capsule.toml`: explicit authoring configuration, never Capsule identity
- `docs/rfcs/`: Architecture specs (accepted/ = confirmed, draft/ = in discussion)
- `samples/`: Example apps

## Troubleshooting

- **Engine Discovery**: Set `NACELLE_PATH` or use `ato engine register`
- **Build Failures**: `cargo clean && cargo build`
- **Desktop ato binary**: Develop ato-desktop against a locally-built CLI by setting `ATO_DESKTOP_ATO_BIN=target/debug/ato` (or `target/release/ato`). Without this, Desktop falls back to `$PATH` resolution which may pick up an older installed release. Desktop logs the resolved binary path on startup.
- **Debug**: Use `tracing`, `RUST_BACKTRACE=1`

## Release Notes

### ato-cli release flow

1. Push/merge changes to `main` (directly or via PR from `dev`).
2. Dispatch release-plz manually to create the version bump PR:
   ```bash
   env -u GH_TOKEN -u GITHUB_TOKEN gh workflow run release-plz.yml --ref main -f command=release-pr
   ```
   The workflow also runs automatically on a weekly Monday schedule (`cron: '0 0 * * 1'`). It does **not** trigger on every `main` push.
3. Wait for the `chore(ato-cli): release vX.Y.Z` PR to open. Monitor checks with `gh pr checks <pr>`.
4. Merge — use `--admin` if branch policy blocks despite green checks:
   ```bash
   env -u GH_TOKEN -u GITHUB_TOKEN gh pr merge <pr> --merge --delete-branch=false --admin
   ```
5. Capture the merge commit SHA: `gh pr view <pr> --json mergeCommit`
6. Wait for the `Security Audit` workflow on the merge commit to pass.
7. Tag the merge commit and push:
   ```bash
   git tag -a vX.Y.Z <merge-sha> -m "ato-cli vX.Y.Z"
   git push origin vX.Y.Z
   ```
8. The tag push triggers `release.yml`, which builds 4-platform artifacts and publishes the GitHub Release.
9. Verify: `gh release view vX.Y.Z --json name,isDraft,publishedAt,assets`

See `docs/ops/RELEASE.md` for the full checklist.

## Temp Files

- NEVER write to `/tmp` or `/var/tmp`.
- Always create a `.tmp/` folder in the current working directory for temporary files.
- Clean up temp files when no longer needed.

## Serena MCP

Serena は、コードベースのシンボルレベルの読み書きを提供する MCP サーバーである。利用可能な場合は、grep/glob/view より Serena のツールを優先して使用すること。

### ツール優先順位

コードを操作・調査する際は以下の順序でツールを選ぶ:

1. **Serena MCP ツール**（`serena-find_symbol`, `serena-find_referencing_symbols`, `serena-replace_symbol_body` 等）— シンボル単位の操作に最優先で使用
2. **LSP ベースのツール**（利用可能な場合）
3. **glob** — ファイルパスのパターン検索
4. **grep** — ファイル内容のテキスト検索
5. **bash** — 上記で対応できない場合のみ

### 主要ツール早見表

| 目的                               | ツール                            |
| ---------------------------------- | --------------------------------- |
| ファイルのシンボル一覧を把握する   | `serena-get_symbols_overview`     |
| 関数・クラス・変数を検索する       | `serena-find_symbol`              |
| シンボルの参照箇所を探す           | `serena-find_referencing_symbols` |
| 関数・メソッド本体を置換する       | `serena-replace_symbol_body`      |
| シンボルの後ろにコードを挿入する   | `serena-insert_after_symbol`      |
| シンボルの前にコードを挿入する     | `serena-insert_before_symbol`     |
| コードベース横断でパターン検索する | `serena-search_for_pattern`       |
| シンボルをリネームする（全体反映） | `serena-rename_symbol`            |
| プロジェクト固有の知識を記録する   | `serena-write_memory`             |

### ルール

- 新しいファイルを触る前に必ず `serena-get_symbols_overview` でシンボル構造を把握する。
- シンボルの移動・リネームは `serena-rename_symbol` を使い、手動での文字列置換は行わない。
- プロジェクト固有の知識（設計上の決定、ファイルの役割等）は `serena-write_memory` に記録する。
- オンボーディング確認は `serena-check_onboarding_performed` で行う。

---

Last updated: 2026-08-17
