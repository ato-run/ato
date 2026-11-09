# CLAUDE.md

Repo-wide guidance for Claude Code (and humans). Crate-specific notes live in
each crate's own `CLAUDE.md` (currently: `apps/desktop/CLAUDE.md`).

## Commit author identity

Use a plain email address only for `user.email` (e.g. `kohsuke20010920@gmail.com`).
Do **not** use GitHub's `<numeric-id>+username@users.noreply.github.com` noreply
format. GitHub attributes commits by the numeric ID embedded in that address, not
by the username string — if the ID is stale or wrong, GitHub silently shows the
commit as authored by whatever account currently owns that ID (this happened:
commits showed as `Koh0920` locally but were attributed to an unrelated GitHub
account, `maicon-sinapsis`, on github.com). Verify with `git config user.email`
(local and `--global`) before committing.

## Branching model

Every change except an exceptional hotfix follows one release path:

```
feat/* | fix/* ──PR──▶ dev ──PR──▶ stg ──release PR──▶ main
                         │
                         └─ nightly = dev + feature-flagged work (rebased onto dev)
feat/* (feature flag only) ──PR──▶ nightly
```

| Branch | Role | Rules |
|--------|------|-------|
| `dev` | Integration of all new work. Default PR base. | PR only, merge commits, no force push / deletion. |
| `stg` | Staging candidate; shared Staging E2E. Updated by `dev → stg` PRs. | PR only, merge commits, no force push / deletion. |
| `main` | Release-approved code. Updated only by `stg → main` release PRs. Merging ≠ production deploy. | PR only, merge commits, no force push / deletion. |
| `nightly` | `dev` plus work that ships behind a feature flag. | Rebased onto `dev` continuously with `--force-with-lease`; never deleted. Nothing flows from `nightly` to `stg`/`main`; flagged work graduates by a normal PR to `dev`. |

- Base new work on `origin/dev` and open the PR against `dev`. Only work that is
  gated behind a feature flag may target `nightly` instead.
- Hotfix (exceptional only): `hotfix/*` from `main` → PR to `main`, then
  immediately back-merge `main` into `stg` and `dev` (nightly picks it up on its
  next rebase). Record why the normal path was skipped in the PR.
- After a PR merges, delete its branch. Long-lived branches are exactly
  `main`, `stg`, `dev`, `nightly`.

### Release promotion

1. `dev → stg` PR; verify on shared Staging.
2. Bump versions in the release crates (`cli`, `desktop`, `nacelle`,
   `ato-desktop-xtask` — xtask tracks desktop because bundle filenames embed
   `CARGO_PKG_VERSION`), refresh Cargo.lock files, commit on `dev`, promote.
3. `stg → main` release PR. After merge, tag `vX.Y.Z` on `main` and push the tag
   (`release.yml` / `desktop-release.yml` publish artifacts).

## Parallel development with git worktrees

When working on multiple issues at once — or when an agent needs an isolated
checkout for an experiment without trampling the primary working tree — use
`git worktree`. The `.worktrees/` directory at the repo root is reserved for
this and is gitignored (see `.gitignore`).

### Conventions

- **Location**: `.worktrees/<branch-name>` at the repo root. Keep all
  worktrees inside `.worktrees/` so they share a single ignore rule and a
  single cleanup root.
- **Branch per worktree**: each worktree gets its own feature branch. Never
  share a branch between two worktrees (git enforces this anyway, but make
  it explicit).
- **Base from `dev`**: feature work integrates against `dev` (flag-gated work may use `nightly`).
  ```sh
  git fetch origin
  git worktree add -b fix/issue-NNN-slug .worktrees/issue-NNN origin/dev
  ```
- **One issue per worktree**: name the directory after the issue number when
  there is one (`.worktrees/issue-NNN`); otherwise after the branch slug.
- **Never put a worktree inside another worktree** — `target/`, `node_modules/`,
  and `.ato/` are large and host-local; sharing them across worktrees via
  symlinks is fine but nesting is not.

### Lifecycle

```sh
# Start work on issue NNN
git fetch origin
git worktree add -b fix/issue-NNN-slug .worktrees/issue-NNN origin/dev

# Work in the worktree
cd .worktrees/issue-NNN
# ... edit, build, test ...

# Open PR against dev
git push -u origin fix/issue-NNN-slug
gh pr create --base dev --title "..." --body "..."

# After PR merges:
cd <repo-root>
git worktree remove .worktrees/issue-NNN
git branch -d fix/issue-NNN-slug                   # local
git push --delete origin fix/issue-NNN-slug        # remote
```

### Build state across worktrees

- Cargo's `target/` is per-worktree by default (each worktree has its own
  `target/`). That's expensive but safe; do not symlink `target/` across
  worktrees — it causes incremental-cache corruption.
- `node_modules/` similarly stays per-worktree.
- `.ato/` (runtime state) is *always* per-worktree — different worktrees may
  pin different ato versions; sharing leaks state.

### When NOT to use a worktree

- For a quick read-only inspection of another branch — just `git stash &&
  git checkout` is faster.
- For tasks that need to run a release-build (`cargo run --release`) of the
  primary working tree's code — those should run in the primary checkout to
  reuse the warm `target/` cache.

## Release artifacts

`v*` tags fire two workflows in parallel: `release.yml` (cargo-dist, builds
cli + nacelle) and `desktop-release.yml` (xtask, builds ato-desktop
bundles). Both upload to the same GitHub release. The desktop workflow
waits up to 15 min for cargo-dist's `host` job to create the release object,
then falls through to creating it itself — so the order of the two
workflows finishing doesn't matter.

Desktop bundle filenames embed `env!("CARGO_PKG_VERSION")` from the
`ato-desktop-xtask` crate. **Always bump xtask's version together with the
ato-desktop bundle's** or the bundle filenames will lag the actual release version.

## Memory and skills

- `.claude/skills/aodd/SKILL.md` — Agent Operation Driven Development. The
  release gate for desktop-touching features is an AODD receipt with
  `result: complete`.
- Auto-memory at `~/.claude/projects/.../memory/MEMORY.md` is the source of
  truth for user preferences (commit authorship, "no lying UI" policy,
  v0.5.0 scope decisions). Read it before making release-process decisions.
