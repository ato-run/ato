# CLAUDE.md

Repo-wide guidance for Claude Code (and humans). Crate-specific notes live in
each crate's own `CLAUDE.md` (currently: `apps/desktop/CLAUDE.md`).

## Branching model

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

```
feat/*, fix/*, hotfix/*
```

Short-lived, one issue or one tightly-scoped change per branch.
Examples: `fix/0.6-desktop-stop-active-session`, `feat/0.7-orchestration-consent`.

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

```
release/0.5 → release/0.6 → dev → nightly
```

Never backport from `nightly` to `release/*`. 0.7 API / placement / session
changes must not bleed into maintenance branches.

### Choosing the base branch for a worktree

| Work type | Base branch |
|-----------|-------------|
| 0.7 new feature / experiment | `nightly` |
| 0.6.x patch or regression fix | `release/0.6` |
| 0.5.x critical / security fix | `release/0.5` |
| Normal dev (non-version-specific) | `dev` |
| Urgent hotfix from stable | `main` |

```sh
# Example: 0.6 patch
git fetch origin
git worktree add -b fix/0.6-issue-NNN-slug .worktrees/issue-NNN origin/release/0.6

# Example: 0.7 feature
git fetch origin
git worktree add -b feat/0.7-issue-NNN-slug .worktrees/issue-NNN origin/nightly
```

### Versioning

| Branch | Version scheme |
|--------|----------------|
| `release/0.5` | `v0.5.x` |
| `release/0.6` | `v0.6.x` |
| `nightly` | `v0.7.0-nightly.YYYYMMDD+sha` |
| `dev` | `v0.7.0-dev` (unreleased) |
| `main` | stable tags only (`vX.Y.Z`) |

### Branch protection rules

| Branch | Policy |
|--------|--------|
| `main` | No direct push. Release PR only. Full CI + manual smoke required. |
| `release/0.6` | No direct push. Patch / security / regression fix only. No new features. |
| `release/0.5` | No direct push. Critical / security fix only. Short-lived or limited support. |
| `dev` | Normal integration. CI required. |
| `nightly` | 0.7 experimental. Compile + test required. AODD may be degraded — log reason. |

### Promoting to a release

#### Patch release from `release/0.6` (or `release/0.5`)

1. Verify the maintenance branch end-to-end.
2. Bump versions in the four release crates: `cli`, `desktop`,
   `nacelle`, `ato-desktop-xtask` (xtask must track desktop because
   `env!("CARGO_PKG_VERSION")` is embedded into bundle filenames).
3. `cargo update -p <crate> --offline` for each (refreshes Cargo.lock files).
4. Commit `chore(release): bump to X.Y.Z` on the maintenance branch.
5. Open a release PR from `release/0.6` → `main`. Merge, then tag:
   ```sh
   git tag -a vX.Y.Z && git push origin vX.Y.Z
   ```
6. Forward-port the bump commit to `dev` (and `nightly` if relevant).

#### Promoting `dev` → `main` (0.7 stable / major release)

1. Verify `dev` end-to-end (AODD receipt with `result: complete` is the gate —
   see `.claude/skills/aodd/SKILL.md`).
2. Bump versions in the four release crates and refresh Cargo.lock files.
3. Commit `chore(release): bump to X.Y.Z` on `dev`.
4. Fast-forward `main` from `dev`, push, then `git tag -a vX.Y.Z` and push the
   tag. The `release.yml` and `desktop-release.yml` workflows handle artifact
   publishing.
5. Create `release/0.7` from the tagged commit; `nightly` advances to 0.8 work.

#### 0.7 graduation options

```
Lightweight (no RC):   nightly → dev → main
With RC period:        nightly → dev → release/0.7 → main
```

### Branch hygiene

- After a PR merges, delete the feature branch (local + remote).
- Long-lived branches: `main`, `dev`, `nightly`, `release/0.6`, `release/0.5`
  (and future `release/0.7`). Any other branch that outlives its PR is a smell.

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
