# AGENTS.md - Capsule Development Guidelines

Guidelines for agentic coding assistants working on the Capsule project.

## Ato Minimal Philosophy

This philosophy should inform design, implementation, and review decisions throughout the repo.

1. **Everything is a capsule**
   Ato が扱う対象は、app・tool・service を問わず、すべて capsule である。違いはカテゴリではなく、実行契約の違いにすぎない。
2. **Everything runs through the same handle**
   capsule の起動は、できる限り同じ操作面で扱う。特別な対象ごとに別の mental model を増やさない。
3. **Declare first, then materialize**
   まず「何が必要か」を宣言し、次に Ato がそれを環境として展開する。実行は宣言の上に成り立つ。
4. **One boundary, one policy**
   どの capsule も、同じ境界モデルで実行される。workspace、filesystem、network、env、permissions は、対象ごとに別物ではなく共通の政策面で扱う。
5. **Execution is not installation**
   インストール、解決、起動、修復、状態確認は分離されうるが、ユーザーから見える世界では一貫した流れであるべき。
6. **Reuse the model, not special cases**
   個別の Ollama 対応や Desky 対応を増やすのではなく、それらを自然に表現できる共通モデルを先に作る。
7. **State is layered**
   - 宣言: `capsule.toml`
   - 解決結果: `ato.lock.json`
   - 実機状態: local state
   この 3 層を混ぜない。
8. **Safe by default**
   Ato は便利さより先に、境界・再現性・監査可能性を守る。曖昧な自動化より、明示的で安全な実行を優先する。

> **One-sentence version:** Ato は、あらゆるソフトウェアを capsule として宣言し、同じハンドルで安全に展開・実行・修復できるようにするための基盤である。

## Repository Structure

This root directory (`capsuled-dev/`) is **NOT** a git repository. Each app under `apps/` is an independent git repository with its own `.git`, branches, and release cycle. Always `cd` into the specific app directory before running `git` commands. Cross-app changes require separate commits in each repository.

### Git Commit Rules

- Do NOT include `Co-Authored-By` lines in commit messages
- Commit frequently to enable easy rollback — at least once per logical change or phase boundary
- During implementation, make small commits at a reasonable cadence so each coherent chunk is preserved
- Use `koh0920` as the commit author identity; do not add any co-author trailers

## Apps Structure

```
apps/
├── ato-cli/            # Meta-CLI (Rust)
│   ├── src/                # CLI commands (open, pack, ipc, profile, key, etc.)
│   ├── core/src/           # capsule-core library (router, resource, signing, IPC)
│   └── tests/              # CLI integration & E2E tests
├── nacelle/            # Source Runtime Engine (Rust)
│   └── src/                # Sandbox (Landlock/eBPF), execution, supervision
├── ato-desktop/        # Desktop Shell (Rust + GPUI + Wry)
│   └── src/                # GPUI shell, Wry WebView host, bridge, orchestrator
├── desky/              # AI Workspace (Electron + React, Tauri variant)
│   ├── src/                # React frontend (@assistant-ui/react)
│   ├── electron/           # Electron main process
│   └── src-tauri/          # Tauri variant backend
├── sync-rs/            # .sync Archive Rust Workspace
│   └── crates/             # sync-format, sync-runtime, sync-fs, sync-wasm-engine
├── uarc/               # UARC Spec & JSON Schema (Single Source of Truth)
│   └── schemas/            # capsule.schema.json (v0.2)
├── ato-api/          # Store API (Cloudflare Workers + Hono + D1 + R2)
│   └── src/                # Routes, services, DB schema (Drizzle)
├── ato-web/      # Store Web GUI (Astro + Cloudflare Pages)
│   └── src/                # Catalog, publisher console, dock UI
├── ato-play-edge/      # Playground Data Plane Worker (*.atousercontent.com)
│   └── src/                # Artifact serving, CSP injection, OpenAI proxy relay
├── ato-play-web/       # Playground Theater UI (React + Vite)
│   └── src/                # Launchpad, Theater, iframe postMessage bridge
├── ato-proxy-edge/     # Proxy Edge Worker (proxy.ato.run)
│   └── src/                # TVM JWT verification, API key swapping, OpenAI relay
├── ato-docs/           # Documentation Site (Astro + Starlight)
│   └── src/                # MDX doc content, custom components
└── ato-tsnetd/         # Tailnet Sidecar (Go + tsnet + gRPC + SOCKS5)
```

## Build/Test/Lint Commands

### Rust (ato-cli, nacelle)

```bash
# Build
 cargo build --workspace                    # All crates
 cargo build -p ato-cli                 # Single crate
 cargo build --release -p nacelle          # Release

# Test
 cargo test --workspace                     # All tests
 cargo test -p ato-cli test_name       # Single test
 cargo test -p capsule-core --lib test_fn  # Library test
 cargo test -- --nocapture                 # Show output

# Lint/Format
 cargo fmt --all
 cargo clippy --all-targets --all-features -- -D warnings

# CI Check
 cargo check --workspace && cargo test --workspace --no-fail-fast && cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings
```

### ato-desktop (GPUI + Wry)

```bash
cd apps/ato-desktop

# Dev
cargo run --bin ato-desktop

# Test
cargo test

# Bundle (macOS)
cargo run --manifest-path xtask/Cargo.toml -- bundle --target darwin-arm64
```

### Web / Workers Build & Deploy

#### ato-web (Astro + Cloudflare Pages)

```bash
cd apps/ato-web
pnpm install

# Build
pnpm build                 # default build
pnpm build:staging         # staging env build
pnpm build:production      # production env build

# Deploy
pnpm deploy:staging        # deploy to staging Pages project
pnpm deploy:production     # deploy to production Pages project
```

#### ato-api (Store API Worker)

```bash
cd apps/ato-api

# Deploy
npx wrangler deploy --env staging
npx wrangler deploy --env production
```

#### ato-play-edge / ato-play-web

```bash
# Edge worker
cd apps/ato-play-edge
npx wrangler deploy --env staging
npx wrangler deploy --env production

# Web frontend
cd apps/ato-play-web
pnpm build
npx wrangler deploy --env staging
npx wrangler deploy --env production
```

#### ato-proxy-edge (Proxy Worker)

```bash
cd apps/ato-proxy-edge
pnpm install
pnpm dev
npx wrangler deploy --env staging
npx wrangler deploy --env production
```

#### Post-deploy Quick Checks

```bash
# store-web
curl -sI https://staging.ato.run | head -n 5
curl -sI https://ato.run | head -n 5

# store api
curl -s https://staging.api.ato.run/v1/capsules?limit=1 | head -c 500
curl -s https://api.ato.run/v1/capsules?limit=1 | head -c 500
```

## Development Workflow

### Spec-Driven Development

1. **Always check specs first**: See `docs/rfcs/` before implementing
   - `docs/rfcs/accepted/` — 確定仕様（現行実装の根拠）
   - `docs/rfcs/draft/` — ドラフト仕様（議論中・未確定）
2. **Key specs**:
   - `ATO_CLI_SPEC.md` - CLI commands & behavior
   - `NACELLE_SPEC.md` - Runtime & sandbox
   - `DRAFT_LIFECYCLE.md` - Task/Service lifecycle (draft)
   - `DRAFT_CAPSULE_IPC.md` - IPC protocol (draft)
3. **Missing specs**: If implementing important logic not in specs, document it as a new RFC in `docs/rfcs/draft/`

### Component Responsibilities

- **ato-cli**: Meta-CLI, runtime routing, metering, IPC broker, orchestration
- **nacelle**: OS-native isolation (Landlock, eBPF), source execution engine
- **ato-desktop**: Desktop shell via GPUI + Wry WebView (NOT Tauri), capsule host, bridge
- **desky**: Local AI workspace (multi-agent), Electron/Tauri, React + @assistant-ui
- **sync-rs**: `.sync` archive format library (sync-format, sync-runtime, sync-fs, sync-wasm-engine)
- **uarc**: UARC manifest spec and JSON Schema (`capsule.toml` v0.2 contract)
- **ato-api**: Store/registry API backend (Cloudflare Workers + Hono + D1 + R2)
- **ato-web**: Store web frontend, publisher console, dock UI (Astro)
- **ato-play-edge**: Playground data plane (`*.atousercontent.com`), artifact serving
- **ato-play-web**: Playground theater UI (`play.ato.run`), iframe bridge
- **ato-proxy-edge**: Proxy worker (`proxy.ato.run`), TVM JWT, API key swap, OpenAI relay
- **ato-docs**: Public documentation site (Astro + Starlight)
- **ato-tsnetd**: Tailnet sidecar (Go + tsnet), SOCKS5, gRPC

### Agent Instructions by App

- Treat this root file as the workspace-wide baseline.
- For `apps/ato-cli`, always read and follow `apps/ato-cli/AGENTS.md` before editing code, tests, CI, or release metadata.
- For `apps/nacelle`, consult `apps/nacelle/docs/` and `docs/rfcs/accepted/NACELLE_SPEC.md`.
- For `apps/ato-desktop` (GPUI + Wry shell), follow the Rust + GPUI patterns; do NOT apply Tauri/TypeScript patterns here.
- For `apps/desky` (Electron/Tauri AI workspace), follow the React + TypeScript patterns in the codebase.
- Keep app-specific release flow, semver policy, and test commands in the nearest app-level `AGENTS.md` instead of duplicating operational detail here.

### Smart Build, Dumb Runtime

- Build-time: Validate manifests, resolve dependencies, compute configs
- Runtime: Minimal logic, pre-computed configs via JSON over stdio

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

### Runtime Selection (router.rs)

1. OCI: `targets.oci.image` or `execution.runtime=oci`
2. Wasm: `targets.wasm` or `*.wasm` entrypoint
3. Source: Default fallback

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
# Rust (ato-cli, nacelle, ato-desktop, sync-rs)
 cargo fmt --all
 cargo clippy --all-targets --all-features -- -D warnings
 cargo test --workspace

# TypeScript (desky, ato-api, ato-web, ato-play-*)
 pnpm lint
 pnpm test
```

## Key Paths

- `~/.ato/config.toml`: CLI configuration
- `~/.ato/store/`: Installed capsules
- `~/.ato/keys/`: Signing keys
- `~/.ato/runtimes/`: Runtime binaries
- `capsule.toml`: Project manifest (spec: `apps/uarc/`)
- `docs/rfcs/`: Architecture specs (accepted/ = confirmed, draft/ = in discussion)
- `samples/`: Example apps

## Troubleshooting

- **Engine Discovery**: Set `NACELLE_PATH` or use `ato engine register`
- **Build Failures**: `cargo clean && cargo build`
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

See `RELEASE.md` for the full checklist.

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

| 目的 | ツール |
|------|--------|
| ファイルのシンボル一覧を把握する | `serena-get_symbols_overview` |
| 関数・クラス・変数を検索する | `serena-find_symbol` |
| シンボルの参照箇所を探す | `serena-find_referencing_symbols` |
| 関数・メソッド本体を置換する | `serena-replace_symbol_body` |
| シンボルの後ろにコードを挿入する | `serena-insert_after_symbol` |
| シンボルの前にコードを挿入する | `serena-insert_before_symbol` |
| コードベース横断でパターン検索する | `serena-search_for_pattern` |
| シンボルをリネームする（全体反映） | `serena-rename_symbol` |
| プロジェクト固有の知識を記録する | `serena-write_memory` |

### ルール

- 新しいファイルを触る前に必ず `serena-get_symbols_overview` でシンボル構造を把握する。
- シンボルの移動・リネームは `serena-rename_symbol` を使い、手動での文字列置換は行わない。
- プロジェクト固有の知識（設計上の決定、ファイルの役割等）は `serena-write_memory` に記録する。
- オンボーディング確認は `serena-check_onboarding_performed` で行う。

---

Last updated: 2026-04-23
