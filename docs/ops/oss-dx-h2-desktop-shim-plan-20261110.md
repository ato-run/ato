# H2 part 2: isolating the legacy `apps/desktop` shim (plan, 2026-11-10)

Status: plan only. The archive move (part 1) is blocked on creating the public
archive repository; this part records what isolating the shim needs so the two
can be reviewed together when that repo exists.

## What `apps/desktop` is now

A three-file launcher shim over the `ato` CLI (`src/lib.rs` maps the five
`ComputationCommand` variants, `src/main.rs` parses `--version`/`--run`,
`src/bin/ato_desktop_mcp.rs` is an MCP entry). Its README says the Desktop
product is `ato-run/ato-desktop`. The shim shares `lib/ipc` with the Tauri shell
in `apps/desktop-tauri`.

## References that must change with the move

| Reference | What it does today | Change with the isolation |
|---|---|---|
| `.github/workflows/desktop-release.yml` | On `v*` and `ato-desktop-v*`: builds `apps/desktop/xtask`, bundles `apps/desktop/assets/system/*`, uploads dmg/msi/zip/AppImage | Move this workflow (and the `xtask`) to `ato-run/ato-desktop`, or delete it from `ato` if the Tauri repo owns Desktop releases |
| `.github/workflows/desktop-tauri-ci.yml` | Builds the Tauri shell and a "legacy apps/desktop shim compiles" job | Drop the shim job; keep the Tauri job if that crate stays in this repo |
| `.github/workflows/release-dry-run.yml` | `cargo test --manifest-path apps/desktop/xtask/Cargo.toml` | Remove the step together with `xtask` |
| `Cargo.toml` workspace members | Includes `apps/desktop` (and the exclusion notes) | Remove the member after the move |
| `AGENTS.md`, `CLAUDE.md`, `.github/copilot-instructions.md` | Describe `apps/desktop` as the current Desktop | Point at `ato-run/ato-desktop` |
| `docs/desktop.md`, `docs/guides/README.md`, `docs/internal/testing-isolation.md` | Reference the shim paths | Update links |
| `apps/desktop-tauri/Cargo.toml` | Path dependency on the shim/`lib/ipc` | Keep `lib/ipc`; drop the shim dependency |
| `docs/archive/...` inventories | Historical references | Leave historical references in the archive; they are not live links |

`apps/desktop/README.md` itself already explains the situation; the move should
carry that text.

## Sequence (when the owner approves the archive repo)

1. Create the archive repo and move `docs/archive` (part 1, manifest already
   tooled by `scripts/archive_manifest.py`).
2. Move `apps/desktop` (source, `xtask`, `assets/system`) to the archive repo or
   to `ato-run/ato-desktop`; decide which repository is the long-term home.
3. In one `ato` change: remove the workspace member, delete the three
   workflows' shim steps (or the workflows themselves), and update the three
   instruction files plus the docs list. Run `cargo check --workspace` and the
   docs audit.
4. Add a `docs/ops` note recording the new home and the commit SHA of the moved
   tree, pointing at the archive manifest.

## Decision points for review

- Does Desktop release (dmg/msi/zip) stay in `ato` (Tauri repo publishes) or move
  entirely to `ato-run/ato-desktop`?
- Is the shim archived (read-only) or deleted after `ato-run/ato-desktop` takes
  over the launcher path?
- Keep `lib/ipc` in `ato` (the Tauri shell uses it), or also move it?

## Proposed edit checklist (for the approved change)

1. `git rm -r apps/desktop` after copying `src/`, `xtask/`, `assets/system/`
   into the destination repository; keep `apps/desktop/README.md` text in the
   destination README (it explains the shim).
2. Root `Cargo.toml`: remove the `apps/desktop` workspace member (and the
   exclusion comment that names it).
3. `desktop-release.yml`: delete the `bundle-macos` / `bundle-windows` /
   `bundle-linux` jobs and the `ato-desktop-v*` trigger, or move the whole
   workflow to `ato-run/ato-desktop` with its `xtask`.
4. `desktop-tauri-ci.yml`: drop the `legacy apps/desktop shim compiles` job;
   keep the Tauri jobs and their `lib/ipc` dependency.
5. `release-dry-run.yml`: remove the
   `cargo test --manifest-path apps/desktop/xtask/Cargo.toml` step.
6. `AGENTS.md`, `CLAUDE.md`, `.github/copilot-instructions.md`, `docs/desktop.md`,
   `docs/guides/README.md`, `docs/internal/testing-isolation.md`: repoint to
   `ato-run/ato-desktop`; historical `docs/archive` references stay.
7. `apps/desktop-tauri/Cargo.toml`: remove the shim dependency, keep `lib/ipc`.
8. Validate: `cargo check --workspace`, `cargo run -p arch-check`,
   `python3 scripts/docs-audit.py …` (`unexpected_failures: 0`), and grep that
   no live reference to `apps/desktop` remains outside `docs/archive` and the
   destination-repository note.
9. Record the destination repository and the moved tree's commit SHA in a
   `docs/ops` note, next to the archive manifest's `origin_commit`.
