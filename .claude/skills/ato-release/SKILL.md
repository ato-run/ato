---
name: ato-release
description: 'Cut a new ato-run/ato release. Merges dev into main, bumps the four dist-tracked crates in lockstep, tags from main, monitors desktop-release.yml + cargo-dist release.yml, and recovers from known failure modes (frontend dist, 403 on host, draft/published race, same-version retry). Use whenever the user says "リリースする", "devの変更をmainに入れてリリース", "cut a release", or `curl install.sh` is broken because of missing assets.'
license: MIT
---

# ato-release

Recipe for shipping a working `vX.Y.Z` from `ato-run/ato`.

## What "released correctly" means

After you finish, `curl -fsSL https://ato.run/install.sh | sh` on a clean macOS host must install `ato`, `nacelle`, **and** the Desktop bundle without a single 404. The single source of truth for that is the GitHub Release for the tag — every asset that `install.sh` may probe must live on the **published** release, not on a draft.

Expected asset count when everything succeeds: **30**.
- 11 `ato-cli-*` (4 platform tar.xz + sha256 + windows .msi + .zip + their sha256 + `ato-cli.rb`)
- 11 `nacelle-*` (same shape)
- 4 `Ato-Desktop-*` (darwin-arm64.zip, windows-x86_64.{msi,zip}, x86_64.AppImage)
- 4 cargo-dist meta (`source.tar.gz` + sha256, `sha256.sum`, `dist-manifest.json`)

## Pre-flight (do before touching anything)

1. `git switch main && git pull --ff-only`
2. `git status` clean
3. Verify the 4 dist-tracked crate versions are aligned at the *previous* release:
   ```
   grep '^version = ' \
     crates/ato-cli/Cargo.toml \
     crates/ato-desktop/Cargo.toml \
     crates/ato-desktop/xtask/Cargo.toml \
     crates/nacelle/Cargo.toml
   ```
   All four must match (e.g. all at `0.5.17`). **If `nacelle` is behind, that is the bug** — cargo-dist `precise-builds = true` silently skips packages whose version ≠ tag, and that is exactly how `nacelle` disappeared from v0.4.98 → v0.4.100 releases. Catch it up in the same bump commit.

4. Check what's new in dev:
   ```
   git log --oneline origin/main..origin/dev | head -20
   ```

## Step 1 — Merge dev into main

```sh
git fetch origin
git merge --no-commit --no-ff origin/dev
# verify auto-merge succeeded (no conflicts)
git commit -m "chore: merge origin/dev into main for vX.Y.Z release"
```

Do **not** skip this step even if `origin/dev` appears to be a fast-forward — using `--no-ff` keeps a merge commit as a clear boundary in history.

## Step 2 — Bump commit (single commit on main)

Edit **seven files**, no more, no less:

- `crates/ato-cli/Cargo.toml` — bump `version`
- `crates/ato-desktop/Cargo.toml` — bump `version`
- `crates/ato-desktop/xtask/Cargo.toml` — bump `version` (must match `ato-desktop`; xtask reads `CARGO_PKG_VERSION` to embed into installer filenames)
- `crates/nacelle/Cargo.toml` — bump `version`
- `Cargo.lock` — update two entries (`ato-cli`, `nacelle`). Edit the `version = "..."` line directly under each `name = "..."`.
- `crates/ato-desktop/Cargo.lock` — update the `ato-desktop` entry.
- `crates/ato-desktop/xtask/Cargo.lock` — update the `ato-desktop-xtask` entry.

> **Why three lock files?** `ato-desktop` and its `xtask` subcrate each have their own `[workspace]` and thus their own `Cargo.lock`. They are excluded from the root workspace. Do NOT run `cargo update`; that pulls in unrelated transitive bumps.

Sanity check after edits:
```
git diff --stat
# expected: 7 files, ~14 insertions(+), ~14 deletions(-)
grep -rn '"X.Y.Z-prev"' crates/ato-cli/Cargo.toml crates/ato-desktop/Cargo.toml \
  crates/ato-desktop/xtask/Cargo.toml crates/nacelle/Cargo.toml \
  Cargo.lock crates/ato-desktop/Cargo.lock crates/ato-desktop/xtask/Cargo.lock
# expected: zero matches
```

Commit (use the current git-configured author identity; no `Co-Authored-By:` trailer):
```
git add crates/ato-cli/Cargo.toml crates/ato-desktop/Cargo.toml \
  crates/ato-desktop/xtask/Cargo.toml crates/nacelle/Cargo.toml \
  Cargo.lock crates/ato-desktop/Cargo.lock crates/ato-desktop/xtask/Cargo.lock
git commit -m "chore(release): bump to vX.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin main
git push origin vX.Y.Z
```

The two `push` calls are deliberate. Pushing the tag separately lets you confirm `main` is healthy before you arm CI.

## What the push triggers

Two workflows run in parallel on the `vX.Y.Z` tag:

| Workflow | What it builds | Slowest job | Where it publishes |
|----------|----------------|-------------|--------------------|
| `desktop-release.yml` | 3 desktop bundles (`Ato-Desktop-*.{zip,msi,AppImage}`) | bundle Windows (~15 min) | uploads to existing release |
| `release.yml` (cargo-dist) | ato-cli + nacelle for 4 targets | `host` (waits for all builds, then `gh release create`) | **creates** the release |

Both want to land on the same `vX.Y.Z` GitHub Release. cargo-dist creates it; desktop-release uploads to it.

Monitor both workflows:
```sh
# Poll until both complete, then print results
until gh run list --limit 5 \
  --json databaseId,name,status,headBranch \
  --jq "[.[] | select(.headBranch == \"vX.Y.Z\")] | all(.status == \"completed\")" \
  2>/dev/null | grep -q true; do sleep 30; done

gh run list --limit 5 \
  --json databaseId,name,status,conclusion,headBranch \
  --jq '.[] | select(.headBranch == "vX.Y.Z") | "\(.databaseId) \(.name): \(.status) \(.conclusion // "-")"'
```

> **Monitoring caveat**: The `until` loop exits as soon as `gh run list` returns only completed runs — but GitHub may not surface the second workflow immediately. If only one run appears completed, wait 30 s and recheck with a direct `gh run view <id>` before concluding success.

Individual job progress:
```sh
gh run view <RUN_ID> --json jobs --jq '.jobs[] | "\(.name): \(.status) \(.conclusion // "-")"'
```

## Known failure modes

### Failure 1 — desktop-release: `frontend dist missing`

**Symptom**: All three `bundle *` jobs fail in `cargo run -- bundle` with:
```
thread 'main' panicked at crates/ato-desktop/build.rs:24:5:
frontend dist missing at .../crates/ato-desktop/frontend/dist
```

**Cause**: `crates/ato-desktop/build.rs` panics if `frontend/dist` is absent.

**Fix**: workflow already has the right steps on `main` (commit `8545aa7` — `ci(desktop-release): build frontend dist before bundling`). Each platform job runs Setup Node 20 → pnpm@10.15.0 → `pnpm install --frozen-lockfile && pnpm run build` in `crates/ato-desktop/frontend/` before the bundle step. If a future change reverts those steps, restore them; do **not** add `ATO_DESKTOP_SKIP_FRONTEND_BUILD=1` as a workaround — that ships an empty `host_panels` to users.

### Failure 2 — cargo-dist `host`: HTTP 403 "Resource not accessible by integration"

**Symptom**: All `build-local-artifacts` succeed; `host` job fails on `gh release create` with `HTTP 403`.

**Cause**: org-level **Workflow permissions** policy on `ato-run` is set to "read", which caps `GITHUB_TOKEN` to read regardless of what the workflow's `permissions: contents: write` block declares.

**Fix paths, in order of preference**:
1. **Org admin flips the policy** — Settings → Actions → "Read and write permissions". This is the only durable fix.
2. **Repo-level override** (only works if org allows): `gh api -X PUT repos/ato-run/ato/actions/permissions/workflow -f default_workflow_permissions=write -F can_approve_pull_request_reviews=false`. If the org policy blocks it, the API responds `409 Conflict` — escalate to org admin.
3. **Hot-patch the failed release** without changing settings:
   ```sh
   # 1) download cargo-dist's artifacts from the run
   WORKDIR=$(mktemp -d)
   gh run download <RELEASE_RUN_ID> --pattern 'artifacts-*' --dir "$WORKDIR"

   # 2) upload to whatever release already exists for the tag
   find "$WORKDIR" -type f \( \
       -name "ato-cli-*.tar.xz" -o -name "ato-cli-*.tar.xz.sha256" \
       -o -name "ato-cli-*.msi"   -o -name "ato-cli-*.msi.sha256" \
       -o -name "ato-cli-*.zip"   -o -name "ato-cli-*.zip.sha256" \
       -o -name "nacelle-*.tar.xz" -o -name "nacelle-*.tar.xz.sha256" \
       -o -name "nacelle-*.msi"   -o -name "nacelle-*.msi.sha256" \
       -o -name "nacelle-*.zip"   -o -name "nacelle-*.zip.sha256" \
       -o -name "ato-cli.rb" -o -name "nacelle.rb" \
       -o -name "source.tar.gz" -o -name "source.tar.gz.sha256" \
       -o -name "sha256.sum" \
     \) -print0 | xargs -0 gh release upload vX.Y.Z --clobber

   # 3) replace title + body with cargo-dist's announcement, un-draft
   ARTIFACTS_DIR="$WORKDIR" python3 -c '
   import json, os
   m = json.load(open(os.environ["ARTIFACTS_DIR"] + "/artifacts-dist-manifest/dist-manifest.json"))
   open("/tmp/notes.txt","w").write(m["announcement_github_body"])
   ' && gh release edit vX.Y.Z --title "vX.Y.Z" --notes-file /tmp/notes.txt --draft=false
   ```

### Failure 3 — Two releases share the same tag (the headline race)

> **Status (post-v0.4.101)**: the workflows now self-heal. `desktop-release.yml` waits up to **15 min** (30 × 30s) and falls through to creating the release under the **canonical name `vX.Y.Z`** (no "Ato Desktop" prefix, no `--draft`). `release.yml`'s `Create GitHub Release` step is **idempotent** — if a release for the tag already exists, it `gh release edit`s the title/notes and `gh release upload --clobber`s its artifacts instead of `gh release create`. So in normal operation only one release object exists. This section stays as a recovery cookbook in case both safeguards somehow fail.

Confirm with:
```sh
gh api repos/ato-run/ato/releases \
  --jq '.[] | select(.tag_name=="vX.Y.Z") | "id=\(.id) name=\(.name) draft=\(.draft) assets=\(.assets | length)"'
```

**Recovery (consolidate into the published release)**:
```sh
DRAFT_ID=$(gh api repos/ato-run/ato/releases --jq '.[] | select(.tag_name=="vX.Y.Z" and .draft==true) | .id')
PUB_TAG=vX.Y.Z

mkdir -p /tmp/vX.Y.Z-desktop && cd /tmp/vX.Y.Z-desktop
gh api repos/ato-run/ato/releases/$DRAFT_ID --jq '.assets[] | "\(.id) \(.name)"' \
  | while read id name; do
      gh api -H "Accept: application/octet-stream" \
             repos/ato-run/ato/releases/assets/$id > "$name"
    done

gh release upload "$PUB_TAG" Ato-Desktop-* --clobber
gh api -X DELETE repos/ato-run/ato/releases/$DRAFT_ID
```

### Failure 4 — Workflow fails; same version retry (no version bump)

Use this when a CI workflow fails and you want to retry **without** advancing the version number (e.g., a transient runner failure, a flaky Windows build, a network timeout).

**Step 1 — Try re-running failed jobs first (cheapest)**:
```sh
# Re-run only the failed jobs of a run
gh run rerun <RUN_ID> --failed

# Watch until it finishes
until [ "$(gh run view <RUN_ID> --json status --jq '.status')" = "completed" ]; do sleep 30; done
gh run view <RUN_ID> --json status,conclusion --jq '"\(.status) \(.conclusion)"'
```

**Step 2 — If the release object is corrupt or the tag itself needs to be re-pointed**:

> Only do this if `gh run rerun` won't fix it (e.g., the bump commit itself had a bug).

```sh
# 1. Delete the remote tag (leaves the GitHub Release orphaned temporarily)
git push --delete origin vX.Y.Z

# 2. Delete the GitHub Release object (if it exists and is wrong)
gh release delete vX.Y.Z --yes

# 3. Fix whatever was wrong (e.g., edit a workflow file and push to main)
#    Do NOT change the version numbers — they are already correct.

# 4. Re-tag the tip of main (or the corrected commit)
git tag -d vX.Y.Z                    # delete local tag
git tag -a vX.Y.Z -m "vX.Y.Z"       # re-create pointing at HEAD
git push origin vX.Y.Z               # re-arm CI
```

> **Hard constraint**: only delete + re-push a tag if the release has not yet been publicly announced / is still a draft / has zero assets. Once users have downloaded assets under a tag, deleting the tag breaks their checksums and install cache. In that case always advance to vX.Y.(Z+1) instead.

## Verification (do this every time)

1. **Asset count is 30** (or document why not):
   ```sh
   gh release view vX.Y.Z --json assets --jq '.assets | length'
   ```
2. **No draft sibling**:
   ```sh
   gh api repos/ato-run/ato/releases --jq '.[] | select(.tag_name=="vX.Y.Z") | .draft' \
     | sort -u   # expected: only "false"
   ```
3. **Smoke test installer end-to-end** on a clean machine:
   ```sh
   curl -fsSL https://ato.run/install.sh | sh
   # expect: ato + nacelle + Ato Desktop installed, no 404 lines
   ```
4. **Homebrew tap updated**: `brew install ato-run/ato/ato-cli` should resolve to the new version. `publish-homebrew-formula` runs as part of cargo-dist `release.yml` and auto-PRs the tap repo (`ato-run/homebrew-ato`).

## Hard rules

- **All four crates bump together.** ato-cli, ato-desktop, ato-desktop-xtask, nacelle. Same version. No exceptions. cargo-dist will silently drop any laggard.
- **All three Cargo.lock files update together.** Root `Cargo.lock`, `crates/ato-desktop/Cargo.lock`, `crates/ato-desktop/xtask/Cargo.lock`.
- **Release commit goes on main, not a feature branch.** push:tag triggers two workflows that pin themselves to the tagged commit; they cannot see commits on side branches.
- **Never amend after tagging.** The tag will silently keep pointing at the old commit; the workflows will run on stale code.
- **Do not hardcode `Koh0920` or any other author.** Use the currently authenticated `gh` user for GitHub operations, and use the current repository/global `git config user.name` and `git config user.email` for the release commit. **No `Co-Authored-By:` trailers** in the release commit.
- **Don't `cargo update` to refresh the lockfile.** Edit the four entries by hand. `cargo update` pulls in unrelated dependency bumps that have no business riding inside a release commit.
- **Don't push a release commit if `crates/nacelle/Cargo.toml` lags `crates/ato-cli/Cargo.toml`.** That single check would have prevented the v0.4.98–v0.4.100 install.sh outage.
- **Same-version tag re-push is safe only on unreleased drafts.** Once assets are public, bump to the next patch instead.

## Quick reference

```sh
# 1. merge dev
git switch main && git pull --ff-only
git fetch origin
git merge --no-commit --no-ff origin/dev
git commit -m "chore: merge origin/dev into main for vX.Y.Z release"

# 2. bump (7 files)
# Edit crates/ato-cli/Cargo.toml, crates/ato-desktop/Cargo.toml,
#       crates/ato-desktop/xtask/Cargo.toml, crates/nacelle/Cargo.toml,
#       Cargo.lock, crates/ato-desktop/Cargo.lock, crates/ato-desktop/xtask/Cargo.lock
git add crates/ato-cli/Cargo.toml crates/ato-desktop/Cargo.toml \
  crates/ato-desktop/xtask/Cargo.toml crates/nacelle/Cargo.toml \
  Cargo.lock crates/ato-desktop/Cargo.lock crates/ato-desktop/xtask/Cargo.lock
git commit -m "chore(release): bump to vX.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin main
git push origin vX.Y.Z

# 3. monitor
until gh run list --limit 5 \
  --json databaseId,name,status,headBranch \
  --jq "[.[] | select(.headBranch == \"vX.Y.Z\")] | all(.status == \"completed\")" \
  2>/dev/null | grep -q true; do sleep 30; done
gh run list --limit 5 --json databaseId,name,status,conclusion,headBranch \
  --jq '.[] | select(.headBranch == "vX.Y.Z") | "\(.databaseId) \(.name): \(.status) \(.conclusion // "-")"'

# 4. retry failed jobs (same version, no bump)
gh run rerun <RUN_ID> --failed

# 5. verify
gh release view vX.Y.Z --json assets --jq '.assets | length'  # expect 30
gh api repos/ato-run/ato/releases \
  --jq '[.[] | select(.tag_name=="vX.Y.Z")] | length'          # expect 1
```
