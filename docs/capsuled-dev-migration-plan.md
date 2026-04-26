# capsuled-dev → ato monorepo migration plan

**Status (2026-04-26):** round 1 (P1–P6a) landed. P6b + P7 deferred to round 2 (after v0.5.0 release verification).

This document is the companion to [`monorepo-consolidation-plan.md`](monorepo-consolidation-plan.md). The N1–N4 work in that plan extracted `capsule-wire` from `capsule-core` *inside* the `ato` repo; this plan covers the **outer** migration of sibling apps from the `capsuled-dev/` parent superproject into the same monorepo.

`capsuled-dev/` itself is **not a git repository** — each app under `apps/*` has its own `.git` and release cycle (see `capsuled-dev/AGENTS.md`). So "migration" here means **subtree-merging selected apps into the `ato` monorepo, preserving history**, then archiving the source repos.

## 1. Scope policy (sealed)

User directive (2026-04-26): *"web/api などはまだ同じレポジトリに入れず別管理にする"*. Combined with this round's actual coverage, the four-bucket classification is:

| Bucket | Apps | Disposition |
|---|---|---|
| **A. Already in the monorepo** | `ato-cli`, `ato-desktop` | Landed via M1 (subtree merge), evolved through M2–M6 + N1–N4. Legacy mirrors at `ato-run/ato-cli` and `ato-run/shiny-disco` will be archived in **P7 (round 2)**. |
| **B. Migrated in round 1 (this doc)** | `nacelle` | Sibling sandbox runtime, invoked by `ato-cli` via process spawn (`--nacelle <path>`). Subtree-merged at `crates/nacelle/`. |
| **C. Permanently out of scope** | `ato-api`, `ato-web`, `ato-docs`, `ato-play-edge`, `ato-play-web`, `ato-proxy-edge`, `sync-rs`, `ato-tsnetd`, `desky`, `ato-search-skill` | See §2 for per-app reasoning. **Sealed** — do not re-litigate without an explicit user override. |
| **D. Artifact-distribution shims** | `packages/lock-draft-wasm` | Stays in `capsuled-dev/packages/` for ato-api consumption. The TS source-of-truth already lives at `crates/ato-cli/lock-draft-engine/index.ts` in the monorepo; the shim is a 1-line re-export plus WASM build outputs. Repointing the shim path is a P7 follow-up (round 2). |

## 2. Out-of-scope reasoning (sealed)

Each row is permanent unless an explicit user reversal is recorded against this doc.

| App | Why it stays out |
|---|---|
| `ato-api`, `ato-web`, `ato-docs`, `ato-play-edge`, `ato-play-web`, `ato-proxy-edge` | User directive: web/api stays in separate repos. These are Cloudflare Workers / Astro / React surfaces with their own deploy cadence (`wrangler deploy`, Pages). Lifting them would mix Cargo-workspace and pnpm-workspace lifecycles for no benefit. |
| `sync-rs` | Self-contained 4-crate Rust workspace (`sync-format`, `sync-runtime`, `sync-fs`, `sync-wasm-engine`) with **zero current callers** in `ato/crates/`. The `repository = "https://github.com/anomalyco/sync-rs"` field in its workspace `Cargo.toml` also means migrating implies an org-level move that's its own decision. **Permanently out of scope** for the ato monorepo (user-confirmed 2026-04-26). |
| `ato-tsnetd` | Go (`go.mod`, gRPC, tsnet). Cargo workspace cannot host it; absorbing would require a polyglot top-level layout (`services/`, `go/`) that's a separate architectural decision. |
| `desky` | Electron + React + Tauri-variant. **A consumer** of the ato runtime, not part of it. Belongs on the user-app side of the boundary. |
| `ato-search-skill` | Pure docs + JSONL examples (no compiled code). Independently published as a "skill". Folding it into `docs/` would conflate library docs with a published artifact. |

## 3. Round 1 phases (landed 2026-04-26)

Author for all commits: `Koh0920 <gjiangshang55@gmail.com>` (sole). No `Co-Authored-By` trailers per `AGENTS.md`.

| Phase | Action | Commit(s) |
|---|---|---|
| **P1.1** | `apps/ato-cli@490efb5` (WiX authors + template) — already independently applied during M6 release-CI lift. **No-op.** | — |
| **P1.2** | `apps/ato-desktop` cherry-picks: 5 candidates verified, `fd5bbe9` already present in monorepo's `desktop-release.yml` matrix (no-op), remaining 4 applied. | `0864e5c`, `bf40777`, `034edfb`, `57fc0f7` |
| **P1.3** | `cargo check --workspace` — clean. | (verification only) |
| **P2** | Subtree merge of `nacelle.git` (history preserved, no `--squash`) → `crates/nacelle/`; workspace `members` updated; nested `Cargo.lock` removed; root `Cargo.lock` resolved for the merged graph; nacelle's local `v0.*` tags scrubbed from monorepo's tag namespace before push to avoid colliding with ato's own future versioning. | `c2c1b2c` (subtree), `9f66f68` (wire-in) |
| **P3** | Extended `.github/workflows/dep-direction.yml` with R5 (ato-cli ⊄ nacelle) and R6 (nacelle ⊄ workspace crates), enforcing the process-spawn boundary in CI. | `f6d8047` |
| **P4** | *Decided not to do.* Inspected ato-cli↔nacelle IPC: it's a JSON event stream over stdout, no Rust types are co-owned across the boundary. Hoisting non-shared types into `capsule-wire` would dilute the "wire = shared contract" invariant from N4. Recorded as an explicit non-action. | (this doc) |
| **P5** | *Skipped for round 1.* `packages/lock-draft-wasm/` is actively imported by `ato-api` (out-of-scope) via `vitest.config.ts` alias and direct TS imports. Repointing or absorbing the shim now would couple this round to ato-api work. Deferred to P7. | — |
| **P6a** | Removed dead workflow copies under `crates/nacelle/.github/workflows/` (Actions only reads root `.github/workflows/`; nested copies are unreachable). | `11798bc` |

## 4. Deferred to round 2

| Phase | Action | Why deferred |
|---|---|---|
| **P6b** | True release-CI consolidation: lift `nacelle/release.yml` → root `.github/workflows/nacelle-release.yml` with (a) tag-prefix `nacelle-v*.*.*` to avoid colliding with ato's `v*.*.*` and ato-desktop's release tags, (b) working-directory rewrites for `Cargo.toml` reads, (c) `crates/nacelle/scripts/upload_release_to_r2.sh` path lift, (d) version-extraction logic that handles the prefixed tag form. | The tag-namespace design touches all three release pipelines (ato, ato-desktop, nacelle) and is best done as part of v0.5.0 release prep, not bolted onto a migration round. |
| **P7** | Archive `ato-run/ato-cli`, `ato-run/shiny-disco` (= ato-desktop), `ato-run/nacelle` on GitHub. Update each archived repo's README to redirect to `ato-run/ato`. Then repoint `packages/lock-draft-wasm/index.ts` from `apps/ato-cli/...` to `apps/ato/crates/ato-cli/...` so ato-api keeps working after the legacy mirrors disappear. | Archive happens **after** v0.5.0 ships and the monorepo's release pipeline (P6b) is verified end-to-end. Never archive a live source. |

## 5. Boundary invariants (round-1 outcome)

These are now enforced by `.github/workflows/dep-direction.yml`:

```
capsule-wire ──► (no deps on workspace crates; DAG root)
                       │
                       ├─► capsule-core
                       │       │
                       │       ├─► ato-cli  ──spawn──► nacelle
                       │       └─► ato-desktop ──spawn──► ato (binary)
                       │
                       └─► ato-cli, ato-desktop directly

nacelle ──► (no deps on workspace crates; sandbox leaf)
```

- `capsule-wire` stays a clean DAG root with no GUI / runtime / heavy-network dependencies (R1, R2)
- `ato-desktop` does not link `ato-cli` (R3)
- `ato-cli` does not link `ato-desktop` or any GPUI/Wry crate (R4)
- `ato-cli` does not link `nacelle` (R5, **NEW in P3**)
- `nacelle` does not link any workspace crate (R6, **NEW in P3**)

## 6. Verification commands

```bash
# Local replay of the CI lint
bash -c '
  for p in capsule-wire ato-cli nacelle ato-desktop; do
    echo "=== $p ==="
    cargo tree -p "$p" --prefix=none --edges=normal,build,dev | awk "{print \$1}" | sort -u | head -20
  done
'

# Workspace build (should be clean save for pre-existing warnings)
cargo check --workspace --all-targets
```
