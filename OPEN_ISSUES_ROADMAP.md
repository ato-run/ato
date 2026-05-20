# ato-run/ato Open Issues — Priority Roadmap (2026-05-20)

Closed this cycle: #189 (Control Bar GA), #25 (validate needs), #58 (store-local sanitizer), #64 (BYOK sample), #38/#44-#53 (pre-FocusView desktop), #59/#65/#66/#56/#54 (P1 security)

Remaining: 38 open (35 actionable + 3 tracking)

---

## P2 — CLI Bug Fixes (low effort, high impact)

| Issue | Title | Est. |
|-------|-------|------|
| **#197** | convert remaining anyhow::bail! in enforce_sandbox_mode_flags to typed diagnostics | small |
| **#127** | ato run rejects paths under ~/.ato/ with an unhelpful E999 | small |
| **#128** | E999 'install confirmation requires TTY' not discoverable from --help | tiny |
| **#129** | --sandbox opt-in not discoverable from --help | tiny |
| **#146** | GitHub cache resolver misparses repo@sha | small |
| **#147** | drive_sync_async panic-on-Pending | small |
| **#148** | --registry flag unused in preflight | tiny |

---

## P3 — Execution Identity / Graph v0.6.0

| Issue | Title | Label |
|-------|-------|-------|
| **#97** | canonical ExecutionGraphBuilder (Phase 1) | enhancement |
| **#98** | graph canonicalization RFC (Phase 2) | type:rfc |
| **#100** | FilesystemIdentityBuilder (Phase 2) | enhancement |
| **#102** | PolicyIdentityBuilder (Phase 2) | enhancement |
| **#125** | persist ExecutionGraph in SessionRecord (Phase 3) | enhancement |
| **#99** | emit ExecutionReceiptV2 for every run (Phase 4) | enhancement |
| **#149** | ExecutionReceiptV2 acceptance test matrix | enhancement |
| **#118** | publish: pack source recipes | enhancement |
| **#150** | extend ReceiptFailureEnvelope for Desktop/agent | enhancement |
| **#35** | git source_tree_hash | enhancement |
| **#33** | split dependency_derivation_hash vs output_hash (RFC) | type:rfc |
| **#34** | introduce [[capabilities]] open-editor | enhancement |

---

## P4 — Platform / Feature Work

| Issue | Title | Est. |
|-------|-------|------|
| **#115** | nacelle: launch children in own process group | medium |
| **#177** | LEIP: Go runtime not supported | medium |
| **#178** | LEIP: static HTML apps give E105 | medium |
| **#179** | LEIP: Electron apps give unclear diagnostic | medium |
| **#138** | desktop MCP: generic webview operations | large |
| **#168** | desktop: NSWindow child-window spike | small |
| **#174** | desktop: trackpad gestures on Control Bar | medium |
| **#36** | Windows: bash-only PATH export | small |
| **#82** | test infra: env_lock flake | small |

---

## P5 — Tools / Refactoring / Backlog

| Issue | Title |
|-------|-------|
| **#30** | migrate ensure_pnpm → ensure_runtime_tool |
| **#31** | register yarn/bun/deno/uv (Slice B) |
| **#32** | record binary_sha256 in lockfile |
| **#23** | Tier1 source/node lockfile asymmetry |
| **#151** | follow-ups from #142 preflight envelope |
| **#71** | ship provider host tools as nested capsules (RFC) |

---

## ◆ Permanent Tracking (closeしない)

| Issue | Title |
|-------|-------|
| **#74** | v0.6.0 — graph-based core architecture umbrella |
| **#40** | RFC: define what v1.0.0 means |
| **#41** | post-v0.5 roadmap themes |

---

## 推奨着手順

1. **P2 CLI fixes** — 7 issues, まとめて 1 PR。低リスク・高効果
2. **P3 Execution Identity** — 多くが #74 umbrella の下で進行中の作業。PR-3a〜5b の進行状況に合わせて pick
3. **P4 Platform** — #168 (spike) → #174 (gestures) → LEIP 3件 → #138 (MCP)
4. **P5 Backlog** — リソースに余裕があるときに
