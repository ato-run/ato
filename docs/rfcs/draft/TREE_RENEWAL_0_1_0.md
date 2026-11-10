# Tree renewal toward 0.1.0 — inventory and proposed dispositions

Status: draft (proposal, nothing removed yet)
Tracking: TODO #223, step 3
Base: `origin/dev` at the time of writing (post 2026-11-10 identity rewrite)

## Context

The 0.1.0 line replaces the active tree through ordinary PRs into `dev`, while the
repository URL, Issues, PRs and history stay. A full pre-renewal copy is preserved
in <https://github.com/ato-run/ato-archive-2026-11> (mirror bundle, GitHub
metadata, release assets, RFC 3161 timestamp). Removing a path from the active
tree therefore does not lose it; it stays in history and in the archive.

Renewal PRs are split by kind, in this order:

1. **Structure** — this inventory; moves that keep behaviour.
2. **Legacy removal** — paths marked *remove* below, after their live references
   are resolved in the same PR.
3. **Specification / compatibility** — M1–M6 in
   `DOCUMENTATION_AUTHORITY_MISMATCHES.md`, each decided in its own ADR. Documents
   are not silently rewritten to match code.
4. **Data migration** — state continuity per `docs/concepts/state-continuity.md`.
5. **Distribution** — version 0.1.0, new MSI `upgrade-guid`, Homebrew / install.sh
   / Latest checks. Wire versions and digests are not reset.

## Inventory (tracked files, last change)

| Path | Files | Last change | Live refs from outside | Proposed |
|---|---:|---|---:|---|
| `apps/cli`, `apps/local-runtime`, workers, `apps/portable-application`, `apps/runtime-object-graph` | — | 2026-11 | — | keep |
| `lib/*`, `services/*`, `extensions/*` (workspace members) | 521 | 2026-11 | — | keep |
| `apps/desktop` (legacy launcher shim + xtask) | 66 | — | 16 | **decide**: current Desktop is the separate `ato-desktop` repo; `desktop-release.yml` and `release-dry-run.yml` still build it |
| `apps/desktop-tauri` | 16 | — | 3 | **decide** (same as above; has `desktop-tauri-ci.yml`) |
| `docs/archive` | 488 | 2026-10-28 | 11 | remove (preserved in archive repo); fix README/AGENTS/CONTRIBUTING/docs-ci/docs-audit refs |
| `docs/aodd` | 86 | 2026-06-20 | 9 | remove or move to archive |
| `docs/ops` | 505 | 2026-11-10 | — | keep (current evidence); prune pre-2026-10 entries in a later PR |
| `docs/dev-notes`, `docs/evidence`, `docs/internal` | 35 | ≤2026-10-28 | — | review per file |
| `docs/specs` vs `docs/specifications` | 3 | 2026-10-26 | — | merge into `docs/specifications` |
| `spikes/*` | 17 | ≤2026-08-14 | 3 (all archival) | remove |
| `benchmarks/ready-state` | 57 | 2026-07-01 | 4 | decide (snapshot bench is opt-in only) |
| `samples/desky-mock-*` | 12 | 2026-04-28 | 3 | remove |
| `samples/ready-state-*`, `samples/native-llama` | 4 | ≤2026-06-29 | — | review |
| other `samples/*` | ~260 | 2026-09–11 | — | keep (interop gate uses `interop-datasette`) |
| `tools/legacy-vm-inspector` | 2 | 2026-10-28 | 1 | decide (workspace member) |
| `skills/*` | 20 | 2026-11 | — | keep |

Counts are from `git ls-files`; "live refs" is the number of files outside the
path that mention it (`git grep -l -F`). Redirect stubs and the 79 live references
noted in the 2026-10-28 audit are handled path by path; nothing is bulk-deleted.

## Rules for the removal PRs

- One PR per row (or small group); each PR resolves every live reference it breaks
  and passes `cargo check --workspace`, docs link checks and `dist generate --check`.
- License exceptions (CPython, WiX/Apache, Missive-derived patch, AODD/MIT) move or
  stay with their files; removal never drops a still-shipped notice.
- The `frontend-skills.agent.md` → `LICENSE.txt` provenance question is resolved
  before that file enters the 0.1.0 tree.
- Each removed path is recorded below with the commit that removed it.

## Open decisions for the owner

1. Remove `apps/desktop` and `apps/desktop-tauri` (and their workflows) from the
   0.1.0 tree, given the Desktop lives in `ato-desktop`?
2. `benchmarks/ready-state` and `tools/legacy-vm-inspector`: keep or remove?
3. `docs/aodd`: remove, or keep as reference material?

## Removal log

| Path | PR | Commit |
|---|---|---|
| — | — | — |
