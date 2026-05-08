# CLAUDE.md

Repo-wide guidance for Claude Code (and humans). Crate-specific notes live in
each crate's own `CLAUDE.md` (currently: `crates/ato-desktop/CLAUDE.md`).

## Branching model

```
feature branch ──PR──▶ dev ──tag(vX.Y.Z)──▶ main
```

- **`main`** — release-only. Every commit on main corresponds to a published
  release tag (`vX.Y.Z`). Never push directly. The `release.yml` and
  `desktop-release.yml` workflows fire on `v*` tags pushed from main.
- **`dev`** — integration branch. Feature PRs merge here first. Local /
  integration verification runs against `dev` HEAD before promotion.
- **feature branches** — short-lived, one issue or one tightly-scoped change
  per branch. Naming: `<type>/<slug>` (e.g. `fix/desktop-stop-active-session`,
  `feat/v060-orchestration-consent-aggregation`). Open PRs against `dev`.

### Promoting `dev` → `main` (cutting a release)

1. Verify `dev` end-to-end locally (the AODD receipt for the release theme is
   the gate — see `.claude/skills/aodd/SKILL.md`).
2. Bump versions in the four release crates: `ato-cli`, `ato-desktop`,
   `nacelle`, `ato-desktop-xtask` (xtask must track ato-desktop because
   `env!("CARGO_PKG_VERSION")` is embedded into bundle filenames).
3. `cargo update -p <crate> --offline` for each (refreshes the two Cargo.lock
   files — root + `crates/ato-desktop/Cargo.lock` + `crates/ato-desktop/xtask/Cargo.lock`).
4. Commit `chore(release): bump to X.Y.Z` on `dev`.
5. Fast-forward `main` from `dev`, push, then `git tag -a vX.Y.Z` and push the
   tag. The release workflows handle artifact publishing.

### Branch hygiene

- After a PR merges to `dev`, delete the feature branch (local + remote).
- `main` and `dev` are the only long-lived branches. Anything else that
  outlives its PR is a smell.

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
- **Base from `dev`**, not `main`: feature work integrates against `dev`.
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
ato-cli + nacelle) and `desktop-release.yml` (xtask, builds ato-desktop
bundles). Both upload to the same GitHub release. The desktop workflow
waits up to 15 min for cargo-dist's `host` job to create the release object,
then falls through to creating it itself — so the order of the two
workflows finishing doesn't matter.

Desktop bundle filenames embed `env!("CARGO_PKG_VERSION")` from the
`ato-desktop-xtask` crate. **Always bump xtask's version together with
ato-desktop's** or the bundle filenames will lag the actual release version.

## Memory and skills

- `.claude/skills/aodd/SKILL.md` — Agent Operation Driven Development. The
  release gate for desktop-touching features is an AODD receipt with
  `result: complete`.
- Auto-memory at `~/.claude/projects/.../memory/MEMORY.md` is the source of
  truth for user preferences (commit authorship, "no lying UI" policy,
  v0.5.0 scope decisions). Read it before making release-process decisions.
